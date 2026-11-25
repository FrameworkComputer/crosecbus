#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Cycle the system through Modern Standby and/or Hibernate and check that
    crosecbus survives, capturing logs.

.DESCRIPTION
    1. Runs smoke-test.ps1 (device state, IOCTL probes, framework_tool).
    2. For each mode, runs pwrtest from the WDK, which enters the state and
       wakes the system on a timer, then runs smoke-test.ps1 again.
         standby:   pwrtest /cs     (Modern Standby / S0 low power idle).
                    The EC device stays in D0; this exercises the ISR and
                    EC state across S0ix.
         hibernate: pwrtest /sleep /s:4. The device goes to D3 and back,
                    so EvtDeviceD0Entry runs again on resume.
    3. Collects pwrtest logs, System event log entries since the start
       (summary + .evtx), Kernel-Power transition counts, and a sleep study.

    Results go to target\power-test\<timestamp>\ with summary.json; the
    script exits non-zero if any check failed. The machine really sleeps
    and hibernates; save your work first. Wake timers must be allowed in
    the active power plan.

.PARAMETER Mode
    standby, hibernate or both.

.PARAMETER Cycles
    Cycles per mode.

.PARAMETER SleepSeconds
    Time spent in the low power state per cycle.

.PARAMETER WakeSeconds
    Time awake between cycles.

.PARAMETER Baseline
    smoke-test.ps1 output folder to diff framework_tool --versions against.

.EXAMPLE
    .\scripts\test-power.ps1 -Mode standby -Cycles 3
    .\scripts\test-power.ps1 -Mode both -Baseline target\smoke-test\c-driver
#>
param(
    [ValidateSet('standby', 'hibernate', 'both')]
    [string]$Mode = 'both',
    [int]$Cycles = 3,
    [int]$SleepSeconds = 60,
    [int]$WakeSeconds = 60,
    [string]$Baseline,
    [string]$OutDir
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
$start = Get-Date
if (-not $OutDir) {
    $OutDir = Join-Path $root ("target\power-test\{0:yyyyMMdd-HHmmss}" -f $start)
}
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

$pwrtest = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\Tools\*\x64\pwrtest.exe' -ErrorAction SilentlyContinue |
    Sort-Object FullName | Select-Object -Last 1 -ExpandProperty FullName
if (-not $pwrtest) { throw 'pwrtest.exe not found (install the WDK)' }

$checks = New-Object System.Collections.Generic.List[object]
function Add-Check([string]$Name, [bool]$Passed, [string]$Detail) {
    $checks.Add([pscustomobject]@{ Name = $Name; Passed = $Passed; Detail = $Detail })
    $mark = if ($Passed) { 'PASS' } else { 'FAIL' }
    Write-Host ("[{0}] {1}: {2}" -f $mark, $Name, $Detail)
}

function Invoke-SmokeTest([string]$Stage) {
    Write-Host "`n=== Smoke test: $Stage"
    $smokeArgs = @{ OutDir = (Join-Path $OutDir $Stage) }
    if ($Baseline) { $smokeArgs.Baseline = $Baseline }
    $r = & "$PSScriptRoot\smoke-test.ps1" @smokeArgs
    Add-Check "Smoke test ($Stage)" $r.Passed (($r.Checks | Where-Object { -not $_.Passed } | ForEach-Object Name) -join ', ')
}

function Get-PowerEventCount([int]$Id, [string]$Provider, [datetime]$Since) {
    @(Get-WinEvent -FilterHashtable @{ LogName = 'System'; ProviderName = $Provider; Id = $Id; StartTime = $Since } -ErrorAction SilentlyContinue).Count
}

powercfg /a | Out-File -Encoding utf8 "$OutDir\powercfg-a.txt"
Invoke-SmokeTest 'pre'

$modes = if ($Mode -eq 'both') { @('standby', 'hibernate') } else { @($Mode) }
foreach ($m in $modes) {
    $dir = Join-Path $OutDir $m
    New-Item -ItemType Directory -Force $dir | Out-Null
    $modeStart = Get-Date

    if ($m -eq 'standby') {
        $pwrArgs = @('/cs', "/c:$Cycles", "/d:$WakeSeconds", "/p:$SleepSeconds")
    } else {
        $pwrArgs = @('/sleep', '/s:4', "/c:$Cycles", "/d:$WakeSeconds", "/p:$SleepSeconds")
    }
    Write-Host "`n=== $m`: pwrtest $($pwrArgs -join ' ')"
    # pwrtest writes pwrtestlog.* to the working directory.
    Push-Location $dir
    try {
        $ErrorActionPreference = 'Continue'
        & $pwrtest @pwrArgs 2>&1 | ForEach-Object { "$_" } | Tee-Object -FilePath "$dir\pwrtest-console.txt" | Out-Null
        $code = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
    } finally {
        Pop-Location
    }
    $detail = 'exit code 0x{0:X8}' -f $code
    if (Select-String -Quiet -Path "$dir\pwrtest-console.txt" -Pattern 'Virtual power button driver not found') {
        # pwrtest /cs enters Modern Standby through WDTF's virtual power
        # button; /sleep (hibernate) doesn't need it.
        $msi = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\Testing\Runtimes\WDTF_Desktop_Kit_Product-x64_*.msi' -ErrorAction SilentlyContinue |
            Select-Object -First 1 -ExpandProperty FullName
        $detail = "WDTF not installed (needed for the virtual power button). Install: msiexec /i `"$msi`""
    }
    Add-Check "pwrtest $m" ($code -eq 0) $detail

    # Confirm the system actually made the transitions.
    if ($m -eq 'standby') {
        $n = Get-PowerEventCount 506 'Microsoft-Windows-Kernel-Power' $modeStart
        Add-Check 'Entered Modern Standby' ($n -ge $Cycles) "$n x Kernel-Power 506 (expected $Cycles)"
    } else {
        $n = Get-PowerEventCount 1 'Microsoft-Windows-Power-Troubleshooter' $modeStart
        Add-Check 'Resumed from Hibernate' ($n -ge $Cycles) "$n x Power-Troubleshooter 1 (expected $Cycles)"
    }

    Invoke-SmokeTest "after-$m"
}

# --- Logs ---------------------------------------------------------------------
Write-Host "`n=== Collecting logs"
$events = Get-WinEvent -FilterHashtable @{ LogName = 'System'; StartTime = $start } -ErrorAction SilentlyContinue
$events | Sort-Object TimeCreated | ForEach-Object {
    $msg = if ($_.Message) { ($_.Message -split "`r?`n")[0] } else { '' }
    '{0:HH:mm:ss.fff} {1,-5} {2,-45} {3,5} {4}' -f $_.TimeCreated, $_.LevelDisplayName, $_.ProviderName, $_.Id, $msg
} | Out-File -Encoding utf8 "$OutDir\system-events.txt"

$relevant = $events | Where-Object {
    $_.Message -match 'CrosEcBus|crosecbus|FRMWC004' -or
    ($_.Level -in 1, 2 -and $_.ProviderName -match 'Kernel-PnP|Kernel-Power|DriverFrameworks|WDF')
}
$relevant | Sort-Object TimeCreated | Format-List TimeCreated, ProviderName, Id, LevelDisplayName, Message |
    Out-File -Encoding utf8 "$OutDir\driver-events.txt"
Add-Check 'No driver-related errors in System log' (-not $relevant) "$(@($relevant).Count) events (see driver-events.txt)"

$since = $start.ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ss.fffZ')
wevtutil epl System "$OutDir\System.evtx" "/q:*[System[TimeCreated[@SystemTime>='$since']]]" 2>&1 | Out-Null

if ($modes -contains 'standby') {
    powercfg /sleepstudy /output "$OutDir\sleepstudy.html" /duration 1 2>&1 | Out-Null
}

$passed = -not ($checks | Where-Object { -not $_.Passed })
$summary = [pscustomobject]@{
    Start = $start.ToString('o'); End = (Get-Date).ToString('o'); OutDir = $OutDir
    Mode = $Mode; Cycles = $Cycles; Passed = $passed; Checks = $checks
}
$summary | ConvertTo-Json -Depth 4 | Out-File -Encoding utf8 "$OutDir\summary.json"

Write-Host "`n=== $(if ($passed) { 'PASSED' } else { 'FAILED' }) - results in $OutDir"
$checks | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
if (-not $passed) { exit 1 }
