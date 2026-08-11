# kungfu installer for Windows
# Usage:
#   irm https://raw.githubusercontent.com/denyzhirkov/kungfu/master/install.ps1 | iex
$ErrorActionPreference = "Stop"

$Repo = "denyzhirkov/kungfu"
$Version = if ($env:KUNGFU_VERSION) { $env:KUNGFU_VERSION } else { "latest" }
$InstallDir = if ($env:KUNGFU_DIR) { $env:KUNGFU_DIR } else { "$env:USERPROFILE\.local\bin" }

# Detect architecture
$RawArch = $null
try {
    $RawArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
} catch {}
if (-not $RawArch) {
    # Fallback for Windows PowerShell 5.1
    $RawArch = $env:PROCESSOR_ARCHITECTURE
}
switch ($RawArch) {
    "X64"   { $Arch = "x86_64" }
    "AMD64" { $Arch = "x86_64" }
    "Arm64" { $Arch = "aarch64" }
    "ARM64" { $Arch = "aarch64" }
    default { Write-Error "Unsupported architecture: $RawArch"; exit 1 }
}

$Platform = "windows-$Arch"

# Resolve version
if ($Version -eq "latest") {
    try {
        $Release = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
        $Version = $Release.tag_name -replace "^v", ""
    } catch {
        Write-Error "Failed to detect latest version. Set `$env:KUNGFU_VERSION=x.y.z manually."
        exit 1
    }
}

$Tag = "v$Version"
$Binary = "kungfu-$Platform.exe"
$Url = "https://github.com/$Repo/releases/download/$Tag/$Binary"

Write-Host ""
Write-Host "  kungfu installer"
Write-Host "  ----------------"
Write-Host "  Version:  $Version"
Write-Host "  Platform: $Platform"
Write-Host ""

# Download
$TmpFile = [System.IO.Path]::GetTempFileName() + ".exe"
Write-Host "  Downloading..."
try {
    Invoke-WebRequest -Uri $Url -OutFile $TmpFile -UseBasicParsing
} catch {
    Write-Error "  Binary not found at $Url"
    Write-Host "  Check available releases: https://github.com/$Repo/releases"
    exit 1
}

# Install
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}
$Dest = Join-Path $InstallDir "kungfu.exe"
Move-Item -Force $TmpFile $Dest
Write-Host "  -> $Dest"

# Add to PATH if not already there
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$InstallDir;$UserPath", "User")
    Write-Host "  -> Added $InstallDir to user PATH"
    Write-Host "  -> Restart your terminal for PATH changes to take effect"
}

Write-Host ""
Write-Host "  Done! Run 'kungfu --help' to get started."
Write-Host "  Later: 'kungfu update' upgrades this binary in place."
Write-Host ""
