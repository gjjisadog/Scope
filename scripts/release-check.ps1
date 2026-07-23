param(
    [switch]$SkipPerformanceBaselines
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot

if ($env:SCOPE_PACKAGE_OFFLINE -eq "1") {
    $env:CARGO_NET_OFFLINE = "true"
    Write-Host "CARGO_NET_OFFLINE=true (inherited from SCOPE_PACKAGE_OFFLINE=1)"
}

function Invoke-ReleaseStep {
    param(
        [string]$Name,
        [scriptblock]$Command
    )

    Write-Host ""
    Write-Host "==> $Name"
    & $Command
    if ($null -ne $LASTEXITCODE -and $LASTEXITCODE -ne 0) {
        throw "$Name failed with exit code $LASTEXITCODE"
    }
}

function Get-RegexValue {
    param(
        [string]$Text,
        [string]$Pattern,
        [string]$Description
    )

    $match = [regex]::Match($Text, $Pattern)
    if (-not $match.Success) {
        throw "Could not read $Description."
    }
    return $match.Groups[1].Value
}

function Assert-VersionSync {
    $cargoToml = Get-Content -Raw (Join-Path $root "Cargo.toml")
    $packageScript = Get-Content -Raw (Join-Path $PSScriptRoot "package-windows.ps1")
    $wxs = Get-Content -Raw (Join-Path $PSScriptRoot "ScopeAnalyzer.wxs")
    $readme = Get-Content -Raw (Join-Path $root "README.md")

    $cargoVersion = Get-RegexValue -Text $cargoToml -Pattern '(?m)^version\s*=\s*"([^"]+)"' -Description "Cargo.toml package version"
    $packageVersion = Get-RegexValue -Text $packageScript -Pattern '\$version\s*=\s*"([^"]+)"' -Description "scripts/package-windows.ps1 version"
    $wxsVersion = Get-RegexValue -Text $wxs -Pattern 'Version="([^"]+)"' -Description "scripts/ScopeAnalyzer.wxs Product Version"

    if ($cargoVersion -ne $packageVersion -or $cargoVersion -ne $wxsVersion) {
        throw "Version mismatch: Cargo.toml=$cargoVersion package-windows.ps1=$packageVersion ScopeAnalyzer.wxs=$wxsVersion"
    }

    foreach ($artifact in @(
        "ScopeAnalyzer-$cargoVersion-win-x64.zip",
        "ScopeAnalyzer-$cargoVersion-win-x64.msi"
    )) {
        if ($readme -notmatch [regex]::Escape($artifact)) {
            throw "README.md does not mention expected release artifact $artifact"
        }
    }

    Write-Host "Version sync ok: $cargoVersion"
}

function Initialize-MingwLibraryPath {
    if ($env:LIBRARY_PATH) {
        return
    }

    $toolsRoot = Join-Path $env:USERPROFILE ".codex-tools"
    if (-not (Test-Path $toolsRoot)) {
        return
    }

    $candidate = Get-ChildItem -Path $toolsRoot -Directory -Filter "llvm-mingw-*" -ErrorAction SilentlyContinue |
        ForEach-Object { Join-Path $_.FullName "x86_64-w64-mingw32\lib" } |
        Where-Object { Test-Path (Join-Path $_ "libshlwapi.a") } |
        Sort-Object -Descending |
        Select-Object -First 1

    if ($candidate) {
        $env:LIBRARY_PATH = $candidate
        Write-Host "Set LIBRARY_PATH=$candidate"
    }
}

Push-Location $root
try {
    Initialize-MingwLibraryPath

    Invoke-ReleaseStep "version sync" {
        Assert-VersionSync
    }

    Invoke-ReleaseStep "PowerShell script parse" {
        $null = [scriptblock]::Create((Get-Content -Raw (Join-Path $PSScriptRoot "package-windows.ps1")))
        $null = [scriptblock]::Create((Get-Content -Raw (Join-Path $PSScriptRoot "run-performance-baselines.ps1")))
        $null = [scriptblock]::Create((Get-Content -Raw (Join-Path $PSScriptRoot "test-package-windows.ps1")))
        $null = [scriptblock]::Create((Get-Content -Raw (Join-Path $PSScriptRoot "release-check.ps1")))
        $null = [scriptblock]::Create((Get-Content -Raw (Join-Path $PSScriptRoot "windows-acceptance.ps1")))
    }

    Invoke-ReleaseStep "WiX XML parse" {
        [xml]$null = Get-Content -Raw (Join-Path $PSScriptRoot "ScopeAnalyzer.wxs")
    }

    Invoke-ReleaseStep "packaging script checks" {
        & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "test-package-windows.ps1")
    }

    Invoke-ReleaseStep "cargo fmt --check" {
        & cargo fmt --check
    }

    Invoke-ReleaseStep "cargo clippy --all-targets" {
        & cargo clippy --locked --all-targets --no-deps --quiet -- -D warnings
    }

    Invoke-ReleaseStep "cargo test --quiet" {
        & cargo test --locked --all-targets --quiet
    }

    if ($SkipPerformanceBaselines) {
        Write-Warning "Skipping performance baselines. Do not use this for release readiness."
    } else {
        Invoke-ReleaseStep "performance baselines" {
            & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "run-performance-baselines.ps1")
        }
    }
}
finally {
    Pop-Location
}
