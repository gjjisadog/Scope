$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$version = "0.2.0"
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

$clang = Get-Command "x86_64-w64-mingw32-clang.exe" -ErrorAction SilentlyContinue
    if ($clang) {
        $runtimeDir = Split-Path -Parent $clang.Source
        $libunwind = Join-Path $runtimeDir "libunwind.dll"
        if (Test-Path $libunwind) {
            Copy-Item $libunwind (Join-Path $stage "libunwind.dll")
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
    & $candle -arch x64 -dStageDir="$stage" -dIncludeLibUnwind="$includeLibUnwind" -dIncludeAngleRuntime="$includeAngleRuntime" -dIncludeD3DCompiler="$includeD3DCompiler" -out $wixobj $wxs
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
