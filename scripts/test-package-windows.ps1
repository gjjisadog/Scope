$ErrorActionPreference = "Stop"

$scriptPath = Join-Path $PSScriptRoot "package-windows.ps1"
$script = Get-Content -Raw $scriptPath
$wxsPath = Join-Path $PSScriptRoot "ScopeAnalyzer.wxs"
$wxs = Get-Content -Raw $wxsPath
$releaseCheckPath = Join-Path $PSScriptRoot "release-check.ps1"
$agentsPath = Join-Path (Split-Path -Parent $PSScriptRoot) "AGENTS.md"

function Assert-Contains {
    param(
        [string]$Text,
        [string]$Pattern,
        [string]$Message
    )

    if ($Text -notmatch $Pattern) {
        throw $Message
    }
}

function Assert-NotContains {
    param(
        [string]$Text,
        [string]$Pattern,
        [string]$Message
    )

    if ($Text -match $Pattern) {
        throw $Message
    }
}

Assert-Contains $script '\$mesaReleaseTag\s*=\s*"26\.0\.8"' "Mesa release tag must be pinned."
Assert-Contains $script '\$mesaAssetName\s*=\s*"mesa3d-26\.0\.8-release-msvc\.7z"' "Mesa release asset must be pinned."
Assert-Contains $script '\$mesaAssetSha256\s*=\s*"[0-9a-fA-F]{64}"' "Mesa archive SHA256 must be pinned."
Assert-Contains $script '\$sevenZipSha256\s*=\s*"[0-9a-fA-F]{64}"' "7zr.exe SHA256 must be pinned."
Assert-Contains $script 'SCOPE_PACKAGE_OFFLINE' "Offline packaging mode must be explicit."
Assert-Contains $script 'mesa-runtime-manifest\.json' "Mesa cache must have a manifest."
Assert-Contains $script 'Get-FileHash' "Downloaded packaging dependencies must be hash-checked."
Assert-Contains $script 'Write-Host "Mesa runtime cache hit:' "Mesa cache hit should be visible in release logs."
Assert-Contains $script 'Write-Host "Mesa runtime cache miss:' "Mesa cache miss should be visible in release logs."
Assert-NotContains $script 'releases/latest' "Packaging must not depend on the floating latest Mesa release."
Assert-Contains $script '\$angleManifestName\s*=\s*"angle-runtime-manifest\.json"' "ANGLE runtime must have a manifest."
Assert-Contains $script 'ANGLE_RUNTIME_DIR' "ANGLE runtime source must support an explicit path."
Assert-Contains $script 'third_party\\angle' "ANGLE runtime source must support a repo-local vendored path."
Assert-Contains $script 'SCOPE_ALLOW_SYSTEM_ANGLE' "System ANGLE probing must be explicit opt-in."
Assert-Contains $script 'ANGLE runtime explicit path:' "ANGLE explicit path should be visible in release logs."
Assert-Contains $script 'ANGLE runtime system path:' "ANGLE system path should be visible in release logs when opted in."
Assert-Contains $script 'Write-AngleRuntimeManifest' "ANGLE runtime files must be recorded in a manifest."
Assert-Contains $script 'Write-AngleRuntimeManifest\s+-RuntimeDir\s+\$angleRuntimeDir\s+-ManifestPath\s+\(Join-Path\s+\$stage\s+\$angleManifestName\)' "ANGLE runtime manifest must be written into the package stage."
Assert-Contains $wxs 'angle-runtime-manifest\.json' "ANGLE runtime manifest must be included in MSI packaging."
Assert-Contains $script 'Start-ScopeAnalyzer\.bat' "Default automatic startup script must be packaged."
Assert-Contains $script 'Start-ScopeAnalyzer-Mesa\.bat' "Manual Mesa fallback startup script must be packaged."
Assert-NotContains $script 'Start-ScopeAnalyzer-(OpenGL|DX12|Software)\.bat' "Obsolete forced renderer startup scripts should not be packaged."
Assert-NotContains $wxs 'Start-ScopeAnalyzer-(OpenGL|DX12|Software)\.bat' "MSI should not include obsolete forced renderer startup scripts."

if (-not (Test-Path $releaseCheckPath)) {
    throw "Release preflight script must exist at scripts/release-check.ps1."
}
$releaseCheck = Get-Content -Raw $releaseCheckPath
Assert-Contains $releaseCheck 'cargo\s+fmt\s+--check' "Release preflight must check formatting."
Assert-Contains $releaseCheck 'cargo\s+clippy\s+--all-targets' "Release preflight must run clippy."
Assert-Contains $releaseCheck 'cargo\s+test\s+--quiet' "Release preflight must run normal tests."
Assert-Contains $releaseCheck 'run-performance-baselines\.ps1' "Release preflight must run ignored performance baselines."
Assert-Contains $releaseCheck 'test-package-windows\.ps1' "Release preflight must run packaging script checks."
Assert-Contains $releaseCheck 'Assert-VersionSync' "Release preflight must verify release version sync."

$agents = Get-Content -Raw $agentsPath
Assert-Contains $agents 'scripts/release-check\.ps1' "AGENTS.md must tell future agents to run release preflight."
Assert-Contains $agents 'scripts/package-windows\.ps1 -OfflinePackage' "AGENTS.md must document the release packaging command."
Assert-Contains $agents 'ANGLE_RUNTIME_DIR' "AGENTS.md must document reproducible ANGLE input."
Assert-Contains $agents 'SCOPE_PACKAGE_OFFLINE=1' "AGENTS.md must document offline reproducible packaging."

Write-Host "package-windows.ps1 reproducibility checks passed"
