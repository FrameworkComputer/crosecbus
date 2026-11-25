#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Install a test-signed crosecbus build (e.g. a CI artifact) on this machine.

.DESCRIPTION
    CI builds are signed with a throwaway WDK test certificate that changes on
    every run. This script:
      1. Checks that test signing is enabled.
      2. Adds the package's signing certificate to the LocalMachine Root and
         TrustedPublisher stores so Windows accepts the catalog.
      3. Adds the driver package to the driver store.
      4. Force-installs it on the matching device(s), because PnP otherwise
         keeps a WHQL-signed driver over any test-signed one.

    The previous driver stays in the driver store, so you can switch back with
    Device Manager -> Roll Back Driver.

.PARAMETER Path
    Folder containing crosecbus.inf, crosecbus.cat and crosecbus.sys.
    Defaults to the script's own folder, which is where it sits in the CI bundle
    and in packages made by package-local.ps1.

.EXAMPLE
    .\install-testsigned.ps1
    Run from an extracted CI bundle.

.EXAMPLE
    .\scripts\install-testsigned.ps1 -Path target\release\crosecbus_package
#>
param(
    [string]$Path = $PSScriptRoot
)

$ErrorActionPreference = 'Stop'

$inf = Join-Path $Path 'crosecbus.inf'
$cat = Join-Path $Path 'crosecbus.cat'
foreach ($f in $inf, $cat, (Join-Path $Path 'crosecbus.sys')) {
    if (-not (Test-Path $f)) { throw "Missing $f" }
}
$inf = (Resolve-Path $inf).Path

# 1. Test signing
if (-not (bcdedit /enum '{current}' | Select-String -Quiet 'testsigning\s+Yes')) {
    throw "Test signing is off. Run 'bcdedit /set testsigning on' and reboot (Secure Boot must be disabled)."
}

# 2. Trust the signing certificate
$cert = (Get-AuthenticodeSignature $cat).SignerCertificate
if (-not $cert) { throw "$cat is not signed" }
Write-Host "Signer: $($cert.Subject) ($($cert.Thumbprint))"
foreach ($store in 'Root', 'TrustedPublisher') {
    $s = New-Object Security.Cryptography.X509Certificates.X509Store($store, 'LocalMachine')
    $s.Open('ReadWrite')
    try {
        if (-not $s.Certificates.Find('FindByThumbprint', $cert.Thumbprint, $false).Count) {
            $s.Add($cert)
            Write-Host "Added certificate to LocalMachine\$store"
        }
    } finally {
        $s.Close()
    }
}
$status = (Get-AuthenticodeSignature $cat).Status
if ($status -ne 'Valid') {
    $now = [DateTime]::Now
    if ($now -lt $cert.NotBefore -or $now -gt $cert.NotAfter) {
        throw ("Signing certificate is valid {0:u} to {1:u} (UTC) but the system clock says {2:u}. " +
               "Fix the system clock (e.g. 'w32tm /resync') and rerun.") -f
              $cert.NotBefore.ToUniversalTime(), $cert.NotAfter.ToUniversalTime(), $now.ToUniversalTime()
    }
    throw "Catalog signature still not trusted: $status - $((Get-AuthenticodeSignature $cat).StatusMessage)"
}

# 3. Stage the package
pnputil /add-driver $inf
if ($LASTEXITCODE -ne 0) { throw "pnputil /add-driver failed ($LASTEXITCODE)" }

# 4. Force-install on every hardware ID listed in the INF
Add-Type -Namespace Native -Name NewDev -MemberDefinition @'
[DllImport("newdev.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern bool UpdateDriverForPlugAndPlayDevicesW(
    IntPtr hwndParent, string HardwareId, string FullInfPath,
    uint InstallFlags, out bool bRebootRequired);
'@
$INSTALLFLAG_FORCE = 0x1

$hwids = Select-String -Path $inf -Pattern '^\s*%[^%]+%\s*=\s*[^,]+,\s*(\S+\\\S+)' |
    ForEach-Object { $_.Matches[0].Groups[1].Value } | Sort-Object -Unique
$reboot = $false
foreach ($id in $hwids) {
    $r = $false
    if ([Native.NewDev]::UpdateDriverForPlugAndPlayDevicesW([IntPtr]::Zero, $id, $inf, $INSTALLFLAG_FORCE, [ref]$r)) {
        Write-Host "Installed on $id"
        $reboot = $reboot -or $r
    } else {
        $err = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        # 0xE000020B = ERROR_NO_SUCH_DEVINST: no device with this ID present
        if ($err -eq 0xE000020B) { Write-Host "No device present for $id, skipped" }
        else { throw "Install on $id failed: 0x$('{0:X8}' -f $err)" }
    }
}

if ($reboot) { Write-Warning 'Reboot required to finish installing.' }
foreach ($id in $hwids) {
    Get-PnpDevice -PresentOnly | Where-Object { $_.HardwareID -contains $id } | ForEach-Object {
        pnputil /enum-devices /instanceid $_.InstanceId /drivers
    }
}
