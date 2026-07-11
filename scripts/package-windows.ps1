param(
    [switch]$SkipPerformanceBaselines,
    [switch]$OfflinePackage
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$version = "0.8.0"
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
$headers = @{ "User-Agent" = "ScopeAnalyzerPackageScript" }

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
        $candidateDirs += Join-Path ${env:ProgramFiles(x86)} "WiX Toolset v3.14\bin"
        $candidateDirs += Join-Path ${env:ProgramFiles(x86)} "WiX Toolset v3.11\bin"
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

    $required = @("opengl32.dll", "libgallium_wgl.dll", "libEGL.dll", "libGLESv2.dll")
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

    foreach ($file in $manifest.fileHashes.PSObject.Properties) {
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
        mesaReleaseTag = $mesaReleaseTag
        mesaAssetName = $mesaAssetName
        mesaAssetSha256 = $mesaAssetSha256
        sourceArchiveSha256 = Get-FileSha256 $Archive
        cachedAtUtc = (Get-Date).ToUniversalTime().ToString("o")
        fileHashes = $fileHashes
    }

    $manifest | ConvertTo-Json -Depth 4 | Set-Content -Path (Get-MesaManifestPath $RuntimeDir) -Encoding ASCII
}

function Write-AngleRuntimeManifest {
    param(
        [string]$RuntimeDir,
        [string]$ManifestPath
    )

    $fileHashes = [ordered]@{}
    foreach ($name in @("libEGL.dll", "libGLESv2.dll", "d3dcompiler_47.dll")) {
        $path = Join-Path $RuntimeDir $name
        if (Test-Path $path) {
            $fileHashes[$name] = Get-FileSha256 $path
        }
    }

    $manifest = [ordered]@{
        sourcePath = (Resolve-Path $RuntimeDir).Path
        cachedAtUtc = (Get-Date).ToUniversalTime().ToString("o")
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
            Write-Host "Mesa runtime explicit path: $resolved"
            return $resolved
        }
    }

    $thirdPartyDir = Join-Path $root "third_party\mesa"
    $resolvedThirdParty = Resolve-MesaRuntimeCandidate $thirdPartyDir
    if ($resolvedThirdParty) {
        Write-Host "Mesa runtime explicit path: $resolvedThirdParty"
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

    cargo rustc --release -- -L "native=$linkLibDir"
    if ($LASTEXITCODE -ne 0) {
        throw "cargo rustc --release failed with exit code $LASTEXITCODE"
    }
    Copy-Item "target\release\scope_analyzer.exe" (Join-Path $stage "ScopeAnalyzer.exe")
    Copy-Item "resources\ScopeAnalyzer.ico" (Join-Path $stage "ScopeAnalyzer.ico")
    Copy-Item "README.md" (Join-Path $stage "README.txt")
    $angleRuntimeDir = Resolve-AngleRuntimeDir
    if ($angleRuntimeDir) {
        Copy-Item (Join-Path $angleRuntimeDir "libEGL.dll") (Join-Path $stage "libEGL.dll") -Force
        Copy-Item (Join-Path $angleRuntimeDir "libGLESv2.dll") (Join-Path $stage "libGLESv2.dll") -Force
        $includeAngleRuntime = "true"

        $d3dCompiler = Join-Path $angleRuntimeDir "d3dcompiler_47.dll"
        if (Test-Path $d3dCompiler) {
            Copy-Item $d3dCompiler (Join-Path $stage "d3dcompiler_47.dll") -Force
            $includeD3DCompiler = "true"
        }
        Write-AngleRuntimeManifest -RuntimeDir $angleRuntimeDir -ManifestPath (Join-Path $stage $angleManifestName)
    } else {
        Write-Warning "ANGLE runtime DLLs were not bundled. The installed app will rely on the target machine's OpenGL/ANGLE installation."
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
        $includeMesaRuntime = "true"
    } else {
        Write-Warning "Mesa runtime DLLs were not found. Mesa/llvmpipe fallback will be unavailable in this package."
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

    Write-Host "Created $msi"
    return
}

throw "WiX Toolset v3 was not found. Install WiX v3 and make candle.exe/light.exe available on PATH to create the MSI."
