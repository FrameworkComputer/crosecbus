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

Add-Type -Namespace SwitchDriver -Name NewDev -MemberDefinition @'
[DllImport("newdev.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern bool UpdateDriverForPlugAndPlayDevicesW(
    IntPtr hwndParent, string HardwareId, string FullInfPath,
    uint InstallFlags, out bool bRebootRequired);
'@
$INSTALLFLAG_FORCE = 0x1
$reboot = $false
if (-not [SwitchDriver.NewDev]::UpdateDriverForPlugAndPlayDevicesW([IntPtr]::Zero, $hwid, $pick.Inf, $INSTALLFLAG_FORCE, [ref]$reboot)) {
    throw "Install failed: 0x$('{0:X8}' -f [Runtime.InteropServices.Marshal]::GetLastWin32Error())"
}
if ($reboot) { Write-Warning 'Reboot required to finish switching drivers.' }

# Report what is actually loaded now.
$dev = Get-PnpDevice -PresentOnly | Where-Object { $_.HardwareID -contains $hwid } | Select-Object -First 1
$svc = Get-CimInstance Win32_SystemDriver -Filter "Name='CrosEcBus'"
$sys = $svc.PathName -replace '^\\\?\?\\', ''
$loadedSigner = (Get-AuthenticodeSignature $sys).SignerCertificate.Subject
Write-Host ("Device {0}: {1}, service {2}, binary signed by {3}" -f $dev.InstanceId, $dev.Status, $svc.State, $loadedSigner)
if ($loadedSigner -notmatch [regex]::Escape($signers[$To])) {
    throw "Loaded binary is not the $To driver"
}
