$ErrorActionPreference = "Stop"

$out = Join-Path $PSScriptRoot "sample_wave.csv"
$sampleRate = 100000.0
$rows = 100000

"time,CH1,CH2,CH3,CH4" | Set-Content -Encoding ASCII $out
for ($i = 0; $i -lt $rows; $i++) {
    $t = $i / $sampleRate
    $ch1 = [math]::Sin(2.0 * [math]::PI * 1000.0 * $t)
    $ch2 = 0.7 * [math]::Sin(2.0 * [math]::PI * 2000.0 * $t)
    $ch3 = 0.4 * [math]::Sin(2.0 * [math]::PI * 3000.0 * $t)
    $ch4 = $ch1 + 0.25 * $ch2 + 0.12 * $ch3
    "{0:F8},{1:F6},{2:F6},{3:F6},{4:F6}" -f $t, $ch1, $ch2, $ch3, $ch4 | Add-Content -Encoding ASCII $out
}

Write-Host "Created $out"

