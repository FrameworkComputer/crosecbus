#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Quick health check of the installed crosecbus driver.

.DESCRIPTION
    Records the device's status, driver and power data, then talks to the EC
    through \Device\CrosEC:
      * IOCTL_CROSEC_RDMEM: reads the "EC" signature from the memory map.
      * IOCTL_CROSEC_XCMD:  EC_CMD_HELLO, which must return input + 0x01020304.
      * framework_tool --versions / --power, if framework_tool is on PATH.
    Each EC access runs in a background job with a timeout, so a hung driver
    shows up as a failed check instead of hanging the script.

    Works the same against Framework's C driver, so it can record a baseline.

.PARAMETER OutDir
    Folder for the results. Defaults to target\smoke-test\<timestamp>.

.PARAMETER Baseline
    Folder from an earlier run (e.g. with the C driver). Its
    framework-versions.txt is diffed against this run's.

.PARAMETER TimeoutSeconds
    Per-check timeout.

.OUTPUTS
    An object with Passed (bool) and Checks (name, passed, detail). Also
    written to summary.json in OutDir.

.EXAMPLE
    .\scripts\smoke-test.ps1 -OutDir baseline-c-driver
    .\scripts\smoke-test.ps1 -Baseline baseline-c-driver
#>
param(
    [string]$OutDir,
    [string]$Baseline,
    [int]$TimeoutSeconds = 30
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
if (-not $OutDir) {
    $OutDir = Join-Path $root ("target\smoke-test\{0:yyyyMMdd-HHmmss}" -f (Get-Date))
}
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

$hwid = 'ACPI\FRMWC004'
$checks = New-Object System.Collections.Generic.List[object]
function Add-Check([string]$Name, [bool]$Passed, [string]$Detail) {
    $checks.Add([pscustomobject]@{ Name = $Name; Passed = $Passed; Detail = $Detail })
    $mark = if ($Passed) { 'PASS' } else { 'FAIL' }
    Write-Host ("[{0}] {1}: {2}" -f $mark, $Name, $Detail)
}

# Runs $Block in a job; returns its output, or $null on timeout.
function Invoke-WithTimeout([scriptblock]$Block, [object[]]$ArgumentList) {
    $job = Start-Job -ScriptBlock $Block -ArgumentList $ArgumentList
    if (Wait-Job $job -Timeout $TimeoutSeconds) {
        $out = Receive-Job $job 2>&1 | ForEach-Object { "$_" }
        Remove-Job $job -Force
        return , @($out)
    }
    # A job stuck in a kernel wait can't be stopped cleanly; leave it.
    return $null
}

# --- Device state -----------------------------------------------------------
$dev = Get-PnpDevice -PresentOnly | Where-Object { $_.HardwareID -contains $hwid } | Select-Object -First 1
if (-not $dev) {
    Add-Check 'Device present' $false "no present device with $hwid"
} else {
    $id = $dev.InstanceId
    pnputil /enum-devices /instanceid $id /drivers | Out-File -Encoding utf8 "$OutDir\device.txt"
    $prop = { param($k) (Get-PnpDeviceProperty -InstanceId $id -KeyName $k -ErrorAction SilentlyContinue).Data }
    $ver = & $prop 'DEVPKEY_Device_DriverVersion'
    $inf = & $prop 'DEVPKEY_Device_DriverInfPath'
    Add-Check 'Device started' ($dev.Status -eq 'OK') ("status={0} problem={1} driver={2} {3}" -f $dev.Status, $dev.Problem, $inf, $ver)

    $pd = & $prop 'DEVPKEY_Device_PowerData'
    if ($pd) {
        $dstate = [BitConverter]::ToUInt32($pd, 4)
        # PD_PowerStateMapping[Unspecified, S0..S5]; DEVICE_POWER_STATE 0 is
        # unspecified, 1..4 are D0..D3.
        $map = (1..6 | ForEach-Object {
                $dx = [BitConverter]::ToUInt32($pd, 24 + 4 * $_)
                'S{0}={1}' -f ($_ - 1), $(if ($dx) { "D$($dx - 1)" } else { '-' })
            }) -join ' '
        "MostRecentPowerState=D$($dstate - 1) Capabilities=0x$('{0:X}' -f [BitConverter]::ToUInt32($pd, 8)) $map" |
            Out-File -Encoding utf8 "$OutDir\power-data.txt"
        Add-Check 'Device in D0' ($dstate -eq 1) "most recent power state D$($dstate - 1)"
    }
}

# --- Direct IOCTLs ------------------------------------------------------------
$ioctl = {
    param($Op)
    Add-Type -Namespace SmokeTest -Name K32 -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern Microsoft.Win32.SafeHandles.SafeFileHandle CreateFileW(
    string name, uint access, uint share, IntPtr sa, uint disp, uint flags, IntPtr tmpl);
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool DeviceIoControl(Microsoft.Win32.SafeHandles.SafeFileHandle h, uint code,
    byte[] inBuf, uint inLen, byte[] outBuf, uint outLen, out uint returned, IntPtr ov);
'@
    $GENERIC_READ_WRITE = [uint32]3221225472   # 0xC0000000
    $IOCTL_CROSEC_RDMEM = [uint32]2162974728   # 0x80EC6008
    $IOCTL_CROSEC_XCMD = [uint32]2163007492    # 0x80ECE004
    $h = [SmokeTest.K32]::CreateFileW('\\.\GLOBALROOT\Device\CrosEC', $GENERIC_READ_WRITE, 3, [IntPtr]::Zero, 3, 0, [IntPtr]::Zero)
    if ($h.IsInvalid) { return "ERROR open failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())" }
    try {
        $ret = [uint32]0
        if ($Op -eq 'rdmem') {
            # CROSEC_READMEM { offset, bytes, buffer[255] }: 2 bytes at EC_MEMMAP_ID
            $buf = New-Object byte[] 264
            [BitConverter]::GetBytes([uint32]0x20).CopyTo($buf, 0)
            [BitConverter]::GetBytes([uint32]2).CopyTo($buf, 4)
            $ok = [SmokeTest.K32]::DeviceIoControl($h, $IOCTL_CROSEC_RDMEM, $buf, 264, $buf, 264, [ref]$ret, [IntPtr]::Zero)
            if (-not $ok) { return "ERROR DeviceIoControl: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())" }
            return [Text.Encoding]::ASCII.GetString($buf, 8, [BitConverter]::ToUInt32($buf, 4))
        }
        # CROSEC_COMMAND { version, command, outsize, insize, result } + data
        $len = 20 + 0xF8
        $buf = New-Object byte[] $len
        [BitConverter]::GetBytes([uint32]1).CopyTo($buf, 4)       # EC_CMD_HELLO
        [BitConverter]::GetBytes([uint32]4).CopyTo($buf, 8)
        [BitConverter]::GetBytes([uint32]4).CopyTo($buf, 12)
        [BitConverter]::GetBytes([uint32]0xFF).CopyTo($buf, 16)
        [BitConverter]::GetBytes([uint32]2695938256).CopyTo($buf, 20) # 0xA0B0C0D0
        $ok = [SmokeTest.K32]::DeviceIoControl($h, $IOCTL_CROSEC_XCMD, $buf, $len, $buf, $len, [ref]$ret, [IntPtr]::Zero)
        if (-not $ok) { return "ERROR DeviceIoControl: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())" }
        return ('returned={0} result={1} out=0x{2:X8}' -f $ret, [BitConverter]::ToUInt32($buf, 16), [BitConverter]::ToUInt32($buf, 20))
    } finally {
        $h.Close()
    }
}

$r = Invoke-WithTimeout $ioctl @('rdmem')
if ($null -eq $r) { Add-Check 'RDMEM signature' $false "timed out after ${TimeoutSeconds}s" }
else { Add-Check 'RDMEM signature' ("$r" -eq 'EC') "read '$r'" }

$r = Invoke-WithTimeout $ioctl @('xcmd')
if ($null -eq $r) { Add-Check 'XCMD EC_CMD_HELLO' $false "timed out after ${TimeoutSeconds}s" }
else { Add-Check 'XCMD EC_CMD_HELLO' ("$r" -eq 'returned=24 result=0 out=0xA1B2C3D4') "$r" }

# --- framework_tool -----------------------------------------------------------
$tool = Get-Command framework_tool -ErrorAction SilentlyContinue
if (-not $tool) {
    Write-Host '[SKIP] framework_tool not on PATH'
} else {
    foreach ($cmd in 'versions', 'power') {
        $r = Invoke-WithTimeout { param($t, $c) & $t "--$c" } @($tool.Source, $cmd)
        if ($null -eq $r) { Add-Check "framework_tool --$cmd" $false "timed out after ${TimeoutSeconds}s"; continue }
        $r | Out-File -Encoding utf8 "$OutDir\framework-$cmd.txt"
        $hasError = [bool]($r | Select-String -Quiet -Pattern 'error|failed|denied')
        Add-Check "framework_tool --$cmd" (-not $hasError) "$($r.Count) lines$(if ($hasError) { ', contains errors' })"
    }
    if ($Baseline) {
        $before = Join-Path $Baseline 'framework-versions.txt'
        $after = "$OutDir\framework-versions.txt"
        if ((Test-Path $before) -and (Test-Path $after)) {
            $diff = Compare-Object (Get-Content $before) (Get-Content $after)
            $diff | ForEach-Object { "$($_.SideIndicator) $($_.InputObject)" } | Out-File -Encoding utf8 "$OutDir\framework-versions.diff"
            Add-Check 'Versions match baseline' (-not $diff) "$(@($diff).Count) differing lines"
        }
    }
}

$passed = -not ($checks | Where-Object { -not $_.Passed })
$summary = [pscustomobject]@{ Time = (Get-Date).ToString('o'); OutDir = $OutDir; Passed = $passed; Checks = $checks }
$summary | ConvertTo-Json -Depth 4 | Out-File -Encoding utf8 "$OutDir\summary.json"
$summary
