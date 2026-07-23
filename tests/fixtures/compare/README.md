# Compare fixtures

These small CSV files describe the semantics of the Compare core. The first
column is time in seconds and the second column is the channel value.

- `reference.csv` is the nominal signal.
- `test-delayed.csv` is shifted by +0.1 seconds and has a 10% amplitude error.
- `test-resampled.csv` has a denser timestamp grid to verify explicit timestamp
  resampling across different sample rates.
- A missing interval is represented by a segment boundary in the Rust fixture
  tests; it must never be linearly interpolated as a zero or bridged sample.

Alignment offsets are applied to Test time. An offset of `+0.1` means a Test
sample at `t - 0.1` is compared with a Reference sample at `t`. Relative error
uses `abs(error) / max(abs(reference), relative_floor)`. Invalid points close
any open tolerance-exceedance interval.
CLI smoke paths:

```text
scope-cli inspect --input reference.csv
scope-cli analyze --input reference.csv --channel 0
scope-cli compare --reference reference.csv --test test-delayed.csv --absolute-tolerance 0.5
scope-cli test --metrics metrics.json --rules rules.json
scope-cli report --compare compare-output.json --source reference.csv --source test-delayed.csv --output report.md
# For SCP1 recordings and project files:
scope-cli validate-recording --input capture.scope
scope-cli project --input analysis.scopeproj
```

Each command emits a versioned JSON envelope and uses non-zero exit codes for
usage, input, compare, and rule failures. A valid `scope-cli test` invocation
returns exit code 5 when the evaluated rules are not all passed. A
`validate-recording` result with `valid: false` (missing clean SessionEnd or a
recovered tail) also returns exit code 5 while preserving the JSON evidence.
