# Builds the release exe and copies it over the one in -Dir.
# Refuses while RENotepad runs from there: the exe is locked, and closing it
# for you could lose unsaved tabs.
# Usage: pwsh scripts/install.ps1 [-Dir E:\RENotepad]
param([string]$Dir = 'E:\RENotepad')
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$target = Join-Path $Dir 'RENotepad.exe'
if (Get-Process RENotepad -ErrorAction SilentlyContinue | Where-Object Path -eq $target) {
  throw "RENotepad is running from ${Dir}: close it first"
}

$cargo = if (Get-Command cargo -ErrorAction SilentlyContinue) { 'cargo' } else { Join-Path $HOME '.cargo\bin\cargo.exe' }
& $cargo build --release
if ($LASTEXITCODE) { throw 'cargo build failed' }

New-Item -ItemType Directory -Force $Dir | Out-Null
Copy-Item (Join-Path $root 'target\release\RENotepad.exe') $target -Force
"Installed $((Get-Item $target).VersionInfo.ProductVersion) to $target"
