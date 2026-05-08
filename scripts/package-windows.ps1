$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$version = "0.1.0"
$dist = Join-Path $root "dist"
$stage = Join-Path $dist "ScopeAnalyzer-$version-win-x64"

New-Item -ItemType Directory -Force -Path $dist | Out-Null
if (Test-Path $stage) {
    Remove-Item -Recurse -Force $stage
}
New-Item -ItemType Directory -Force -Path $stage | Out-Null

Push-Location $root
cargo build --release
Copy-Item "target\release\scope_analyzer.exe" (Join-Path $stage "ScopeAnalyzer.exe")
Copy-Item "README.md" (Join-Path $stage "README.txt")
Pop-Location

$zip = Join-Path $dist "ScopeAnalyzer-$version-win-x64.zip"
if (Test-Path $zip) {
    Remove-Item -Force $zip
}
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip
Write-Host "Created $zip"

