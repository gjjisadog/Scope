# Route A Compare MVP Implementation Plan

> Implementation status: completed in the current 0.12.0 change set. The repository also includes the project V2 integration, deterministic rules, report generation, and the remaining CLI inspection/analysis/recording/project commands described by the Route A design.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a deterministic, gap-preserving Reference/Compare core that can compare two sampled channels with explicit alignment, resampling, error metrics, tolerance intervals, and JSON-safe results.

**Architecture:** The core lives in `src/compare/` and contains no egui, filesystem, or UI state. `Series` preserves contiguous segments so interpolation never bridges a gap. The first application integration consumes this core from future GUI/CLI adapters; project schema migration is a separate task after the core is stable.

**Tech Stack:** Rust 2021, serde, existing `SampleBlock` conventions, inline unit tests, Cargo locked dependencies.

## Global Constraints

- Preserve SCP1 V1, `.scope` V1, and existing `DataSource` behavior.
- Do not add compare logic to `src/app.rs`.
- A gap is invalid data, never an implicit zero or an interpolated connection.
- Do not silently divide by zero when computing relative error.
- New behavior follows Red → Green → Refactor.
- Run `cargo fmt --all -- --check`, targeted tests, and `cargo test --locked --all-targets` before handoff.

---

### Task 1: Add the compare data model and module boundary

**Files:**
- Create: `src/compare/mod.rs`
- Modify: `src/lib.rs`
- Test: `src/compare/mod.rs`

**Interfaces:**
- Produces `Series`, `SeriesSegment`, `AlignmentSpec`, `Tolerance`, `CompareRequest`, `ComparePoint`, `CompareInterval`, `CompareSummary`, and `CompareResult`.
- Later tasks consume these public-in-crate types through `crate::compare`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn series_rejects_mismatched_time_and_value_lengths() {
    let result = SeriesSegment::new(vec![0.0, 1.0], vec![1.0]);
    assert!(matches!(result, Err(CompareError::LengthMismatch { .. })));
}

#[test]
fn series_preserves_empty_gap_free_segments_only() {
    let series = Series::new(vec![
        SeriesSegment::new(vec![0.0, 1.0], vec![1.0, 2.0]).unwrap(),
    ])
    .unwrap();
    assert_eq!(series.segments().len(), 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --locked compare::tests::series_rejects_mismatched_time_and_value_lengths`

Expected: compile failure because `crate::compare` and the requested types do not exist.

- [ ] **Step 3: Write minimal implementation**

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct SeriesSegment { pub times: Vec<f64>, pub values: Vec<f64> }

impl SeriesSegment {
    pub fn new(times: Vec<f64>, values: Vec<f64>) -> Result<Self, CompareError> {
        if times.len() != values.len() {
            return Err(CompareError::LengthMismatch { times: times.len(), values: values.len() });
        }
        if times.windows(2).any(|pair| !pair[0].is_finite() || !pair[1].is_finite() || pair[1] <= pair[0])
            || values.iter().any(|value| !value.is_finite())
        {
            return Err(CompareError::InvalidSegment);
        }
        Ok(Self { times, values })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Series { segments: Vec<SeriesSegment> }

impl Series {
    pub fn new(segments: Vec<SeriesSegment>) -> Result<Self, CompareError> {
        if segments.iter().any(|segment| segment.times.is_empty()) { return Err(CompareError::EmptySegment); }
        Ok(Self { segments })
    }
    pub fn segments(&self) -> &[SeriesSegment] { &self.segments }
}
```

Define `CompareError` with `LengthMismatch`, `InvalidSegment`, `EmptySegment`, `InvalidAlignment`, and `NoOverlap` variants. Add `pub mod compare;` to `src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --locked compare::tests::series_`

Expected: targeted tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/compare/mod.rs src/lib.rs
git commit -m "feat(compare): add gap-preserving series model"
```

### Task 2: Implement explicit alignment and gap-safe interpolation

**Files:**
- Modify: `src/compare/mod.rs`
- Test: `src/compare/mod.rs`

**Interfaces:**
- `AlignmentSpec::ManualOffset { seconds }` shifts Test time by the declared amount.
- `AlignmentSpec::Anchor { reference_time, test_time }` derives the same shift from two known event times.
- `Series::sample_at(time)` returns `Option<f64>` and only searches one contiguous segment.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn interpolation_uses_linear_value_inside_a_segment() {
    let series = test_series(&[(0.0, 0.0), (1.0, 10.0)]);
    assert_eq!(series.sample_at(0.25), Some(2.5));
}

#[test]
fn interpolation_does_not_bridge_a_gap() {
    let series = Series::new(vec![
        segment(&[(0.0, 0.0), (1.0, 1.0)]),
        segment(&[(3.0, 3.0), (4.0, 4.0)]),
    ]).unwrap();
    assert_eq!(series.sample_at(2.0), None);
}

#[test]
fn anchor_alignment_shifts_test_into_reference_time() {
    let alignment = AlignmentSpec::Anchor { reference_time: 5.0, test_time: 3.0 };
    assert_eq!(alignment.offset_seconds().unwrap(), 2.0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --locked compare::tests::interpolation_uses_linear_value_inside_a_segment`

Expected: compile failure because `sample_at` and `AlignmentSpec` are not implemented.

- [ ] **Step 3: Implement interpolation and alignment**

Use `binary_search_by` to find a neighboring pair, return an exact endpoint without extrapolation, and return `None` for all times outside a segment or between segments. Reject non-finite alignment values and offsets that are not finite.

The unit-test helpers used by this and later tasks are:

```rust
fn segment(points: &[(f64, f64)]) -> SeriesSegment {
    SeriesSegment::new(
        points.iter().map(|(time, _)| *time).collect(),
        points.iter().map(|(_, value)| *value).collect(),
    ).unwrap()
}

fn test_series(points: &[(f64, f64)]) -> Series {
    Series::new(vec![segment(points)]).unwrap()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --locked compare::tests::interpolation_`

Expected: all interpolation tests pass, including endpoint and out-of-range cases.

- [ ] **Step 5: Commit**

```bash
git add src/compare/mod.rs
git commit -m "feat(compare): add explicit alignment and gap-safe interpolation"
```

### Task 3: Implement comparison metrics and tolerance intervals

**Files:**
- Modify: `src/compare/mod.rs`
- Test: `src/compare/mod.rs`

**Interfaces:**
- `Tolerance { absolute: Option<f64>, relative: Option<f64> }` requires at least one finite non-negative limit.
- `CompareRequest { reference, test, alignment, tolerance, relative_floor }` compares reference timestamps against the shifted Test series.
- `CompareResult.points` contains valid and invalid points; `summary` includes valid count, invalid count, RMS error, max absolute error, max relative error, and exceedance intervals.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn compare_reports_absolute_and_relative_error() {
    let result = compare(CompareRequest::new(
        test_series(&[(0.0, 10.0), (1.0, 10.0)]),
        test_series(&[(0.0, 12.0), (1.0, 12.0)]),
    )).unwrap();
    assert_eq!(result.summary.valid_points, 2);
    assert!((result.summary.rms_error - 2.0).abs() < 1.0e-12);
    assert!((result.summary.max_relative_error - 0.2).abs() < 1.0e-12);
}

#[test]
fn compare_marks_gap_as_invalid_and_does_not_create_exceedance() {
    let request = CompareRequest::new(
        Series::new(vec![segment(&[(0.0, 1.0), (1.0, 1.0)]), segment(&[(3.0, 1.0), (4.0, 1.0)])]).unwrap(),
        test_series(&[(0.0, 1.0), (4.0, 1.0)]),
    );
    let result = compare(request).unwrap();
    assert!(result.points.iter().any(|point| !point.valid));
}

#[test]
fn tolerance_exceedance_is_closed_into_intervals() {
    let mut request = CompareRequest::new(
        test_series(&[(0.0, 0.0), (1.0, 0.0), (2.0, 0.0)]),
        test_series(&[(0.0, 0.0), (1.0, 2.0), (2.0, 0.0)]),
    );
    request.tolerance = Some(Tolerance::absolute(1.0));
    let result = compare(request).unwrap();
    assert_eq!(result.summary.exceedance_intervals, vec![CompareInterval { start: 1.0, end: 1.0 }]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --locked compare::tests::compare_reports_absolute_and_relative_error`

Expected: compile failure because metrics types and `compare` are missing.

- [ ] **Step 3: Implement minimal deterministic metrics**

Iterate reference segments and each timestamp. Shift the Test query by the alignment offset, sample Test with `sample_at`, and emit an invalid point when no Test value exists. Relative error is `abs(error) / max(abs(reference), relative_floor)` only when the denominator is positive and finite; otherwise it is `None`. An interval starts at the first exceeding valid point and closes at the last consecutive exceeding point; invalid points close the interval.

- [ ] **Step 4: Run targeted and full tests**

Run: `cargo test --locked compare::tests::`

Expected: all Compare tests pass.

Run: `cargo test --locked --all-targets`

Expected: existing repository tests and Compare tests pass with zero failures.

- [ ] **Step 5: Commit**

```bash
git add src/compare/mod.rs
git commit -m "feat(compare): add deterministic error metrics and tolerances"
```

### Task 4: Add fixture corpus and public documentation

**Files:**
- Create: `tests/fixtures/compare/README.md`
- Create: `tests/fixtures/compare/reference.csv`
- Create: `tests/fixtures/compare/test-delayed.csv`
- Modify: `README.md`

**Interfaces:**
- Fixtures describe a known 2-second signal, a 0.1-second delay, a 10% amplitude error, and a deliberate missing interval.

- [ ] **Step 1: Add fixture assertions**

Keep the executable golden vectors in `src/compare/mod.rs` so the core test suite has no GUI or filesystem dependency. Store the equivalent CSV files as reviewable fixture artifacts; the tests must assert the same delay, amplitude error, and invalid interval described by the files.

- [ ] **Step 2: Verify fixture contents**

Run: `cargo test --locked compare::tests:: && cargo fmt --all -- --check`

Expected: Compare tests and formatting pass.

- [ ] **Step 3: Document semantics**

Document alignment sign, gap behavior, relative-error floor, tolerance precedence, and the fact that no cross-device clock drift is inferred.

- [ ] **Step 4: Commit**

```bash
git add tests/fixtures/compare README.md
git commit -m "docs(compare): document fixture semantics"
```

## Verification checklist

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --locked --all-targets --no-deps -- -D warnings`
- [ ] `cargo test --locked --all-targets`
- [ ] Compare core has tests for exact endpoints, out-of-range values, gaps, manual/anchor alignment, zero reference values, tolerance intervals, and invalid input.
- [ ] `git diff --check`
