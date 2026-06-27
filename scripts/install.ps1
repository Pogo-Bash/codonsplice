# CodonSplice installer for Windows (PowerShell).
#
#   irm https://github.com/Pogo-Bash/codonsplice/releases/latest/download/install.ps1 | iex
#
# Downloads the latest `splice-windows-x86_64.exe` from GitHub Releases,
# installs it as `splice.exe`, and adds it to PATH. No admin required — it
# installs per-user under %LOCALAPPDATA% unless run elevated. Override the
# install dir with:  $env:CODONSPLICE_DIR = "C:\tools\splice"; irm ... | iex
$ErrorActionPreference = "Stop"

$repo  = "Pogo-Bash/codonsplice"
$asset = "splice-windows-x86_64.exe"

# Apple Silicon-style arch note: only x86_64 is published for Windows; it runs
# fine on Windows on ARM via the x64 emulation layer.
if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") {
  Write-Host "Note: no native ARM64 build yet — installing the x64 binary (runs under emulation)."
}

# Run elevated? Then we may install machine-wide; otherwise per-user (no admin).
$isAdmin = ([Security.Principal.WindowsPrincipal] `
  [Security.Principal.WindowsIdentity]::GetCurrent() `
).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

$installDir =
  if ($env:CODONSPLICE_DIR) { $env:CODONSPLICE_DIR }
  elseif ($isAdmin)         { "$env:ProgramFiles\CodonSplice" }
  else                      { "$env:LOCALAPPDATA\CodonSplice" }

Write-Host "Fetching the latest CodonSplice release..."
$release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest"
$url = ($release.assets | Where-Object { $_.name -eq $asset }).browser_download_url
if (-not $url) { Write-Error "Could not find $asset in the latest release."; return }

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
$dest = Join-Path $installDir "splice.exe"
Write-Host "Downloading $asset -> $dest"
Invoke-WebRequest -Uri $url -OutFile $dest

# Add the install dir to PATH for this user (or the machine, if elevated).
$scope = if ($isAdmin) { "Machine" } else { "User" }
$cur = [Environment]::GetEnvironmentVariable("PATH", $scope)
if ($cur -notlike "*$installDir*") {
  [Environment]::SetEnvironmentVariable("PATH", "$cur;$installDir", $scope)
  Write-Host "Added $installDir to the $scope PATH."
}
# Make it usable in the current session too.
$env:PATH = "$env:PATH;$installDir"

$v = & $dest --version 2>&1
Write-Host ""
Write-Host "Installed: $v"
Write-Host "Open a new terminal (so PATH refreshes), then run:  splice --help"
