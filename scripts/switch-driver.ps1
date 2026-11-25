#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Switch the Framework EC device between Framework's C crosecbus driver and
    this Rust port.

.DESCRIPTION
    Both drivers must already be in the driver store: the C driver ships with
    Windows Update / Framework's driver bundle, the Rust one is added by
    install-testsigned.ps1. The packages are told apart by who signed their
    catalog:
      c     Microsoft Windows Hardware Compatibility Publisher (WHQL)
      rust  crosecbus-rs local test signing (needs test signing on)
    The highest version of the chosen kind is force-installed on
    ACPI\FRMWC004, since PnP would otherwise keep whichever ranks best.

    Storage Sense deletes driver packages that aren't in use, so whichever
    driver is not installed may disappear from the store. Pass -CPackage /
    -RustPackage to re-add it from a folder in that case.

.PARAMETER To
    c or rust.

.PARAMETER CPackage
    Folder with Framework's WHQL crosecbus.inf/.cat/.sys (e.g. the
    "Framework_EC" folder of Framework's driver bundle). Added to the store
    if no WHQL crosecbus package is there.

.PARAMETER RustPackage
    Folder with a test-signed Rust package. Defaults to
    target\release\crosecbus_package.

.EXAMPLE
    .\scripts\switch-driver.ps1 -To c -CPackage $env:USERPROFILE\Downloads\...\19_Framework_EC_0.0.0.6
    .\scripts\test-power.ps1 -Mode hibernate -Cycles 1
    .\scripts\switch-driver.ps1 -To rust
#>
param(
    [Parameter(Mandatory)]
    [ValidateSet('c', 'rust')]
    [string]$To,
    [string]$CPackage,
    [string]$RustPackage = (Join-Path (Split-Path $PSScriptRoot) 'target\release\crosecbus_package')
)

$ErrorActionPreference = 'Stop'
$hwid = 'ACPI\FRMWC004'
$signers = @{
    c    = 'Microsoft Windows Hardware Compatibility Publisher'
    rust = 'crosecbus-rs local test signing'
}

# Find crosecbus packages in the driver store and who signed them.
function Get-CrosEcPackages {
    Get-WindowsDriver -Online | Where-Object { (Split-Path -Leaf $_.OriginalFileName) -eq 'crosecbus.inf' } |
        ForEach-Object {
            $cat = Get-ChildItem (Split-Path $_.OriginalFileName) -Filter *.cat | Select-Object -First 1
            [pscustomobject]@{
                Published = $_.Driver
                Inf       = $_.OriginalFileName
                Version   = [version]$_.Version
                Signer    = if ($cat) { (Get-AuthenticodeSignature $cat.FullName).SignerCertificate.Subject } else { '' }
            }
        }
}
function Select-Package($packages) {
    $packages | Where-Object { $_.Signer -match [regex]::Escape($signers[$To]) } |
        Sort-Object Version -Descending | Select-Object -First 1
}

$packages = Get-CrosEcPackages
$pick = Select-Package $packages
if (-not $pick) {
    $source = if ($To -eq 'c') { $CPackage } else { $RustPackage }
    if (-not $source -or -not (Test-Path (Join-Path $source 'crosecbus.inf'))) {
        $hint = if ($To -eq 'c') { 'Pass -CPackage <folder with Framework''s crosecbus.inf>.' } else { 'Run package-local.ps1 or pass -RustPackage.' }
        throw "No $To crosecbus package (signed by '$($signers[$To])') in the driver store. $hint"
    }
    $sourceSigner = (Get-AuthenticodeSignature (Join-Path $source 'crosecbus.cat')).SignerCertificate.Subject
    if ($sourceSigner -notmatch [regex]::Escape($signers[$To])) {
        throw "$source is signed by '$sourceSigner', not '$($signers[$To])'"
    }
    Write-Host "Adding $To package from $source to the driver store"
    pnputil /add-driver (Join-Path $source 'crosecbus.inf') | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "pnputil /add-driver failed ($LASTEXITCODE)" }
    $packages = Get-CrosEcPackages
    $pick = Select-Package $packages
    if (-not $pick) { throw "Package was added but not found in the driver store" }
}
$packages | Format-Table Published, Version, Signer -AutoSize | Out-String | Write-Host
Write-Host "Installing $To driver: $($pick.Published) $($pick.Version)"

# The service's registry path changes immediately, but the old image stays
# loaded until the device restarts, so check the image the kernel has loaded.
Add-Type -Namespace SwitchDriver -Name Native -MemberDefinition @'
[DllImport("newdev.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern bool UpdateDriverForPlugAndPlayDevicesW(
    IntPtr hwndParent, string HardwareId, string FullInfPath,
    uint InstallFlags, out bool bRebootRequired);
[DllImport("psapi.dll", SetLastError = true)]
public static extern bool EnumDeviceDrivers(IntPtr[] addresses, uint size, out uint needed);
[DllImport("psapi.dll", CharSet = CharSet.Unicode)]
public static extern uint GetDeviceDriverFileNameW(IntPtr imageBase, System.Text.StringBuilder name, uint size);
'@
function Get-LoadedCrosEcImage {
    $needed = [uint32]0
    [void][SwitchDriver.Native]::EnumDeviceDrivers($null, 0, [ref]$needed)
    $bases = New-Object IntPtr[] ($needed / [IntPtr]::Size)
    [void][SwitchDriver.Native]::EnumDeviceDrivers($bases, $needed, [ref]$needed)
    foreach ($b in $bases) {
        $sb = New-Object Text.StringBuilder 512
        [void][SwitchDriver.Native]::GetDeviceDriverFileNameW($b, $sb, 512)
        if ($sb.ToString() -match '\crosecbus\.sys$') { return $sb.ToString() }
    }
}
$wantFolder = Split-Path -Leaf (Split-Path $pick.Inf)
function Test-Switched { (Get-LoadedCrosEcImage) -match [regex]::Escape("\$wantFolder\") }

$dev = Get-PnpDevice -PresentOnly | Where-Object { $_.HardwareID -contains $hwid } | Select-Object -First 1
if ((Get-PnpDeviceProperty -InstanceId $dev.InstanceId -KeyName DEVPKEY_Device_IsRebootRequired -ErrorAction SilentlyContinue).Data) {
    throw "$($dev.InstanceId) is already waiting for a reboot to finish an earlier driver change. Reboot, then run this again."
}

# Framework Sensors (UMDF) keeps \Device\CrosEC open and vetoes the EC
# device's removal, which makes PnP defer any driver change to a reboot.
# Stop it for the duration of the switch.
$clients = @(Get-PnpDevice -PresentOnly | Where-Object {
        $inf = (Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName DEVPKEY_Device_DriverInfPath -ErrorAction SilentlyContinue).Data
        $inf -and (Get-WindowsDriver -Online -Driver $inf -ErrorAction SilentlyContinue |
                Select-Object -First 1).OriginalFileName -match '\frameworksensors\.inf$'
    } | Where-Object Status -eq 'OK')
foreach ($c in $clients) {
    Write-Host "Stopping EC client $($c.FriendlyName) ($($c.InstanceId))"
    Disable-PnpDevice -InstanceId $c.InstanceId -Confirm:$false
}
try {
    $INSTALLFLAG_FORCE = 0x1
    $reboot = $false
    if (-not [SwitchDriver.Native]::UpdateDriverForPlugAndPlayDevicesW([IntPtr]::Zero, $hwid, $pick.Inf, $INSTALLFLAG_FORCE, [ref]$reboot)) {
        throw "Install failed: 0x$('{0:X8}' -f [Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    if (-not (Test-Switched) -and -not $reboot) {
        # Already the selected driver (e.g. a rerun): just restart the device.
        Write-Host "Restarting $($dev.InstanceId)"
        pnputil /restart-device $dev.InstanceId | Out-Host
    }
} finally {
    foreach ($c in $clients) {
        Write-Host "Starting EC client $($c.FriendlyName)"
        Enable-PnpDevice -InstanceId $c.InstanceId -Confirm:$false
    }
}

$dev = Get-PnpDevice -PresentOnly | Where-Object { $_.HardwareID -contains $hwid } | Select-Object -First 1
Write-Host ("Device {0}: {1}; loaded image {2}" -f $dev.InstanceId, $dev.Status, (Get-LoadedCrosEcImage))
if (-not (Test-Switched)) {
    throw "The $To driver is selected but not loaded yet; reboot to finish switching."
}
Write-Host "Now running the $To driver ($($pick.Published) $($pick.Version))."
