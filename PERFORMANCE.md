# Performance Baselines

Large-file performance checks live in `src/perf_tests.rs` as ignored tests, so
normal `cargo test` runs stay fast.

Run all performance baselines:

```powershell
.\scripts\run-performance-baselines.ps1
```

Each test creates temporary CSV, DAT, cloud `Content` CSV, or PNG files, prints
timings like `perf:csv_open 123.45 ms`, and removes the temporary files.

The script runs:

```powershell
cargo test perf_ -- --ignored --nocapture --test-threads=1
```

and sets conservative default regression thresholds for every printed label.
Override any threshold with `SCOPE_PERF_MAX_<LABEL>_MS`, where `<LABEL>` is the
printed label in uppercase with non-alphanumeric characters replaced by `_`.

Examples:

```powershell
$env:SCOPE_PERF_MAX_CSV_OPEN_MS = "1500"
$env:SCOPE_PERF_MAX_DAT_SUMMARY_FULL_MS = "200"
.\scripts\run-performance-baselines.ps1
```

To inspect the command and effective thresholds without running the perf tests:

```powershell
.\scripts\run-performance-baselines.ps1 -PrintCommandOnly
```

`scripts/package-windows.ps1` runs the performance gate before building release
artifacts. Use `-SkipPerformanceBaselines` only for local packaging experiments,
not release builds.

Covered scenarios:

- CSV: open, zoomed range read, full-range summary, plot-data load, FFT.
- DAT: open, zoomed range read, full-range summary, plot-data load, FFT.
- Cloud `Content` CSV: open, zoomed range read, full-range summary, plot-data load.
- PNG export path: canvas draw and PNG encode/write smoke baseline.
