#Requires -Version 5.1
<#
.SYNOPSIS
    Build quackc and install it on your PATH so `quackc` works from any folder.

.DESCRIPTION
    One-command build + install for the Quack s-t algebra compiler. It:
      1. builds the optimized release binary (cargo build --release),
      2. copies quackc.exe and the standard library (std.sta) into an install folder,
      3. sets the QUACKC_STDLIB_PATH user environment variable (so the auto-imported
         std.sta is always found, even when quackc runs outside this source tree),
      4. adds the install folder to your user PATH (idempotent),
      5. verifies the installed binary runs.

    Re-run this script any time you change the compiler or the standard library to
    update the installed copy.

.PARAMETER InstallDir
    Where to install. Defaults to %USERPROFILE%\quackc.

.PARAMETER SkipBuild
    Skip `cargo build --release` and install the binary already in target\release.

.EXAMPLE
    .\install.ps1
    Build and install to the default location.

.EXAMPLE
    .\install.ps1 -InstallDir D:\tools\quackc
    Install to a custom folder.

.EXAMPLE
    .\install.ps1 -SkipBuild
    Reinstall the existing release binary without rebuilding.
#>
[CmdletBinding()]
param(
    [string] $InstallDir = (Join-Path $env:USERPROFILE "quackc"),
    [switch] $SkipBuild
)

# Stop on the first unhandled error so a failed step never leaves a half-done install.
$ErrorActionPreference = "Stop"

# Resolve paths relative to THIS script, so the script works no matter the current directory.
$ProjectRoot = $PSScriptRoot
$ExeSource   = Join-Path $ProjectRoot "target\release\quackc.exe"
$StdSource   = Join-Path $ProjectRoot "std\std.sta"
$StdDir      = Join-Path $InstallDir "std"

function Write-Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

# 1. Build (unless skipped). ----------------------------------------------------------------------
if ($SkipBuild) {
    Write-Step "Skipping build (-SkipBuild); using the existing release binary."
} else {
    Write-Step "Building the release binary (cargo build --release)..."
    Push-Location $ProjectRoot
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE." }
    } finally {
        Pop-Location
    }
}

if (-not (Test-Path $ExeSource)) {
    throw "Release binary not found at '$ExeSource'. Run without -SkipBuild, or build first."
}
if (-not (Test-Path $StdSource)) {
    throw "Standard library not found at '$StdSource'."
}

# 2. Copy the binary and the standard library into the install folder. ----------------------------
Write-Step "Installing to '$InstallDir'..."
New-Item -ItemType Directory -Force -Path $InstallDir, $StdDir | Out-Null
Copy-Item $ExeSource -Destination $InstallDir -Force
Copy-Item $StdSource -Destination $StdDir     -Force

# 3. Point the compiler at its standard library (the directory containing std.sta). ---------------
Write-Step "Setting QUACKC_STDLIB_PATH = '$StdDir' (user)."
[Environment]::SetEnvironmentVariable("QUACKC_STDLIB_PATH", $StdDir, "User")
$env:QUACKC_STDLIB_PATH = $StdDir  # also for this session

# 4. Add the install folder to the user PATH, only if not already present. -------------------------
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ([string]::IsNullOrEmpty($userPath)) { $userPath = "" }
$onPath = ($userPath -split ';') -contains $InstallDir
if ($onPath) {
    Write-Step "Install folder already on user PATH."
} else {
    Write-Step "Adding '$InstallDir' to user PATH."
    $newPath = if ($userPath.TrimEnd(';') -eq "") { $InstallDir } else { $userPath.TrimEnd(';') + ";" + $InstallDir }
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
}
# Make it resolvable in THIS session too, so the verification below works immediately.
if (($env:Path -split ';') -notcontains $InstallDir) { $env:Path = "$env:Path;$InstallDir" }

# 5. Verify the installed binary runs. ------------------------------------------------------------
Write-Step "Verifying..."
$installedExe = Join-Path $InstallDir "quackc.exe"
$version = & $installedExe --version
if ($LASTEXITCODE -ne 0) { throw "Installed quackc failed to run (--version exit $LASTEXITCODE)." }

Write-Host ""
Write-Host "Installed $version to $InstallDir" -ForegroundColor Green
if (-not $onPath) {
    Write-Host "Open a NEW terminal for the updated PATH to take effect, then run: quackc --help" -ForegroundColor Yellow
} else {
    Write-Host "Run 'quackc --help' to get started." -ForegroundColor Green
}