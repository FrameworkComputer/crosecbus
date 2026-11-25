<#
.SYNOPSIS
    Build and test-sign a crosecbus driver package locally.

.DESCRIPTION
    Does what 'cargo make' does (build, stampinf, infverif, inf2cat, sign)
    without needing Developer Mode for its symlinks. Signs with a self-signed
    code signing certificate in CurrentUser\My, created on first use.

    The package lands in target\<profile>\crosecbus_package together with
    install-testsigned.ps1, which installs it (run that as Administrator with
    test signing on).

.PARAMETER Profile
    'release' (default) or 'debug'.

.PARAMETER Version
    DriverVer stamped into the INF.

.EXAMPLE
    .\scripts\package-local.ps1
    .\target\release\crosecbus_package\install-testsigned.ps1
#>
param(
    [ValidateSet('release', 'debug')]
    [string]$Profile = 'release',
    [string]$Version = '0.1.0.0'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot

$kits = 'C:\Program Files (x86)\Windows Kits\10'
$sdk = (Get-ChildItem "$kits\bin" -Directory | Where-Object Name -match '^10\.' |
    Sort-Object Name | Select-Object -Last 1).Name
$stampinf = "$kits\bin\$sdk\x64\stampinf.exe"
$signtool = "$kits\bin\$sdk\x64\signtool.exe"
$inf2cat = "$kits\bin\$sdk\x86\Inf2Cat.exe"
$infverif = "$kits\Tools\$sdk\x64\infverif.exe"
foreach ($tool in $stampinf, $signtool, $inf2cat, $infverif) {
    if (-not (Test-Path $tool)) { throw "Missing $tool (is the WDK installed?)" }
}

function Invoke-Checked {
    # Native tools (cargo) write progress to stderr, which Windows PowerShell
    # would turn into a terminating error under 'Stop'. Use the exit code.
    $ErrorActionPreference = 'Continue'
    & $args[0] $args[1..($args.Count - 1)] 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { throw "$(Split-Path -Leaf $args[0]) failed ($LASTEXITCODE)" }
}

# 1. Build
$cargoArgs = @('build', '--locked', '-p', 'crosecbus')
if ($Profile -eq 'release') { $cargoArgs += '--release' }
Push-Location $root
try { Invoke-Checked cargo @cargoArgs } finally { Pop-Location }

# 2. Lay out the package
$out = Join-Path $root "target\$Profile"
$pkg = Join-Path $out 'crosecbus_package'
if (Test-Path $pkg) { Remove-Item -Recurse -Force $pkg }
New-Item -ItemType Directory $pkg | Out-Null
Copy-Item "$root\crosecbus\crosecbus.inx" "$pkg\crosecbus.inf"
Copy-Item "$out\crosecbus.dll" "$pkg\crosecbus.sys"
Copy-Item "$out\crosecbus.pdb" $pkg
Copy-Item "$PSScriptRoot\install-testsigned.ps1" $pkg

# 3. Stamp and verify the INF (same flags as wdk-build's cargo-make flow)
Invoke-Checked $stampinf -f "$pkg\crosecbus.inf" -d '*' -a amd64 -c crosecbus.cat -v $Version -k 1.33
Invoke-Checked $infverif /v /w "$pkg\crosecbus.inf"

# 4. Sign the driver, build the catalog, sign the catalog
$subject = 'CN=crosecbus-rs local test signing'
$cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
    Where-Object { $_.Subject -eq $subject -and $_.NotAfter -gt (Get-Date) } |
    Select-Object -First 1
if (-not $cert) {
    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $subject `
        -CertStoreLocation Cert:\CurrentUser\My -NotAfter (Get-Date).AddYears(2)
    Write-Host "Created test signing certificate $($cert.Thumbprint) in CurrentUser\My"
}
Export-Certificate -Cert $cert -FilePath "$pkg\crosecbus-test.cer" | Out-Null

$sign = @('sign', '/fd', 'SHA256', '/s', 'My', '/sha1', $cert.Thumbprint)
Invoke-Checked $signtool @sign "$pkg\crosecbus.sys"
Invoke-Checked $inf2cat "/driver:$pkg" /os:10_x64 /uselocaltime
Invoke-Checked $signtool @sign "$pkg\crosecbus.cat"

Write-Host "`nPackage: $pkg"
Get-ChildItem $pkg | Format-Table Name, Length -AutoSize
