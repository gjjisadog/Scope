param(
    [switch]$NoDefaultThresholds,
    [switch]$PrintCommandOnly
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot

$defaultThresholdsMs = [ordered]@{
    "CSV_OPEN" = 12000
    "CSV_READ_ZOOM" = 2000
    "CSV_SUMMARY_FULL" = 2000
    "PLOT_LOAD_CSV_FULL" = 3000
    "FFT_CSV_ZOOM" = 1000
    "DAT_OPEN" = 4000
    "DAT_READ_ZOOM" = 1000
    "DAT_SUMMARY_FULL" = 1000
    "PLOT_LOAD_DAT_FULL" = 2000
    "FFT_DAT_ZOOM" = 1000
    "CLOUD_OPEN" = 12000
    "CLOUD_READ_ZOOM" = 2500
    "CLOUD_SUMMARY_FULL" = 2000
    "PLOT_LOAD_CLOUD_FULL" = 3000
    "PNG_CANVAS_DRAW" = 1000
    "PNG_WRITE" = 2000
}

function Set-DefaultThreshold {
    param(
        [string]$Name,
        [int]$Milliseconds
    )

    $envName = "SCOPE_PERF_MAX_${Name}_MS"
    if (-not [Environment]::GetEnvironmentVariable($envName, "Process")) {
        [Environment]::SetEnvironmentVariable($envName, [string]$Milliseconds, "Process")
    }
}

if (-not $NoDefaultThresholds -and $env:SCOPE_PERF_NO_DEFAULT_THRESHOLDS -ne "1") {
    foreach ($entry in $defaultThresholdsMs.GetEnumerator()) {
        Set-DefaultThreshold -Name $entry.Key -Milliseconds $entry.Value
    }
}

Write-Host "Performance thresholds for this run:"
foreach ($entry in $defaultThresholdsMs.GetEnumerator()) {
    $envName = "SCOPE_PERF_MAX_$($entry.Key)_MS"
    $value = [Environment]::GetEnvironmentVariable($envName, "Process")
    if (-not $value) {
        $value = [Environment]::GetEnvironmentVariable($envName, "User")
    }
    if (-not $value) {
        $value = [Environment]::GetEnvironmentVariable($envName, "Machine")
    }
    if ($value) {
        Write-Host "  ${envName}=${value}"
    } else {
        Write-Host "  ${envName}=<unset>"
    }
}

$cargoArgs = @("test", "perf_", "--", "--ignored", "--nocapture", "--test-threads=1")
Write-Host "Running: cargo $($cargoArgs -join ' ')"

if ($PrintCommandOnly) {
    return
}

Push-Location $root
try {
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "performance baseline tests failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}
