$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$version = "0.4.0"
$dist = Join-Path $root "dist"
$stage = Join-Path $dist "ScopeAnalyzer-$version-win-x64"
$zip = Join-Path $dist "ScopeAnalyzer-$version-win-x64.zip"
$msi = Join-Path $dist "ScopeAnalyzer-$version-win-x64.msi"
$wixobj = Join-Path $dist "ScopeAnalyzer-$version-win-x64.wixobj"
$wxs = Join-Path $PSScriptRoot "ScopeAnalyzer.wxs"

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

function Resolve-AngleRuntimeDir {
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
        if ((Test-Path (Join-Path $dir "libEGL.dll")) -and (Test-Path (Join-Path $dir "libGLESv2.dll"))) {
            return $dir
        }

        if (-not (Test-Path $dir)) {
            continue
        }

        $versionDirs = Get-ChildItem -Path $dir -Directory -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending
        foreach ($versionDir in $versionDirs) {
            if ((Test-Path (Join-Path $versionDir.FullName "libEGL.dll")) -and (Test-Path (Join-Path $versionDir.FullName "libGLESv2.dll"))) {
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

function Install-MesaRuntime {
    $cacheDir = Join-Path $root "target\mesa-runtime"
    $runtimeDir = Join-Path $cacheDir "x64"
    $cached = Resolve-MesaRuntimeCandidate $runtimeDir
    if ($cached) {
        return $cached
    }

    New-Item -ItemType Directory -Force -Path $cacheDir | Out-Null
    $headers = @{ "User-Agent" = "ScopeAnalyzerPackageScript" }
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/pal1000/mesa-dist-win/releases/latest" -Headers $headers
    $asset = $release.assets |
        Where-Object { $_.name -like "*release-msvc.7z" } |
        Select-Object -First 1
    if (-not $asset) {
        Write-Warning "Mesa release-msvc asset was not found in the latest mesa-dist-win release."
        return $null
    }

    $archive = Join-Path $cacheDir $asset.name
    if (-not (Test-Path $archive)) {
        Invoke-WebRequest -Uri $asset.browser_download_url -Headers $headers -OutFile $archive
    }

    $sevenZip = Join-Path $cacheDir "7zr.exe"
    if (-not (Test-Path $sevenZip)) {
        Invoke-WebRequest -Uri "https://www.7-zip.org/a/7zr.exe" -OutFile $sevenZip
    }

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

    return $runtimeDir
}

function Resolve-MesaRuntimeDir {
    $candidateDirs = @()
    if ($env:MESA_RUNTIME_DIR) {
        $candidateDirs += $env:MESA_RUNTIME_DIR
    }
    $candidateDirs += Join-Path $root "third_party\mesa"
    $candidateDirs += Join-Path $root "target\mesa-runtime\x64"

    foreach ($dir in $candidateDirs) {
        $resolved = Resolve-MesaRuntimeCandidate $dir
        if ($resolved) {
            return $resolved
        }
    }

    if ($env:SCOPE_SKIP_MESA_DOWNLOAD -eq "1") {
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
    } else {
        Write-Warning "ANGLE runtime DLLs were not found. The package will rely on the target machine's OpenGL/ANGLE installation."
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
Set-Content -Path (Join-Path $stage "Start-ScopeAnalyzer-DX12.bat") -Encoding ASCII -Value @(
    "@echo off",
    "cd /d ""%~dp0""",
    "set SCOPE_RENDERER=wgpu",
    "start """" ""%~dp0ScopeAnalyzer.exe"""
)
Set-Content -Path (Join-Path $stage "Start-ScopeAnalyzer-Software.bat") -Encoding ASCII -Value @(
    "@echo off",
    "cd /d ""%~dp0""",
    "set SCOPE_RENDERER=glow-software",
    "start """" ""%~dp0ScopeAnalyzer.exe"""
)
Set-Content -Path (Join-Path $stage "Start-ScopeAnalyzer-OpenGL.bat") -Encoding ASCII -Value @(
    "@echo off",
    "cd /d ""%~dp0""",
    "set SCOPE_RENDERER=glow",
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
