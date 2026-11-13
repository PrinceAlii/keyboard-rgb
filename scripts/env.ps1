# Loads the MSVC (Visual Studio) environment and Rust toolchain into the
# current PowerShell session, then runs whatever command you pass.
#
# Usage:
#   . .\scripts\env.ps1
#   cargo build
#
# Or:
#   .\scripts\env.ps1 cargo build --release

$ErrorActionPreference = 'Stop'

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path $vswhere)) {
    throw "vswhere.exe not found. Install Visual Studio Build Tools with the C++ workload."
}

$vsPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vsPath) {
    throw "No Visual Studio installation with MSVC C++ tools found."
}

$vcvars = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat'
if (-not (Test-Path $vcvars)) {
    throw "vcvars64.bat not found at $vcvars"
}

# Import the environment that vcvars64.bat would set in a Developer Command Prompt.
$envDump = cmd /c "`"$vcvars`" >nul 2>&1 && set"
foreach ($line in $envDump) {
    if ($line -match '^([^=]+)=(.*)$') {
        Set-Item -Path "Env:$($matches[1])" -Value $matches[2]
    }
}

# Rustup installs cargo/rustc here but does not always add it to PATH.
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if ((Test-Path $cargoBin) -and ($env:Path -notlike "*$cargoBin*")) {
    $env:Path = "$cargoBin;$env:Path"
}

Write-Host "MSVC:  $vsPath"
Write-Host "Rust:  $(& rustc --version)"
Write-Host "Cargo: $(& cargo --version)"

if ($args.Count -gt 0) {
    & $args[0] @($args[1..($args.Count - 1)])
}
