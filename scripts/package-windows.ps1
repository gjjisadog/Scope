param(
    [switch]$SkipPerformanceBaselines,
    [switch]$OfflinePackage,
    [switch]$RequireSignature,
    [string]$CertificateThumbprint = $env:SCOPE_SIGN_CERT_SHA1
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$version = "0.15.0"
$dist = Join-Path $root "dist"
$stage = Join-Path $dist "ScopeAnalyzer-$version-win-x64"
$zip = Join-Path $dist "ScopeAnalyzer-$version-win-x64.zip"
$msi = Join-Path $dist "ScopeAnalyzer-$version-win-x64.msi"
$wixobj = Join-Path $dist "ScopeAnalyzer-$version-win-x64.wixobj"
$wxs = Join-Path $PSScriptRoot "ScopeAnalyzer.wxs"
$mesaReleaseTag = "26.0.8"
$mesaAssetName = "mesa3d-26.0.8-release-msvc.7z"
$mesaAssetSha256 = "a438c26c2752726916e455f9ad121f8a7e3cfecf8626251abf8e5b3e129d8497"
$sevenZipUrl = "https://www.7-zip.org/a/7zr.exe"
$sevenZipSha256 = "abcf64ae1cbafddb5395e4cdd3bdc7e3e0561d54a0c6380e3dd43bdbffe519a2"
$mesaManifestName = "mesa-runtime-manifest.json"
$angleManifestName = "angle-runtime-manifest.json"
$anglePreloadManifestName = "angle-runtime-preload-manifest.json"
$angleSourceSha256 = $env:ANGLE_RUNTIME_SOURCE_SHA256
$anglePreloadManifestSha256 = $env:ANGLE_RUNTIME_MANIFEST_SHA256
$wixToolsetVersion = "3.14"
$headers = @{ "User-Agent" = "ScopeAnalyzerPackageScript" }

if ($RequireSignature -and [string]::IsNullOrWhiteSpace($CertificateThumbprint)) {
    throw "-RequireSignature requires SCOPE_SIGN_CERT_SHA1 or -CertificateThumbprint."
}

function Resolve-Tool {
    param([string]$Name)

    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    $candidateDirs = @()
    if ($env:WIX) {
        $candidateDirs += $env:WIX
        $candidateDirs += Join-Path $env:WIX "bin"
    }
    if (${env:ProgramFiles(x86)}) {
        $candidateDirs += Join-Path ${env:ProgramFiles(x86)} "WiX Toolset v$wixToolsetVersion\bin"
    }
    $candidateDirs += "C:\tmp\wix314"

    foreach ($dir in $candidateDirs) {
        $path = Join-Path $dir $Name
        if (Test-Path $path) {
            return $path
        }
    }

    return $null
}

function Test-AngleRuntimeDir {
    param([string]$Dir)

    if (-not $Dir -or -not (Test-Path $Dir)) {
        return $false
    }

    return (Test-Path (Join-Path $Dir "libEGL.dll")) -and (Test-Path (Join-Path $Dir "libGLESv2.dll"))
}

function Resolve-AngleRuntimeCandidate {
    param([string]$Dir)

    if (Test-AngleRuntimeDir $Dir) {
        return $Dir
    }

    $x64Dir = Join-Path $Dir "x64"
    if (Test-AngleRuntimeDir $x64Dir) {
        return $x64Dir
    }

    return $null
}

function Assert-AngleRuntimePreload {
    param(
        [string]$RuntimeDir,
        [string]$ExpectedSourceSha256,
        [string]$ExpectedManifestSha256
    )

    if ([string]::IsNullOrWhiteSpace($ExpectedManifestSha256) -or
        $ExpectedManifestSha256 -notmatch '^[0-9a-fA-F]{64}$') {
        throw "ANGLE_RUNTIME_MANIFEST_SHA256 must be a 64-character hexadecimal SHA256 value for signed or offline packaging."
    }

    $manifestPath = Join-Path $RuntimeDir $anglePreloadManifestName
    if (-not (Test-Path $manifestPath)) {
        throw "ANGLE runtime preload manifest is required for signed or offline packaging: $manifestPath"
    }
    $actualManifestSha256 = Get-FileSha256 $manifestPath
    if ($actualManifestSha256 -ne $ExpectedManifestSha256.ToLowerInvariant()) {
        throw "ANGLE runtime preload manifest hash mismatch at $manifestPath. Expected $ExpectedManifestSha256, got $actualManifestSha256"
    }

    try {
        $manifest = Get-Content -Raw $manifestPath | ConvertFrom-Json
    } catch {
        throw "ANGLE runtime preload manifest is not valid JSON at $manifestPath"
    }
    if ($manifest.schemaVersion -ne 1 -or $manifest.runtime -ne "ANGLE") {
        throw "ANGLE runtime preload manifest must declare schemaVersion 1 and runtime ANGLE."
    }
    if ([string]$manifest.sourceArchiveSha256 -notmatch '^[0-9a-fA-F]{64}$' -or
        ([string]$manifest.sourceArchiveSha256).ToLowerInvariant() -ne $ExpectedSourceSha256.ToLowerInvariant()) {
        throw "ANGLE runtime preload manifest sourceArchiveSha256 does not match ANGLE_RUNTIME_SOURCE_SHA256."
    }
    if (-not $manifest.fileHashes) {
        throw "ANGLE runtime preload manifest does not contain fileHashes."
    }

    foreach ($name in @("libEGL.dll", "libGLESv2.dll", "d3dcompiler_47.dll")) {
        $path = Join-Path $RuntimeDir $name
        $entry = $manifest.fileHashes.PSObject.Properties[$name]
        if (Test-Path $path) {
            if (-not $entry) {
                throw "ANGLE runtime preload manifest does not hash packaged file $name."
            }
            if ([string]$entry.Value -notmatch '^[0-9a-fA-F]{64}$') {
                throw "ANGLE runtime preload manifest hash for $name is not a SHA256 value."
            }
            $actualFileSha256 = Get-FileSha256 $path
            if ($actualFileSha256 -ne ([string]$entry.Value).ToLowerInvariant()) {
                throw "ANGLE runtime preload file hash mismatch for $name."
            }
        } elseif ($entry) {
            throw "ANGLE runtime preload manifest hashes missing file $name."
        }
    }

    return $actualManifestSha256
}

function Resolve-SystemAngleRuntimeDir {
    $candidateDirs = @()
    if ($env:SystemRoot) {
        $candidateDirs += Join-Path $env:SystemRoot "System32\Microsoft-Edge-WebView"
    }
    if (${env:ProgramFiles(x86)}) {
        $candidateDirs += Join-Path ${env:ProgramFiles(x86)} "Microsoft\EdgeWebView\Application"
        $candidateDirs += Join-Path ${env:ProgramFiles(x86)} "Microsoft\Edge\Application"
    }
    if ($env:ProgramFiles) {
        $candidateDirs += Join-Path $env:ProgramFiles "Microsoft\Edge\Application"
    }

    foreach ($dir in $candidateDirs) {
        if (Test-AngleRuntimeDir $dir) {
            return $dir
        }

        if (-not (Test-Path $dir)) {
            continue
        }

        $versionDirs = Get-ChildItem -Path $dir -Directory -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending
        foreach ($versionDir in $versionDirs) {
            if (Test-AngleRuntimeDir $versionDir.FullName) {
                return $versionDir.FullName
            }
        }
    }

    return $null
}

function Test-MesaRuntimeDir {
    param([string]$Dir)

    if (-not $Dir -or -not (Test-Path $Dir)) {
        return $false
    }

    $required = @("opengl32.dll", "libgallium_wgl.dll", "libEGL.dll", "libGLESv2.dll", "libGLESv1_CM.dll")
    foreach ($name in $required) {
        if (-not (Test-Path (Join-Path $Dir $name))) {
            return $false
        }
    }

    return $true
}

function Resolve-MesaRuntimeCandidate {
    param([string]$Dir)

    if (Test-MesaRuntimeDir $Dir) {
        return $Dir
    }

    $x64Dir = Join-Path $Dir "x64"
    if (Test-MesaRuntimeDir $x64Dir) {
        return $x64Dir
    }

    return $null
}

function Resolve-AngleRuntimeDir {
    if ($env:ANGLE_RUNTIME_DIR) {
        $resolved = Resolve-AngleRuntimeCandidate $env:ANGLE_RUNTIME_DIR
        if ($resolved) {
            Write-Host "ANGLE runtime explicit path: $resolved"
            return $resolved
        }
        Write-Warning "ANGLE_RUNTIME_DIR was set but does not contain libEGL.dll and libGLESv2.dll: $env:ANGLE_RUNTIME_DIR"
    }

    foreach ($dir in @(
        (Join-Path $root "third_party\angle"),
        (Join-Path $root "target\angle-runtime")
    )) {
        $resolved = Resolve-AngleRuntimeCandidate $dir
        if ($resolved) {
            Write-Host "ANGLE runtime explicit path: $resolved"
            return $resolved
        }
    }

    if ($env:SCOPE_ALLOW_SYSTEM_ANGLE -eq "1") {
        $resolvedSystem = Resolve-SystemAngleRuntimeDir
        if ($resolvedSystem) {
            Write-Host "ANGLE runtime system path: $resolvedSystem"
            return $resolvedSystem
        }
    } else {
        Write-Host "ANGLE runtime cache miss: set ANGLE_RUNTIME_DIR or populate third_party\angle to bundle ANGLE. Set SCOPE_ALLOW_SYSTEM_ANGLE=1 to opt in to build-machine Edge/WebView probing."
    }

    return $null
}

function Get-IsOfflinePackage {
    return $OfflinePackage -or $env:SCOPE_PACKAGE_OFFLINE -eq "1"
}

function Get-CommandVersion {
    param([string]$Name)

    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if (-not $command) {
        return "unavailable"
    }
    try {
        $text = & $command.Source --version 2>$null | Select-Object -First 1
        if ($text) {
            return $text.ToString().Trim()
        }
    } catch {
    }
    return "unavailable"
}

function Get-RelativeStagePath {
    param(
        [string]$StageDir,
        [string]$Path
    )

    return $Path.Substring($StageDir.Length).TrimStart('\', '/') -replace '\\', '/'
}

function Write-BuildEvidence {
    param(
        [string]$StageDir,
        [string]$Version,
        [string]$IncludeAngle,
        [string]$IncludeMesa
    )

    $commit = "unknown"
    $dirty = $false
    try {
        $commit = (& git -C $root rev-parse HEAD 2>$null | Select-Object -First 1).ToString().Trim()
        $dirty = [bool](& git -C $root status --porcelain 2>$null)
    } catch {
    }
    if ($RequireSignature -and ($commit -eq "unknown" -or $dirty)) {
        throw "Signed packages must be produced from a clean source tree with a known source commit"
    }
    $cargoLockSha256 = (Get-FileHash -Path (Join-Path $root "Cargo.lock") -Algorithm SHA256).Hash.ToLowerInvariant()
    $toolchainSha256 = (Get-FileHash -Path (Join-Path $root "rust-toolchain.toml") -Algorithm SHA256).Hash.ToLowerInvariant()

    $fileHashes = [ordered]@{}
    Get-ChildItem -Path $StageDir -Recurse -File |
        Sort-Object FullName |
        ForEach-Object {
            $relative = Get-RelativeStagePath -StageDir $StageDir -Path $_.FullName
            $fileHashes[$relative] = (Get-FileHash -Path $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        }

    $runtimeManifests = @()
    foreach ($manifestPath in @(
        (Join-Path $StageDir $angleManifestName),
        (Join-Path $StageDir "mesa\$mesaManifestName")
    )) {
        if (Test-Path $manifestPath) {
            $runtimeManifests += [ordered]@{
                path = Get-RelativeStagePath -StageDir $StageDir -Path $manifestPath
                sha256 = (Get-FileHash -Path $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        }
    }

    $provenance = [ordered]@{
        schemaVersion = 1
        packageVersion = $Version
        sourceCommit = $commit
        sourceDirty = $dirty
        cargoLockSha256 = $cargoLockSha256
        toolchainSha256 = $toolchainSha256
        rustc = Get-CommandVersion "rustc"
        cargo = Get-CommandVersion "cargo"
        wix = Get-CommandVersion "candle.exe"
        includeAngleRuntime = ($IncludeAngle -eq "true")
        includeMesaRuntime = ($IncludeMesa -eq "true")
        runtimeManifests = $runtimeManifests
        fileHashesExclude = @("build-provenance.json")
        fileHashes = $fileHashes
    }
    $provenancePath = Join-Path $StageDir "build-provenance.json"
    $provenance | ConvertTo-Json -Depth 8 | Set-Content -Path $provenancePath -Encoding ASCII

    $components = @(
        [ordered]@{
            type = "application"
            name = "scope-analyzer"
            version = $Version
            "bom-ref" = "scope-analyzer@$Version"
        },
        [ordered]@{
            type = "application"
            name = "scope-cli"
            version = $Version
            "bom-ref" = "scope-cli@$Version"
        }
    )
    if ($IncludeAngle -eq "true") {
        $components += [ordered]@{
            type = "library"
            name = "ANGLE runtime"
            version = "manifest-pinned"
            "bom-ref" = "angle-runtime"
            licenses = @(
                [ordered]@{
                    license = [ordered]@{ id = "BSD-3-Clause" }
                }
            )
        }
    }
    if ($IncludeMesa -eq "true") {
        $components += [ordered]@{
            type = "library"
            name = "Mesa runtime"
            version = $mesaReleaseTag
            "bom-ref" = "mesa-runtime@$mesaReleaseTag"
            licenses = @(
                [ordered]@{
                    license = [ordered]@{ name = "MIT/X11 and Mesa component licenses" }
                }
            )
        }
    }
    $sbom = [ordered]@{
        bomFormat = "CycloneDX"
        specVersion = "1.5"
        serialNumber = "urn:scope-analyzer:$Version"
        version = 1
        metadata = [ordered]@{
            component = [ordered]@{
                type = "application"
                name = "Scope Analyzer"
                version = $Version
            }
        }
        components = $components
    }
    $sbom | ConvertTo-Json -Depth 8 | Set-Content -Path (Join-Path $StageDir "sbom.cdx.json") -Encoding ASCII

    # The provenance file cannot contain its own hash without becoming
    # self-referential. Recompute the final manifest after SBOM generation so
    # every other staged artifact, including sbom.cdx.json, is covered.
    $fileHashes = [ordered]@{}
    Get-ChildItem -Path $StageDir -Recurse -File |
        Sort-Object FullName |
        ForEach-Object {
            $relative = Get-RelativeStagePath -StageDir $StageDir -Path $_.FullName
            if ($relative -ne "build-provenance.json") {
                $fileHashes[$relative] = (Get-FileHash -Path $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        }
    $provenance.fileHashes = $fileHashes
    $provenance | ConvertTo-Json -Depth 8 | Set-Content -Path $provenancePath -Encoding ASCII
}

function Write-ThirdPartyNotices {
    param([string]$StageDir)

    $notice = @"
Scope Analyzer third-party runtime notices
==========================================

This package may contain the following redistributable runtime components. The
runtime manifests next to these files record the exact copied file hashes.

Mesa 26.0.8 (llvmpipe/WGL fallback)
  Project: https://mesa3d.org/
  License family: MIT/X11 and the component licenses documented by Mesa.
  Release asset: $mesaAssetName
  Release asset SHA256: $mesaAssetSha256

ANGLE (EGL/GLES translation runtime)
  Project: https://chromium.googlesource.com/angle/angle
  License: BSD 3-Clause and the dependent notices distributed by ANGLE.
  The package angle-runtime-manifest.json records the copied DLL hashes.

7-Zip command-line extractor (build-time helper only)
  Project: https://www.7-zip.org/
  License: LGPL-2.1-or-later with the 7-Zip unRAR restriction where applicable.
  7zr.exe is not shipped in the installed application.

The Scope Analyzer application and its CLI are distributed under the repository
license. See the source repository for the complete dependency license report.
"@
    Set-Content -Path (Join-Path $StageDir "THIRD-PARTY-NOTICES.txt") -Encoding UTF8 -Value $notice
}

function Resolve-SignTool {
    if ($env:SIGNTOOL_PATH -and (Test-Path $env:SIGNTOOL_PATH)) {
        return $env:SIGNTOOL_PATH
    }
    return Resolve-Tool "signtool.exe"
}

function Sign-ReleaseFile {
    param(
        [string]$Path,
        [string]$SignTool
    )

    & $SignTool sign /fd SHA256 /sha1 $CertificateThumbprint $Path
    if ($LASTEXITCODE -ne 0) {
        throw "signtool failed for $Path with exit code $LASTEXITCODE"
    }
    $signature = Get-AuthenticodeSignature -FilePath $Path
    if ($signature.Status -ne "Valid") {
        throw "Authenticode signature validation failed for ${Path}: $($signature.Status)"
    }
}

function Write-ReleaseEvidence {
    param(
        [string]$MsiPath,
        [string]$ZipPath,
        [string]$StageDir,
        [string]$SignatureStatus
    )

    $artifacts = [ordered]@{}
    foreach ($path in @($MsiPath, $ZipPath)) {
        if (Test-Path $path) {
            $artifacts[(Split-Path -Leaf $path)] = [ordered]@{
                sha256 = (Get-FileHash -Path $path -Algorithm SHA256).Hash.ToLowerInvariant()
                bytes = (Get-Item $path).Length
            }
        }
    }
    $commit = "unknown"
    $dirty = $false
    try {
        $commitValue = & git -C $root rev-parse HEAD 2>$null | Select-Object -First 1
        if ($commitValue) {
            $commit = $commitValue.ToString().Trim()
        }
        $dirty = [bool](& git -C $root status --porcelain 2>$null)
    } catch {
    }
    $evidence = [ordered]@{
        schemaVersion = 1
        packageVersion = $version
        sourceCommit = $commit
        sourceDirty = $dirty
        cargoLockSha256 = (Get-FileHash -Path (Join-Path $root "Cargo.lock") -Algorithm SHA256).Hash.ToLowerInvariant()
        toolchainSha256 = (Get-FileHash -Path (Join-Path $root "rust-toolchain.toml") -Algorithm SHA256).Hash.ToLowerInvariant()
        stagePath = Split-Path -Leaf $StageDir
        signatureStatus = $SignatureStatus
        artifacts = $artifacts
        generatedBy = "scripts/package-windows.ps1"
    }
    $evidence | ConvertTo-Json -Depth 8 | Set-Content -Path (Join-Path $dist "release-evidence.json") -Encoding ASCII
}

function Get-FileSha256 {
    param([string]$Path)

    if (-not (Test-Path $Path)) {
        return $null
    }

    return (Get-FileHash -Path $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Test-FileSha256 {
    param(
        [string]$Path,
        [string]$ExpectedSha256
    )

    $actual = Get-FileSha256 $Path
    return $actual -and $actual -eq $ExpectedSha256.ToLowerInvariant()
}

function Assert-FileSha256 {
    param(
        [string]$Path,
        [string]$ExpectedSha256,
        [string]$Description
    )

    if (-not (Test-FileSha256 -Path $Path -ExpectedSha256 $ExpectedSha256)) {
        $actual = Get-FileSha256 $Path
        if (-not $actual) {
            throw "$Description was not found at $Path"
        }
        throw "$Description hash mismatch at $Path. Expected $ExpectedSha256, got $actual"
    }
}

function Save-PinnedDownload {
    param(
        [string]$Uri,
        [string]$Path,
        [string]$ExpectedSha256,
        [string]$Description,
        [hashtable]$Headers
    )

    if (Test-FileSha256 -Path $Path -ExpectedSha256 $ExpectedSha256) {
        Write-Host "$Description cache hit: $Path"
        return
    }

    if (Test-Path $Path) {
        if (Get-IsOfflinePackage) {
            Assert-FileSha256 -Path $Path -ExpectedSha256 $ExpectedSha256 -Description $Description
        }
        Write-Host "$Description cache miss: removing hash-mismatched $Path"
        Remove-Item -Force $Path
    } else {
        Write-Host "$Description cache miss: $Path"
    }

    if (Get-IsOfflinePackage) {
        throw "$Description is missing and SCOPE_PACKAGE_OFFLINE=1/-OfflinePackage forbids downloads."
    }

    if ($Headers) {
        Invoke-WebRequest -Uri $Uri -Headers $Headers -OutFile $Path
    } else {
        Invoke-WebRequest -Uri $Uri -OutFile $Path
    }
    Assert-FileSha256 -Path $Path -ExpectedSha256 $ExpectedSha256 -Description $Description
}

function Get-MesaManifestPath {
    param([string]$RuntimeDir)

    return Join-Path $RuntimeDir $mesaManifestName
}

function Test-MesaRuntimeCache {
    param([string]$RuntimeDir)

    if (-not (Test-MesaRuntimeDir $RuntimeDir)) {
        Write-Host "Mesa runtime cache miss: required DLLs are missing from $RuntimeDir"
        return $false
    }

    $manifestPath = Get-MesaManifestPath $RuntimeDir
    if (-not (Test-Path $manifestPath)) {
        Write-Host "Mesa runtime cache miss: manifest not found at $manifestPath"
        return $false
    }

    try {
        $manifest = Get-Content -Raw $manifestPath | ConvertFrom-Json
    } catch {
        Write-Host "Mesa runtime cache miss: manifest is not valid JSON at $manifestPath"
        return $false
    }

    if ($manifest.mesaReleaseTag -ne $mesaReleaseTag -or
        $manifest.mesaAssetName -ne $mesaAssetName -or
        $manifest.mesaAssetSha256 -ne $mesaAssetSha256) {
        Write-Host "Mesa runtime cache miss: manifest pins $($manifest.mesaReleaseTag)/$($manifest.mesaAssetName), expected $mesaReleaseTag/$mesaAssetName"
        return $false
    }

    if (-not $manifest.fileHashes) {
        Write-Host "Mesa runtime cache miss: manifest does not contain file hashes"
        return $false
    }
    if ($manifest.sourceArchiveSha256 -notmatch '^[0-9a-fA-F]{64}$') {
        Write-Host "Mesa runtime cache miss: manifest sourceArchiveSha256 is not a SHA256 value"
        return $false
    }
    foreach ($name in @("opengl32.dll", "libgallium_wgl.dll", "libEGL.dll", "libGLESv2.dll", "libGLESv1_CM.dll")) {
        if (-not $manifest.fileHashes.PSObject.Properties[$name]) {
            Write-Host "Mesa runtime cache miss: manifest does not hash required file $name"
            return $false
        }
    }

    foreach ($file in $manifest.fileHashes.PSObject.Properties) {
        if ([string]$file.Value -notmatch '^[0-9a-fA-F]{64}$') {
            Write-Host "Mesa runtime cache miss: hash for $($file.Name) is not a SHA256 value"
            return $false
        }
        $path = Join-Path $RuntimeDir $file.Name
        if (-not (Test-FileSha256 -Path $path -ExpectedSha256 $file.Value)) {
            Write-Host "Mesa runtime cache miss: cached file hash mismatch for $($file.Name)"
            return $false
        }
    }

    Write-Host "Mesa runtime cache hit: $RuntimeDir ($mesaReleaseTag/$mesaAssetName)"
    return $true
}

function Write-MesaRuntimeManifest {
    param(
        [string]$RuntimeDir,
        [string]$Archive
    )

    $fileHashes = [ordered]@{}
    foreach ($name in @("opengl32.dll", "libgallium_wgl.dll", "libEGL.dll", "libGLESv2.dll", "libGLESv1_CM.dll")) {
        $path = Join-Path $RuntimeDir $name
        if (Test-Path $path) {
            $fileHashes[$name] = Get-FileSha256 $path
        }
    }

    $manifest = [ordered]@{
        schemaVersion = 1
        mesaReleaseTag = $mesaReleaseTag
        mesaAssetName = $mesaAssetName
        mesaAssetSha256 = $mesaAssetSha256
        sourceArchiveSha256 = Get-FileSha256 $Archive
        fileHashes = $fileHashes
    }

    $manifest | ConvertTo-Json -Depth 4 | Set-Content -Path (Get-MesaManifestPath $RuntimeDir) -Encoding ASCII
}

function Write-AngleRuntimeManifest {
    param(
        [string]$RuntimeDir,
        [string]$ManifestPath,
        [string]$SourceSha256,
        [string]$PreloadManifestSha256
    )

    $fileHashes = [ordered]@{}
    foreach ($name in @("libEGL.dll", "libGLESv2.dll", "d3dcompiler_47.dll")) {
        $path = Join-Path $RuntimeDir $name
        if (Test-Path $path) {
            $fileHashes[$name] = Get-FileSha256 $path
        }
    }

    $manifest = [ordered]@{
        schemaVersion = 1
        runtime = "ANGLE"
        sourceArchiveSha256 = $SourceSha256
        preloadManifestSha256 = $PreloadManifestSha256
        fileHashes = $fileHashes
    }

    $manifest | ConvertTo-Json -Depth 4 | Set-Content -Path $ManifestPath -Encoding ASCII
}

function Install-MesaRuntime {
    $cacheDir = Join-Path $root "target\mesa-runtime"
    $runtimeDir = Join-Path $cacheDir "x64"
    if (Test-MesaRuntimeCache $runtimeDir) {
        return $runtimeDir
    }

    if (Get-IsOfflinePackage) {
        throw "Mesa runtime cache is missing or stale and SCOPE_PACKAGE_OFFLINE=1/-OfflinePackage forbids downloads. Preload $runtimeDir with $mesaManifestName or set MESA_RUNTIME_DIR/third_party\mesa."
    }

    New-Item -ItemType Directory -Force -Path $cacheDir | Out-Null
    Write-Host "Mesa runtime cache miss: downloading pinned $mesaReleaseTag/$mesaAssetName"
    $assetUrl = "https://github.com/pal1000/mesa-dist-win/releases/download/$mesaReleaseTag/$mesaAssetName"
    $archive = Join-Path $cacheDir $mesaAssetName
    Save-PinnedDownload -Uri $assetUrl -Path $archive -ExpectedSha256 $mesaAssetSha256 -Description "Mesa runtime archive" -Headers $headers

    $sevenZip = Join-Path $cacheDir "7zr.exe"
    Save-PinnedDownload -Uri $sevenZipUrl -Path $sevenZip -ExpectedSha256 $sevenZipSha256 -Description "7-Zip extractor" -Headers $null

    $extractDir = Join-Path $cacheDir "extract"
    if (Test-Path $extractDir) {
        Remove-Item -Recurse -Force $extractDir
    }
    New-Item -ItemType Directory -Force -Path $extractDir | Out-Null

    & $sevenZip x $archive "-o$extractDir" -y | Out-Host
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "Failed to extract Mesa runtime archive."
        return $null
    }

    $sourceDir = Resolve-MesaRuntimeCandidate $extractDir
    if (-not $sourceDir) {
        $sourceDir = Get-ChildItem -Path $extractDir -Directory -Recurse -ErrorAction SilentlyContinue |
            ForEach-Object { Resolve-MesaRuntimeCandidate $_.FullName } |
            Where-Object { $_ } |
            Select-Object -First 1
    }
    if (-not $sourceDir) {
        Write-Warning "Mesa runtime DLLs were not found after extraction."
        return $null
    }

    New-Item -ItemType Directory -Force -Path $runtimeDir | Out-Null
    foreach ($name in @("opengl32.dll", "libgallium_wgl.dll", "libEGL.dll", "libGLESv2.dll", "libGLESv1_CM.dll")) {
        if (-not (Test-Path (Join-Path $sourceDir $name))) {
            continue
        }
        Copy-Item (Join-Path $sourceDir $name) (Join-Path $runtimeDir $name) -Force
    }

    Write-MesaRuntimeManifest -RuntimeDir $runtimeDir -Archive $archive
    if (-not (Test-MesaRuntimeCache $runtimeDir)) {
        throw "Mesa runtime cache failed validation after extraction."
    }

    return $runtimeDir
}

function Resolve-MesaRuntimeDir {
    if ($env:MESA_RUNTIME_DIR) {
        $resolved = Resolve-MesaRuntimeCandidate $env:MESA_RUNTIME_DIR
        if ($resolved) {
            if (Test-MesaRuntimeCache $resolved) {
                Write-Host "Mesa runtime explicit path: $resolved"
                return $resolved
            }
            if ($RequireSignature -or (Get-IsOfflinePackage)) {
                throw "Mesa runtime explicit path does not pass the pinned manifest validation: $resolved"
            }
            Write-Warning "Mesa runtime explicit path is not pinned by a valid manifest: $resolved"
            return $resolved
        }
    }

    $thirdPartyDir = Join-Path $root "third_party\mesa"
    $resolvedThirdParty = Resolve-MesaRuntimeCandidate $thirdPartyDir
    if ($resolvedThirdParty) {
        if (Test-MesaRuntimeCache $resolvedThirdParty) {
            Write-Host "Mesa runtime explicit path: $resolvedThirdParty"
            return $resolvedThirdParty
        }
        if ($RequireSignature -or (Get-IsOfflinePackage)) {
            throw "Mesa runtime explicit path does not pass the pinned manifest validation: $resolvedThirdParty"
        }
        Write-Warning "Mesa runtime explicit path is not pinned by a valid manifest: $resolvedThirdParty"
        return $resolvedThirdParty
    }

    $cacheRuntimeDir = Join-Path $root "target\mesa-runtime\x64"
    if (Test-MesaRuntimeCache $cacheRuntimeDir) {
        return $cacheRuntimeDir
    }

    if ($env:SCOPE_SKIP_MESA_DOWNLOAD -eq "1") {
        Write-Warning "Skipping Mesa download because SCOPE_SKIP_MESA_DOWNLOAD=1."
        return $null
    }

    return Install-MesaRuntime
}

New-Item -ItemType Directory -Force -Path $dist | Out-Null
if (Test-Path $stage) {
    Remove-Item -Recurse -Force $stage
}
if (Test-Path $zip) {
    Remove-Item -Force $zip
}
if (Test-Path $msi) {
    Remove-Item -Force $msi
}
if (Test-Path $wixobj) {
    Remove-Item -Force $wixobj
}
New-Item -ItemType Directory -Force -Path $stage | Out-Null

$includeLibUnwind = "false"
$includeAngleRuntime = "false"
$includeD3DCompiler = "false"
$includeMesaRuntime = "false"

Push-Location $root
try {
    if (-not $SkipPerformanceBaselines) {
        & (Join-Path $PSScriptRoot "run-performance-baselines.ps1")
        if ($LASTEXITCODE -ne 0) {
            throw "performance baseline gate failed with exit code $LASTEXITCODE"
        }
    } else {
        Write-Warning "Skipping performance baseline gate. Do not use this for release builds."
    }

    $linkLibDir = Join-Path $root "target\package-link-libs"
    New-Item -ItemType Directory -Force -Path $linkLibDir | Out-Null
    $shlwapiImportLib = Get-ChildItem -Path (Join-Path $env:USERPROFILE ".cargo\registry\src") -Recurse -Filter "libwinapi_shlwapi.a" -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -like "*winapi-x86_64-pc-windows-gnu-0.4.0*" } |
        Select-Object -First 1
    if ($shlwapiImportLib) {
        Copy-Item $shlwapiImportLib.FullName (Join-Path $linkLibDir "libshlwapi.a") -Force
    }

    $cargoArgs = @("rustc", "--release", "--locked", "--bin", "scope_analyzer")
    if (Get-IsOfflinePackage) {
        $cargoArgs += "--offline"
    }
    & cargo @cargoArgs -- -L "native=$linkLibDir"
    if ($LASTEXITCODE -ne 0) {
        throw "cargo rustc --release failed with exit code $LASTEXITCODE"
    }
    $cargoCliArgs = @("build", "--release", "--locked", "--bin", "scope-cli")
    if (Get-IsOfflinePackage) {
        $cargoCliArgs += "--offline"
    }
    & cargo @cargoCliArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build --release --bin scope-cli failed with exit code $LASTEXITCODE"
    }
    Copy-Item "target\release\scope_analyzer.exe" (Join-Path $stage "ScopeAnalyzer.exe")
    Copy-Item "target\release\scope-cli.exe" (Join-Path $stage "scope-cli.exe")
    Copy-Item "resources\ScopeAnalyzer.ico" (Join-Path $stage "ScopeAnalyzer.ico")
    Copy-Item "README.md" (Join-Path $stage "README.txt")
    $angleRuntimeDir = Resolve-AngleRuntimeDir
    if ($angleRuntimeDir) {
        if (($RequireSignature -or (Get-IsOfflinePackage)) -and
            [string]::IsNullOrWhiteSpace($angleSourceSha256)) {
            throw "ANGLE_RUNTIME_SOURCE_SHA256 is required when a signed or offline package bundles ANGLE."
        }
        if (-not [string]::IsNullOrWhiteSpace($angleSourceSha256) -and
            $angleSourceSha256 -notmatch '^[0-9a-fA-F]{64}$') {
            throw "ANGLE_RUNTIME_SOURCE_SHA256 must be a 64-character hexadecimal SHA256 value."
        }
        $anglePreloadManifestSha256ForStage = $null
        if ($RequireSignature -or (Get-IsOfflinePackage)) {
            $anglePreloadManifestSha256ForStage = Assert-AngleRuntimePreload `
                -RuntimeDir $angleRuntimeDir `
                -ExpectedSourceSha256 $angleSourceSha256 `
                -ExpectedManifestSha256 $anglePreloadManifestSha256
        }
        Copy-Item (Join-Path $angleRuntimeDir "libEGL.dll") (Join-Path $stage "libEGL.dll") -Force
        Copy-Item (Join-Path $angleRuntimeDir "libGLESv2.dll") (Join-Path $stage "libGLESv2.dll") -Force
        $includeAngleRuntime = "true"

        $d3dCompiler = Join-Path $angleRuntimeDir "d3dcompiler_47.dll"
        if (Test-Path $d3dCompiler) {
            Copy-Item $d3dCompiler (Join-Path $stage "d3dcompiler_47.dll") -Force
            $includeD3DCompiler = "true"
        }
        $angleManifestSourceSha256 = if ([string]::IsNullOrWhiteSpace($angleSourceSha256)) {
            $null
        } else {
            $angleSourceSha256.ToLowerInvariant()
        }
        Write-AngleRuntimeManifest -RuntimeDir $angleRuntimeDir -ManifestPath (Join-Path $stage $angleManifestName) `
            -SourceSha256 $angleManifestSourceSha256 `
            -PreloadManifestSha256 $anglePreloadManifestSha256ForStage
    } else {
        Write-Warning "ANGLE runtime DLLs were not bundled. The installed app will rely on the target machine's OpenGL/ANGLE installation."
        if ($RequireSignature) {
            throw "Signed packages must bundle the pinned ANGLE runtime."
        }
    }
    $mesaRuntimeDir = Resolve-MesaRuntimeDir
    if ($mesaRuntimeDir) {
        $mesaStage = Join-Path $stage "mesa"
        New-Item -ItemType Directory -Force -Path $mesaStage | Out-Null
        Copy-Item "target\release\scope_analyzer.exe" (Join-Path $mesaStage "ScopeAnalyzerMesa.exe") -Force
        Set-Content -Path (Join-Path $mesaStage "ScopeAnalyzerMesa.exe.local") -Encoding ASCII -Value "local"
        foreach ($name in @("opengl32.dll", "libgallium_wgl.dll", "libEGL.dll", "libGLESv2.dll", "libGLESv1_CM.dll")) {
            if (-not (Test-Path (Join-Path $mesaRuntimeDir $name))) {
                continue
            }
            Copy-Item (Join-Path $mesaRuntimeDir $name) (Join-Path $mesaStage $name) -Force
        }
        $mesaManifestPath = Get-MesaManifestPath $mesaRuntimeDir
        if (-not (Test-Path $mesaManifestPath)) {
            throw "Mesa runtime manifest is required for reproducible packaging: $mesaManifestPath"
        }
        Copy-Item $mesaManifestPath (Join-Path $mesaStage $mesaManifestName) -Force
        $includeMesaRuntime = "true"
    } else {
        Write-Warning "Mesa runtime DLLs were not found. Mesa/llvmpipe fallback will be unavailable in this package."
        if ($RequireSignature) {
            throw "Signed packages must bundle the pinned Mesa runtime."
        }
    }
Set-Content -Path (Join-Path $stage "Start-ScopeAnalyzer.bat") -Encoding ASCII -Value @(
    "@echo off",
    "cd /d ""%~dp0""",
    "start """" ""%~dp0ScopeAnalyzer.exe"""
)
Set-Content -Path (Join-Path $stage "Start-ScopeAnalyzer-Mesa.bat") -Encoding ASCII -Value @(
    "@echo off",
    "cd /d ""%~dp0""",
    "if exist ""%~dp0mesa\ScopeAnalyzerMesa.exe"" (",
    "  set SCOPE_RENDERER=mesa",
    "  set SCOPE_APP_HOME=%~dp0",
    "  set SCOPE_GL_API=wgl",
    "  start """" ""%~dp0mesa\ScopeAnalyzerMesa.exe""",
    ") else (",
    "  echo Mesa runtime was not packaged.",
    "  pause",
    ")"
)

$clang = Get-Command "x86_64-w64-mingw32-clang.exe" -ErrorAction SilentlyContinue
    if ($clang) {
        $runtimeDir = Split-Path -Parent $clang.Source
        $libunwind = Join-Path $runtimeDir "libunwind.dll"
        if (Test-Path $libunwind) {
            Copy-Item $libunwind (Join-Path $stage "libunwind.dll")
            if ($includeMesaRuntime -eq "true") {
                Copy-Item $libunwind (Join-Path $stage "mesa\libunwind.dll")
            }
            $includeLibUnwind = "true"
        }
    }
} finally {
    Pop-Location
}

Write-ThirdPartyNotices -StageDir $stage
$signTool = Resolve-SignTool
$signatureStatus = "not-requested"
if (-not [string]::IsNullOrWhiteSpace($CertificateThumbprint)) {
    if (-not $signTool) {
        throw "SCOPE_SIGN_CERT_SHA1 was supplied but signtool.exe was not found. Set SIGNTOOL_PATH or add signtool.exe to PATH."
    }
    $signableBinaries = @(
        (Join-Path $stage "ScopeAnalyzer.exe"),
        (Join-Path $stage "scope-cli.exe")
    )
    $mesaExecutable = Join-Path $stage "mesa\ScopeAnalyzerMesa.exe"
    if (Test-Path $mesaExecutable) {
        $signableBinaries += $mesaExecutable
    }
    foreach ($binary in $signableBinaries) {
        Sign-ReleaseFile -Path $binary -SignTool $signTool
    }
    $signatureStatus = "stage-binaries-valid"
} elseif ($RequireSignature) {
    throw "-RequireSignature requires SCOPE_SIGN_CERT_SHA1 or -CertificateThumbprint."
}
Write-BuildEvidence -StageDir $stage -Version $version -IncludeAngle $includeAngleRuntime -IncludeMesa $includeMesaRuntime
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip -Force
Write-Host "Created $zip"

$candle = Resolve-Tool "candle.exe"
$light = Resolve-Tool "light.exe"
if ($candle -and $light) {
    & $candle -arch x64 -dStageDir="$stage" -dIncludeLibUnwind="$includeLibUnwind" -dIncludeAngleRuntime="$includeAngleRuntime" -dIncludeD3DCompiler="$includeD3DCompiler" -dIncludeMesaRuntime="$includeMesaRuntime" -out $wixobj $wxs
    if ($LASTEXITCODE -ne 0) {
        throw "candle failed with exit code $LASTEXITCODE"
    }

    & $light -spdb -sval -out $msi $wixobj
    if ($LASTEXITCODE -ne 0) {
        throw "light failed with exit code $LASTEXITCODE"
    }

    if (-not [string]::IsNullOrWhiteSpace($CertificateThumbprint)) {
        Sign-ReleaseFile -Path $msi -SignTool $signTool
        $signatureStatus = "msi-and-stage-binaries-valid"
    }
    Write-ReleaseEvidence -MsiPath $msi -ZipPath $zip -StageDir $stage -SignatureStatus $signatureStatus

    Write-Host "Created $msi"
    return
}

throw "WiX Toolset v3 was not found. Install WiX v3 and make candle.exe/light.exe available on PATH to create the MSI."
