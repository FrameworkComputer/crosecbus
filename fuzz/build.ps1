<#
.SYNOPSIS
Builds the user-mode libFuzzer harness for crosecbus.

.DESCRIPTION
Compiles the driver's comm-lpc.c, comm-mec_lpc.c and userspaceQueue.c
unmodified against fuzz/shim, with MSVC AddressSanitizer and libFuzzer.
Output: fuzz/build/fuzz_ioctl.exe (plus the ASan runtime DLL next to it).

Run it, e.g.:
  fuzz\build\fuzz_ioctl.exe fuzz\build\corpus fuzz\corpus -dict=fuzz\ec.dict -max_total_time=300
Replay saved crashes / regressions:
  fuzz\build\fuzz_ioctl.exe fuzz\regressions\*

Pass -Trace to build fuzz_ioctl_trace.exe instead, which logs every IOCTL
and its result to stderr (handy when triaging a crash input).
#>
param([switch]$Trace)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$fuzz = $PSScriptRoot
$out = Join-Path $fuzz 'build'
New-Item -ItemType Directory -Force $out | Out-Null
New-Item -ItemType Directory -Force (Join-Path $out 'corpus') | Out-Null

# Import the MSVC x64 environment
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { throw 'Visual Studio with the C++ x64 tools not found' }
cmd /c "`"$vs\VC\Auxiliary\Build\vcvars64.bat`" >nul 2>&1 && set" | ForEach-Object {
	if ($_ -match '^([^=]+)=(.*)$') { Set-Item "env:$($Matches[1])" $Matches[2] }
}

$sources = @(
	"$fuzz\fuzz_ioctl.c",
	"$fuzz\fake_ec.c",
	"$fuzz\fake_wdf.c",
	"$root\crosecbus\comm-lpc.c",
	"$root\crosecbus\comm-mec_lpc.c",
	"$root\crosecbus\userspaceQueue.c"
)

$flags = @(
	'/nologo', '/Zi', '/O2', '/W3',
	'/fsanitize=address', '/fsanitize=fuzzer',
	"/I$fuzz\shim", "/I$root\crosecbus", '/FIfuzz_shim.h',
	# Warnings inherent to building kernel sources against the shim
	'/wd4057', '/wd4047', '/wd4133', '/wd4312',
	"/Fo$out\", "/Fd$out\"
)
$exe = if ($Trace) { "$out\fuzz_ioctl_trace.exe" } else { "$out\fuzz_ioctl.exe" }
$flags += "/Fe$exe"
if ($Trace) { $flags += '/DFUZZ_VERBOSE' }

& cl @flags @sources
if ($LASTEXITCODE -ne 0) { throw "cl failed with exit code $LASTEXITCODE" }

$asan = Join-Path $env:VCToolsInstallDir 'bin\Hostx64\x64\clang_rt.asan_dynamic-x86_64.dll'
Copy-Item $asan $out -Force

Write-Host "Built $exe"
