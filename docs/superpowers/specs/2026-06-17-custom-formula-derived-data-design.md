# Custom Formula Derived Data Design

## Goal

Add a custom formula calculation feature that lets users define reusable formulas, generate new derived waveform variables from loaded data, and apply those formulas to selected datasets without storing duplicated sample results.

The primary user outcome is: after importing waveform data, the user can create a formula such as `P_phaseA = stVg_0.iA * stIg_0.iA`, validate it with autocomplete assistance, add the result to the left channel list, and reuse the same formula on other compatible datasets with explicit channel mapping when names do not match.

## Confirmed Product Direction

The selected direction is a formula library plus derived variables:

- Formula results appear in the left channel panel under a `Formulas` group.
- Formula results behave like other plotted variables: they can be checked, hidden, colored, assigned to panes, measured with cursors, and exported in images or Word reports.
- Formula definitions are saved; computed samples are not persisted.
- Reopening data or changing a time window recalculates formula results from the current source data.
- Formula reuse defaults to channel name matching and falls back to explicit user mapping when a target dataset lacks matching names.

## First-Version Scope

In scope:

- Add a formula manager for creating, editing, deleting, validating, and applying formulas.
- Store formula definitions with result name, expression, optional unit, optional description, display settings, enabled state, and per-dataset channel mappings.
- Add formula result variables to the channel list under `Formulas`.
- Support formula results in plot rendering, cursor measurements, export preview, PNG/SVG export, batch export, and Word report export through the same derived data path.
- Support expression autocomplete for functions and channel names after one or two typed characters.
- Support name-based channel matching using raw names, display names, and stable `CHn` aliases.
- Show an explicit mapping prompt when a formula is reused on a dataset with missing variable references.
- Add a formula configuration import/export path: `Config > Formula Settings` using `scope-formulas.json`.
- Preserve existing PLL/dq0 behavior by migrating it into the generalized derived variable system as built-in derived outputs.
- Add automated tests for parser behavior, evaluation, autocomplete, mapping, caching, configuration, and PLL/dq0 regression.
- Update README, in-app help, and release metadata version files because this is a user-facing workflow change.

Out of scope for the first version:

- Cross-dataset formulas.
- Formula references to other formulas.
- Sliding-window functions.
- Previous/next sample references.
- Integration, differentiation, or resampling functions.
- Multi-line scripts or assignment statements inside formulas.
- Persisting computed formula sample arrays.
- Exporting formula results as a standalone synthetic dataset.

## User Experience

The formula workflow should be reachable from the analysis area or the left channel panel:

1. User opens `Formula Manager`.
2. User creates a formula with result name, expression, optional unit, and optional description.
3. The editor validates the expression while typing.
4. Function and channel autocomplete help the user enter expressions.
5. User saves the formula.
6. The result appears in the left channel list under `Formulas`.
7. User checks the formula result to plot it in the active pane.
8. User can apply the formula to selected datasets.
9. If a target dataset cannot resolve all referenced variables, the app opens a mapping prompt.
10. User completes mappings, then the formula result becomes available for that dataset.

Formula rows should show concise status:

- Ready: formula is valid and all channel mappings are resolved.
- Needs mapping: expression is valid but one or more referenced variables are unresolved for the selected dataset.
- Invalid: expression cannot be parsed or references unsupported functions.
- Error: calculation failed for the whole result.

Sample-level math errors should not disable a formula. Those samples become `NaN`; plotting and measurement code should skip or break over non-finite values as it already does for invalid samples.

## Formula Language

The first version uses a bounded expression language, not a scripting language.

Supported syntax:

- Arithmetic operators: `+`, `-`, `*`, `/`, `^`.
- Parentheses.
- Comparisons: `>`, `>=`, `<`, `<=`, `==`, `!=`.
- Logic: `&&`, `||`, `!`.
- Numeric constants including scientific notation, such as `1.5` and `1e-3`.
- Channel references by raw channel name, display name, or alias such as `CH1`.
- Conditional function: `if(condition, value_when_true, value_when_false)`.
- Functions: `abs`, `sqrt`, `sin`, `cos`, `tan`, `min`, `max`, `clamp`, `avg`, `rms`.

Evaluation rules:

- Formulas are evaluated point by point over the current calculation window.
- Formula output uses the same time axis as the loaded source block.
- `avg(x)` and `rms(x)` are whole-window statistics in the first version. Each returns a constant value repeated over the output time axis.
- Formula inputs come from one dataset at a time.
- Division by zero, invalid square root input, invalid function input, and non-finite source samples produce `NaN` for the affected output sample.
- Parse errors, unknown variables, unknown functions, wrong function arity, missing mappings, and circular references prevent the formula from being enabled.
- Formula references to other formulas are rejected in the first version, so dependency ordering and cycle resolution are not required yet.

## Autocomplete

Formula editing should provide autocomplete after short prefixes:

- Typing one or two letters filters function names, such as `r` -> `rms()` and `sq` -> `sqrt()`.
- Function suggestions show a signature and short description, such as `clamp(x, min, max)`.
- Accepting a function suggestion inserts the function name and parentheses, then places the caret at the first argument.
- Channel suggestions search raw names, display names, and `CHn` aliases.
- Channel suggestions should prefer exact prefix matches, then substring matches.
- Suggestions should avoid inserting ambiguous text silently. If a channel name needs escaping, the inserted expression should use the parser-supported escaped form.

The autocomplete engine should be independent from egui widgets so it can be unit tested with a fake channel catalog.

## Channel Matching And Reuse

Formula definitions store symbolic references, not channel indexes only.

For each referenced variable, store:

- the originally typed token;
- resolved raw channel name when available;
- resolved display name when available;
- resolved `CHn` alias when used;
- per-dataset mapping override when the user manually maps the reference.

When applying a formula to a dataset, resolve references in this order:

1. Existing manual mapping for that dataset.
2. Exact raw channel name match.
3. Exact display name match.
4. Stable `CHn` alias match.
5. Case-insensitive raw or display name match.
6. User mapping prompt.

The app must not silently fall back to an arbitrary channel. If a required reference cannot be resolved, the formula row should remain in `Needs mapping` state for that dataset until the user maps it or disables it.

## Architecture

The implementation should convert the current fixed PLL/dq0 derived-curve path into a generalized derived variable system.

### Formula Module

Create `src/formula.rs`.

Responsibilities:

- Tokenize formula text.
- Parse expressions into an AST.
- Track source spans for user-facing errors.
- Extract referenced variable tokens.
- Validate function names and argument counts.
- Evaluate scalar and vector expressions over a `SampleBlock`.
- Provide deterministic error types suitable for localization.

This module should not depend on egui.

### Formula Completion Module

Create `src/formula_completion.rs` or keep this code in `src/formula.rs` if it stays small.

Responsibilities:

- Define function metadata: name, signature, inserted text, and short description.
- Accept a channel catalog containing raw names, display names, and aliases.
- Return ranked completion candidates for a prefix.
- Keep insertion behavior testable without UI.

### Derived Module

Create `src/derived.rs`.

Responsibilities:

- Define `DerivedDefinition`.
- Define `DerivedKind::BuiltInPllDq0` and `DerivedKind::Formula`.
- Define formula reference mappings.
- Define derived output metadata used by UI, plotting, measurement, and export.
- Own cache keys for derived calculations that include data generation, dataset index, time range, formula revision, source channel mappings, channel scale bits, and relevant analysis settings.
- Preserve PLL/dq0 outputs as built-in derived definitions.

### App Integration

Update `src/app.rs`, `src/app/state.rs`, `src/app/plot.rs`, and `src/app/jobs.rs` to use the generalized derived model.

The app layer should handle:

- Formula manager UI state.
- Formula editor text and autocomplete popup state.
- Formula mapping dialog state.
- Worker scheduling, cancellation, polling, and error display.
- Marking plot, measurement, and export caches dirty when formula definitions or mappings change.
- Rendering formula-derived outputs in the channel panel.

The app layer should avoid owning parser and evaluator logic directly.

## Data Flow

Single dataset formula calculation:

1. User checks a formula result.
2. App builds a derived job key for the current dataset, visible time range, formula definitions, mappings, and display scale dependencies.
3. Worker reads required source channels through the existing `DataSource::read_range_cancellable` path.
4. Worker applies channel scale factors consistently with normal plotting and measurements.
5. Worker evaluates formula AST over source channel arrays.
6. Worker returns a `SampleBlock` whose channels are formula result arrays.
7. App prepares plot points using the existing `PreparedPlotSeries` path.
8. Measurement and export consume the same derived result cache where possible, or request a range-specific derived calculation with the same evaluator.

Multi-dataset formula reuse:

1. User selects one or more datasets and chooses `Apply Formula`.
2. App resolves references for each dataset.
3. Datasets with complete mappings become enabled.
4. Datasets with missing mappings show a mapping prompt.
5. The app reports which datasets are ready and which still need user action.

## Configuration

Add a dedicated formula configuration section:

- Default file: `scope-formulas.json`.
- Recent file list: `scope-recent-formula-configs.json`.
- Menu: `Config > Formula Settings`.

The formula config stores:

- formula definitions;
- result names;
- expressions;
- units and descriptions;
- enabled states;
- display colors, line styles, and pane assignments for formula outputs;
- per-dataset mapping overrides when meaningful.

The display config can continue storing general display settings. Formula definitions should not be mixed into `scope-display.json` because formulas are semantic user logic rather than only presentation.

Older configs without formulas must continue to import successfully.

## Error Handling

User-facing errors should be specific and localized:

- Invalid token at position.
- Missing closing parenthesis.
- Unknown function.
- Wrong number of function arguments.
- Unknown channel reference.
- Formula needs channel mapping for a dataset.
- Formula references another formula, which is not supported in the first version.
- Calculation worker panicked.
- Calculation was cancelled.

Formula evaluation should distinguish:

- formula-level errors that prevent a result from existing;
- sample-level invalid values that produce `NaN`.

Batch operations should summarize partial readiness. For example, applying one formula to five datasets may enable three and leave two in `Needs mapping`.

## UI Notes

The Formula Manager should stay practical and dense, consistent with the existing oscilloscope-style tool:

- A list of formulas on the left.
- Editor fields on the right: result name, expression, unit, description.
- Validation status under the expression field.
- Function/channel autocomplete popup near the caret.
- Buttons: `Validate`, `Save`, `Apply to selected datasets`, `Delete`.
- Mapping prompt table: referenced token, current match, target dataset channel selector.

Avoid a large marketing-style or wizard interface. This is an engineering tool; fast repeated edits matter more than visual ceremony.

## Testing And Verification

Minimum automated tests:

- Parser precedence: `CH1 + CH2 * 2`, `(CH1 + CH2) * 2`, unary minus, and `^`.
- Function evaluation: `abs`, `sqrt`, `sin`, `cos`, `min`, `max`, `clamp`, `if`, `avg`, `rms`.
- Non-finite behavior: division by zero and invalid `sqrt` produce `NaN` at affected samples.
- Variable extraction from raw names, display names, and aliases.
- Autocomplete ranking for function prefixes and channel prefixes.
- Function completion insertion text and caret target.
- Channel matching order, including manual mapping and missing mapping.
- Formula config serialization and old-config compatibility.
- Derived cache key changes when formula text, mapping, source scales, time range, dataset, or data generation changes.
- Worker cancellation returns without updating stale state.
- PLL/dq0 regression tests still pass after migration to the generalized derived system.
- Existing measurement and export tests continue to pass with derived formula outputs selected.

Manual verification:

- Import a normal CSV dataset.
- Create `P_phaseA = CH1 * CH2`.
- Confirm autocomplete suggests `CH1`, `CH2`, and functions.
- Check the formula result and confirm it plots.
- Move the formula result to a different pane.
- Measure the formula result between cursors.
- Export PNG/SVG and confirm the formula result appears.
- Export a Word report and confirm the formula result appears in the image and cursor table when enabled.
- Apply the formula to a second compatible dataset and confirm automatic mapping.
- Apply the formula to a dataset with different channel names and confirm the mapping prompt appears.
- Save formula settings, restart, reload data, import formula settings, and confirm definitions reload and results recalculate.

## Documentation

Update README and in-app Help to describe:

- where to open Formula Manager;
- how to create and validate formulas;
- supported operators and functions;
- function and channel autocomplete;
- how formula results appear in `Formulas`;
- how formula reuse and channel mapping work;
- first-version limitations.

## Versioning

This feature changes user-facing behavior and introduces a new workflow. It counts as a major change under the repository rules.

When implementing, update these files in the same change set:

- `Cargo.toml` package `version`;
- `scripts/package-windows.ps1` `$version`;
- `scripts/ScopeAnalyzer.wxs` `Product Version`;
- README package artifact names when they include the version.

Before producing release artifacts, run the repository release preflight:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/release-check.ps1
```
