$ErrorActionPreference = "Stop"

$scriptPath = Join-Path $PSScriptRoot "package-windows.ps1"
$script = Get-Content -Raw $scriptPath
$wxsPath = Join-Path $PSScriptRoot "ScopeAnalyzer.wxs"
$wxs = Get-Content -Raw $wxsPath
$releaseCheckPath = Join-Path $PSScriptRoot "release-check.ps1"
$acceptancePath = Join-Path $PSScriptRoot "windows-acceptance.ps1"
$workflowPath = Join-Path (Split-Path -Parent $PSScriptRoot) ".github\workflows\windows-release-acceptance.yml"
$standardWorkflowPath = Join-Path (Split-Path -Parent $PSScriptRoot) ".github\workflows\windows-standard-user-acceptance.yml"
$hardwareWorkflowPath = Join-Path (Split-Path -Parent $PSScriptRoot) ".github\workflows\windows-hardware-smoke.yml"
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
Assert-Contains $script '\$wixToolsetVersion\s*=\s*"3\.14"' "WiX Toolset major version must be pinned."
Assert-Contains $script 'SCOPE_PACKAGE_OFFLINE' "Offline packaging mode must be explicit."
Assert-Contains $script '--locked' "Packaging must lock Cargo dependencies."
Assert-Contains $script '--offline' "Offline packaging must pass Cargo offline mode."
Assert-Contains $script '--bin.*scope-cli' "CLI binary must be built for release packaging."
Assert-Contains $script 'scope-cli\.exe' "CLI binary must be included in the release stage."
Assert-Contains $script 'mesa-runtime-manifest\.json' "Mesa cache must have a manifest."
Assert-Contains $script 'Get-FileHash' "Downloaded packaging dependencies must be hash-checked."
Assert-Contains $script 'manifest does not hash required file' "Mesa manifests must hash every required runtime DLL."
Assert-Contains $script 'Write-Host "Mesa runtime cache hit:' "Mesa cache hit should be visible in release logs."
Assert-Contains $script 'Write-Host "Mesa runtime cache miss:' "Mesa cache miss should be visible in release logs."
Assert-NotContains $script 'releases/latest' "Packaging must not depend on the floating latest Mesa release."
Assert-Contains $script '\$angleManifestName\s*=\s*"angle-runtime-manifest\.json"' "ANGLE runtime must have a manifest."
Assert-Contains $script '\$anglePreloadManifestName\s*=\s*"angle-runtime-preload-manifest\.json"' "ANGLE runtime preload must have a controlled manifest."
Assert-Contains $script 'ANGLE_RUNTIME_DIR' "ANGLE runtime source must support an explicit path."
Assert-Contains $script 'ANGLE_RUNTIME_SOURCE_SHA256' "ANGLE runtime source hash must be explicit for reproducible release packaging."
Assert-Contains $script 'ANGLE_RUNTIME_MANIFEST_SHA256' "ANGLE preload manifest hash must be explicit for reproducible release packaging."
Assert-Contains $script 'third_party\\angle' "ANGLE runtime source must support a repo-local vendored path."
Assert-Contains $script 'SCOPE_ALLOW_SYSTEM_ANGLE' "System ANGLE probing must be explicit opt-in."
Assert-Contains $script 'ANGLE runtime explicit path:' "ANGLE explicit path should be visible in release logs."
Assert-Contains $script 'ANGLE runtime system path:' "ANGLE system path should be visible in release logs when opted in."
Assert-Contains $script 'Write-AngleRuntimeManifest' "ANGLE runtime files must be recorded in a manifest."
Assert-Contains $script 'Assert-AngleRuntimePreload' "Signed and offline packaging must verify the controlled ANGLE preload."
Assert-Contains $script '\$anglePreloadManifestSha256ForStage\s*=\s*Assert-AngleRuntimePreload' "Packaging must invoke the ANGLE preload validation before copying DLLs."
Assert-Contains $script 'ANGLE runtime preload file hash mismatch' "ANGLE preload validation must hash-check copied DLLs."
Assert-Contains $script 'Mesa runtime explicit path does not pass the pinned manifest validation' "Signed and offline packaging must validate explicit Mesa runtimes."
Assert-Contains $script 'sourceArchiveSha256' "ANGLE runtime manifest must record the source asset hash."
Assert-Contains $script 'preloadManifestSha256' "ANGLE runtime manifest must record the controlled preload manifest hash."
Assert-Contains $script 'Write-BuildEvidence' "Package must emit reproducible build provenance."
Assert-Contains $script 'Write-ThirdPartyNotices' "Package must emit third-party notices."
Assert-Contains $script 'RequireSignature' "Release packaging must support a required signing gate."
Assert-Contains $script 'requires SCOPE_SIGN_CERT_SHA1 or -CertificateThumbprint' "Required signing must fail before an unsigned build starts."
Assert-Contains $script 'Get-AuthenticodeSignature' "Signed artifacts must be verified after signing."
Assert-Contains $script 'mesa\\ScopeAnalyzerMesa\.exe' "Mesa fallback executable must be included in signing coverage."
Assert-Contains $script 'release-evidence\.json' "Release packaging must archive artifact evidence."
Assert-Contains $script 'build-provenance\.json' "Build provenance must be packaged."
Assert-Contains $script 'sbom\.cdx\.json' "CycloneDX SBOM must be packaged."
Assert-Contains $script 'THIRD-PARTY-NOTICES\.txt' "Third-party license notices must be packaged."
Assert-Contains $script 'sourceCommit' "Build provenance must include the source commit."
Assert-Contains $script 'sourceDirty' "Release provenance must record source cleanliness."
Assert-Contains $script 'cargoLockSha256' "Build provenance must bind the dependency lockfile."
Assert-Contains $script 'toolchainSha256' "Build provenance must bind the Rust toolchain pin."
Assert-Contains $script 'fileHashes' "Build provenance must include staged file hashes."
Assert-Contains $script 'clean source tree with a known source commit' "Signed packaging must reject dirty or unknown source provenance."
Assert-Contains $script 'Signed packages must bundle the pinned ANGLE runtime' "Signed packaging must require bundled ANGLE evidence."
Assert-Contains $script 'Signed packages must bundle the pinned Mesa runtime' "Signed packaging must require bundled Mesa evidence."
Assert-Contains $script 'fileHashesExclude' "Build provenance must declare its self-hash exclusion."
Assert-Contains $script 'sbom.cdx.json' "Build provenance must cover the final SBOM artifact."
Assert-Contains $script 'self-referential' "Build provenance must document the self-hash exclusion."
Assert-Contains $script 'schemaVersion\s*=\s*1' "Runtime manifests must have a schema version."
Assert-NotContains $script 'sourcePath\s*=' "Runtime provenance must not include an absolute source path."
Assert-NotContains $script 'cachedAtUtc\s*=' "Runtime manifests must not include a build timestamp."
Assert-Contains $script 'Write-AngleRuntimeManifest\s+-RuntimeDir\s+\$angleRuntimeDir\s+-ManifestPath\s+\(Join-Path\s+\$stage\s+\$angleManifestName\)' "ANGLE runtime manifest must be written into the package stage."
Assert-Contains $wxs 'angle-runtime-manifest\.json' "ANGLE runtime manifest must be included in MSI packaging."
Assert-Contains $wxs 'build-provenance\.json' "Build provenance must be included in MSI packaging."
Assert-Contains $wxs 'sbom\.cdx\.json' "CycloneDX SBOM must be included in MSI packaging."
Assert-Contains $wxs 'THIRD-PARTY-NOTICES\.txt' "Third-party license notices must be included in MSI packaging."
Assert-Contains $wxs 'scope-cli\.exe' "CLI binary must be included in MSI packaging."
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
Assert-Contains $releaseCheck '--locked' "Release preflight must use the lockfile."
Assert-Contains $releaseCheck 'cargo\s+test\s+--quiet' "Release preflight must run normal tests."
Assert-Contains $releaseCheck 'run-performance-baselines\.ps1' "Release preflight must run ignored performance baselines."
Assert-Contains $releaseCheck 'test-package-windows\.ps1' "Release preflight must run packaging script checks."
Assert-Contains $releaseCheck 'windows-acceptance\.ps1' "Release preflight must parse the Windows acceptance script."
Assert-Contains $releaseCheck 'Assert-VersionSync' "Release preflight must verify release version sync."
Assert-Contains $releaseCheck 'CARGO_NET_OFFLINE' "Offline release preflight must propagate Cargo offline mode."

$coveragePath = Join-Path (Split-Path -Parent $PSScriptRoot) "scripts\coverage-check.py"
if (-not (Test-Path $coveragePath)) {
    throw "Route A core coverage gate must exist at scripts/coverage-check.py."
}
$coverage = Get-Content -Raw $coveragePath
Assert-Contains $coverage 'src/compare/mod\.rs' "Coverage gate must include Compare core."
Assert-Contains $coverage 'src/live/protocol\.rs' "Coverage gate must include SCP1 protocol."
Assert-Contains $coverage 'src/live/recording\.rs' "Coverage gate must include .scope recording."
Assert-Contains $coverage 'src/project\.rs' "Coverage gate must include project migration/validation."
Assert-Contains $coverage 'OVERALL_LINE_GATE\s*=\s*75\.0' "Coverage gate must enforce the 75% core overall target."
Assert-Contains $coverage '90\.0' "Coverage gate must enforce the 90% core module target."

if (-not (Test-Path $acceptancePath)) {
    throw "Windows acceptance script must exist at scripts/windows-acceptance.ps1."
}
$acceptance = Get-Content -Raw $acceptancePath
Assert-Contains $acceptance 'msiexec\.exe' "Windows acceptance must exercise MSI install and uninstall."
Assert-Contains $acceptance 'PreviousMsiPath' "Windows acceptance must support the upgrade matrix."
Assert-Contains $acceptance 'ReleaseEvidencePath' "Windows acceptance must verify final artifact evidence."
Assert-Contains $acceptance 'archives the stage contents directly' "Windows acceptance must handle a flat ZIP stage layout."
Assert-Contains $acceptance 'flatRoot' "Windows acceptance must inspect the flat ZIP root before subdirectories."
Assert-Contains $acceptance 'does not contain ScopeAnalyzer\.exe' "Windows acceptance must reject ZIPs without a package root."
Assert-Contains $acceptance 'SkipMsiLifecycle' "Windows acceptance must support a deployed-installation launch run."
Assert-Contains $acceptance 'RequireStandardUser' "Windows acceptance must verify a standard-user launch context."
Assert-Contains $acceptance 'RequireMesaRuntime' "Windows acceptance must be able to require bundled Mesa evidence."
Assert-Contains $acceptance 'RequireAngleRuntime' "Windows acceptance must be able to require bundled ANGLE evidence."
Assert-Contains $acceptance 'RequireRdpSession' "Windows acceptance must be able to require an RDP session."
Assert-Contains $acceptance 'RequireSignature' "Windows acceptance must be able to require Authenticode evidence."
Assert-Contains $acceptance 'ZIP Mesa ScopeAnalyzerMesa\.exe' "Windows acceptance must verify the bundled Mesa executable signature."
Assert-Contains $acceptance 'Installed Mesa ScopeAnalyzerMesa\.exe' "Windows acceptance must verify the installed Mesa executable signature."
Assert-Contains $acceptance 'installed-artifact-match' "Windows acceptance must compare installed executables with the packaged ZIP."
Assert-Contains $acceptance 'WindowsPrincipal' "Windows acceptance must inspect the current Windows token."
Assert-Contains $acceptance 'MSI lifecycle acceptance run must be started' "Windows acceptance must enforce an elevated lifecycle run."
Assert-Contains $acceptance 'wgpu-software' "Windows acceptance must smoke-test WARP/software rendering."
Assert-Contains $acceptance 'mesa-renderer' "Windows acceptance must record Mesa renderer evidence."
Assert-Contains $acceptance 'angle-renderer' "Windows acceptance must record ANGLE/EGL renderer evidence."
Assert-Contains $acceptance 'ScopeAnalyzer-startup\.log' "Renderer smoke must inspect the startup log."
Assert-Contains $acceptance 'starting renderer:' "Renderer smoke must assert the selected renderer."
Assert-Contains $acceptance 'timedOut' "Renderer smoke must allow a healthy GUI process to stay alive until startup is observed."
Assert-Contains $acceptance 'angle-manifest' "Windows acceptance must verify the bundled ANGLE manifest."
Assert-Contains $acceptance 'sourceArchiveSha256' "Windows acceptance must verify the ANGLE source asset hash."
Assert-Contains $acceptance 'preloadManifestSha256' "Windows acceptance must verify the ANGLE preload manifest hash declaration."
Assert-Contains $acceptance 'mesa-manifest' "Windows acceptance must verify the bundled Mesa manifest."
Assert-Contains $acceptance 'Assert-RuntimeManifestFiles' "Windows acceptance must verify runtime manifest file hashes."
Assert-Contains $acceptance 'Assert-ManifestContainsFiles' "Windows acceptance must require every runtime DLL in its manifest."
Assert-Contains $acceptance 'expectedMesaAssetSha256' "Windows acceptance must verify the pinned Mesa asset hash."
Assert-Contains $acceptance 'Windows 10 or newer' "Windows acceptance must enforce the supported Windows 10/11 family."
Assert-Contains $acceptance 'rdp-session' "Windows acceptance must record RDP evidence."
Assert-Contains $acceptance 'sbom\.cdx\.json' "Windows acceptance must verify the SBOM artifact."
Assert-Contains $acceptance 'THIRD-PARTY-NOTICES\.txt' "Windows acceptance must verify third-party notices."
Assert-Contains $acceptance 'Assert-BuildProvenance' "Windows acceptance must verify staged file hashes from build-provenance.json."
Assert-Contains $acceptance 'Compare-Object' "Windows acceptance must reject unrecorded or missing staged files."
Assert-Contains $acceptance 'sourceDirty' "Signed Windows acceptance must reject dirty-source provenance."
Assert-Contains $acceptance 'cargoLockSha256' "Windows acceptance must verify dependency lock provenance."
Assert-Contains $acceptance 'known source commit' "Signed Windows acceptance must reject unknown source commits."
Assert-Contains $acceptance 'Signed release evidence must be produced from a clean source tree' "Signed Windows acceptance must reject dirty release evidence."

if (-not (Test-Path $workflowPath)) {
    throw "Controlled Windows release acceptance workflow must exist at .github/workflows/windows-release-acceptance.yml."
}
$workflow = Get-Content -Raw $workflowPath
Assert-Contains $workflow 'workflow_dispatch' "Windows release acceptance must be manually triggerable."
Assert-Contains $workflow 'self-hosted, windows, x64, scope-release, rdp' "Windows release acceptance must target the controlled native runner."
Assert-Contains $workflow 'preloaded Rust toolchain' "Windows release acceptance must use a preloaded offline Rust toolchain."
Assert-NotContains $workflow 'actions/cache@v4' "Windows release acceptance must not depend on the online GitHub cache service."
Assert-Contains $workflow 'SCOPE_PACKAGE_OFFLINE' "Windows release acceptance must use offline packaging."
Assert-Contains $workflow 'CARGO_NET_OFFLINE' "Windows release acceptance must keep Cargo offline during preflight."
Assert-Contains $workflow 'MESA_RUNTIME_DIR' "Windows release acceptance must provide an external Mesa runtime path."
Assert-Contains $workflow 'ANGLE_RUNTIME_DIR' "Windows release acceptance must provide an external ANGLE runtime path."
Assert-Contains $workflow 'ANGLE_RUNTIME_SOURCE_SHA256' "Windows release acceptance must provide the pinned ANGLE source hash."
Assert-Contains $workflow 'ANGLE_RUNTIME_MANIFEST_SHA256' "Windows release acceptance must pin the controlled ANGLE preload manifest."
Assert-Contains $workflow 'Assert-ControlledRuntimeDir' "Windows release acceptance must reject runtimes inside the cleaned checkout."
Assert-Contains $workflow 'angle-runtime-preload-manifest\.json' "Windows release acceptance must require a controlled ANGLE preload manifest."
Assert-NotContains $workflow 'Join-Path \$env:GITHUB_WORKSPACE "target\\(mesa-runtime|angle-runtime)' "Windows release acceptance must not load preloaded runtimes from the cleaned workspace target directory."
Assert-Contains $workflow 'RequireSignature' "Windows release acceptance must require signed artifacts."
Assert-Contains $workflow 'RequireMesaRuntime' "Windows release acceptance must require Mesa evidence."
Assert-Contains $workflow 'RequireAngleRuntime' "Windows release acceptance must require ANGLE evidence."
Assert-Contains $workflow 'RequireRdpSession' "Windows release acceptance must require RDP evidence."
Assert-Contains $workflow 'sourceCommit' "Windows release acceptance must bind evidence to the checked-out source commit."
Assert-Contains $workflow 'sourceDirty' "Windows release acceptance must reject dirty release evidence."
Assert-Contains $workflow 'cargoLockSha256' "Windows release acceptance must bind the dependency lockfile hash."
Assert-Contains $workflow 'toolchainSha256' "Windows release acceptance must bind the toolchain hash."

$ciWorkflowPath = Join-Path (Split-Path -Parent $PSScriptRoot) ".github\workflows\ci.yml"
if (-not (Test-Path $ciWorkflowPath)) {
    throw "CI workflow must exist at .github/workflows/ci.yml."
}
$ciWorkflow = Get-Content -Raw $ciWorkflowPath
Assert-Contains $ciWorkflow 'rules-failing\.json' "CI must exercise a failing rule fixture."
Assert-Contains $ciWorkflow 'failing_exit' "CI must assert the failing-rule exit code."

if (-not (Test-Path $standardWorkflowPath)) {
    throw "Standard-user Windows acceptance workflow must exist at .github/workflows/windows-standard-user-acceptance.yml."
}
$standardWorkflow = Get-Content -Raw $standardWorkflowPath
Assert-Contains $standardWorkflow 'workflow_dispatch' "Standard-user acceptance must be manually triggerable."
Assert-Contains $standardWorkflow 'self-hosted, windows, x64, scope-standard-user, rdp' "Standard-user acceptance must target a non-elevated native runner."
Assert-Contains $standardWorkflow 'actions/download-artifact@v4' "Standard-user acceptance must consume administrator release evidence."
Assert-Contains $standardWorkflow 'SkipMsiLifecycle' "Standard-user acceptance must not mutate MSI lifecycle."
Assert-Contains $standardWorkflow 'RequireStandardUser' "Standard-user acceptance must require a non-elevated token."
Assert-Contains $standardWorkflow 'RequireSignature' "Standard-user acceptance must verify signed artifacts."
Assert-Contains $standardWorkflow 'sourceCommit' "Standard-user acceptance must bind evidence to the checked-out source commit."
Assert-Contains $standardWorkflow 'sourceDirty' "Standard-user acceptance must reject dirty release evidence."
Assert-Contains $standardWorkflow 'cargoLockSha256' "Standard-user acceptance must bind the dependency lockfile hash."
Assert-Contains $standardWorkflow 'toolchainSha256' "Standard-user acceptance must bind the toolchain hash."
Assert-Contains $standardWorkflow 'release-meta.outputs.head_sha' "Standard-user acceptance must check out the administrator run's source commit."

if (-not (Test-Path $hardwareWorkflowPath)) {
    throw "Hardware capture smoke workflow must exist at .github/workflows/windows-hardware-smoke.yml."
}
$hardwareWorkflow = Get-Content -Raw $hardwareWorkflowPath
Assert-Contains $hardwareWorkflow 'workflow_dispatch' "Hardware smoke must be manually triggerable."
Assert-Contains $hardwareWorkflow 'self-hosted, windows, x64, scope-hardware' "Hardware smoke must target a connected native runner."
Assert-Contains $hardwareWorkflow 'preloaded Rust toolchain' "Hardware smoke must use a preloaded Rust toolchain."
Assert-Contains $hardwareWorkflow 'cargo build --locked --offline' "Hardware smoke builds must honor offline dependency resolution."
Assert-Contains $hardwareWorkflow 'scope-hardware-smoke' "Hardware smoke must use the dedicated capture tool."
Assert-Contains $hardwareWorkflow '--serial-port' "Hardware smoke must exercise a physical serial transport."
Assert-Contains $hardwareWorkflow 'validate-recording' "Hardware smoke must validate the resulting .scope recording."
Assert-Contains $hardwareWorkflow 'firmwareName' "Hardware smoke evidence must include non-empty firmware identity."
Assert-Contains $hardwareWorkflow 'result.valid' "Hardware recording validation must require valid:true."
Assert-Contains $hardwareWorkflow 'recoveredTail' "Hardware recording validation must reject recovered tails."

$agents = Get-Content -Raw $agentsPath
Assert-Contains $agents 'scripts/release-check\.ps1' "AGENTS.md must tell future agents to run release preflight."
Assert-Contains $agents 'scripts/package-windows\.ps1 -OfflinePackage' "AGENTS.md must document the release packaging command."
Assert-Contains $agents 'ANGLE_RUNTIME_DIR' "AGENTS.md must document reproducible ANGLE input."
Assert-Contains $agents 'ANGLE_RUNTIME_SOURCE_SHA256' "AGENTS.md must document the pinned ANGLE source hash."
Assert-Contains $agents 'ANGLE_RUNTIME_MANIFEST_SHA256' "AGENTS.md must document the controlled ANGLE preload manifest hash."
Assert-Contains $agents 'SCOPE_PACKAGE_OFFLINE=1' "AGENTS.md must document offline reproducible packaging."

Write-Host "package-windows.ps1 reproducibility checks passed"
