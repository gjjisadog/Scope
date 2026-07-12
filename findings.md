# Findings & Decisions

## Active Objective: Shared Live/Offline Analysis (2026-07-11)

### Authoritative State
- Current worktree: `/Users/wangxuwen/Documents/Scope-live-scope-shared-analysis` on branch `codex/live-scope-shared-analysis`.
- Pre-existing unrelated dirty files are `AGENTS.md`, `CLAUDE.md`, and `.claude/skills/`; preserve them.
- `DataSource` is the common offline contract and is already implemented by `ScopeRecordingDataSource`.
- `LiveScopeState::display_snapshot` returns either the frozen snapshot or a newly generated immutable snapshot.
- Live plotting currently bypasses the main offline plot/cache/analysis pipeline and renders directly in `src/app/live_ui.rs`.
- The main analysis workers and export/annotation paths are concentrated in the large `src/app.rs`; extraction must proceed interface-first.

### Initial Architecture Direction
- Add an in-memory `SnapshotDataSource` backed by immutable `SampleBlock`/metadata.
- Convert Live timestamps to a stable local range while retaining sample rate and gap boundaries needed by plotting/export.
- Enter the existing offline dataset initialization path rather than duplicating analysis panels.
- Introduce shared presentation/view state incrementally so acquisition semantics remain isolated.

### GitNexus Impact Audit
- `DataSource` has 10 direct implementers and medium change risk. The segmented-read and presentation hooks are additive default methods, so legacy implementations retain source compatibility; all-target compilation and full tests confirm this.
- `LiveSnapshot` is critical-risk because it participates in buffer, trigger, state, and Live UI flows. Prefer a conversion layer over changing its existing fields/semantics.
- Existing Live snapshots already preserve gap-separated `segments`; snapshot conversion must not silently draw across gaps.
- Shared plot/export state is presently rooted in `ScopeApp`, while Live plotting reads `LiveSnapshot` directly. The first safe convergence point is the offline primary-source initialization path.

### Shared Presentation
- Added a serializable `ChannelPresentation` value object containing display name, RGBA color, visibility, scale, and pane.
- Live state now uses one `BTreeMap<u16, ChannelPresentation>` instead of three independently mutable maps, eliminating drift between online display properties.
- Snapshot-to-offline transition applies the same presentation values to the replay/analysis dataset.
- `.scope` metadata now stores `ChannelPresentation` with a serde default for backward compatibility; replay restores presentation through the common `DataSource::channel_presentation` hook.

### Shared Viewport, Gap, and Export
- Offline cursor/X/Y/pane state now lives in shared `PlotViewport`; Live linked plots mirror X bounds and cursor clicks into the same type, then transfer it when opening a Capture for analysis.
- `DataSource::read_range_segments` preserves discontinuities. Snapshot and `.scope` sources return gap-separated blocks, while legacy sources retain the default one-block behavior.
- Mainline prepared plot series, cursor interpolation, annotation anchors, canvas/SVG line rendering, and label collision sampling consume segments without connecting across gaps.
- The trigger-Capture integration test proves the frozen source reaches mainline measurement, FFT/THD, sequence analysis, pane/presentation initialization, and export-preview entry.

### Final Verification Audit
- `cargo fmt --all -- --check` and `git diff --check` passed.
- `cargo clippy --all-targets --quiet` passed; remaining warnings are pre-existing application/vendor lint debt.
- Normal tests passed: 86 library, 118 main-application, and 1 simulator test; 5 explicitly ignored tests remain in the application target.
- The four release-relevant ignored performance baselines passed, including large source reads, FFT, plot loading, and PNG export.
- Both optimized release binaries built successfully.
- The synchronized 0.10.0 release-version test passed. PowerShell/WiX packaging execution remains Windows-host-only because PowerShell is unavailable on the current macOS machine.

## Requirements
- Rebuild Live Scope around selected option 2: a dockable professional engineering workspace.
- Keep live waveform viewing central while making channels, trigger/display/diagnostics, events, and link state easier to access.
- Implement in the existing Rust/egui Windows desktop application.
- Preserve existing TCP/serial acquisition, trigger, recording, and offline-open behavior.
- Synchronize the application version because the change alters default layout and user workflow.
- Pass behavior checks and screenshot-based Product Design QA before handoff.

## Research Findings
- Current Live UI is isolated in `src/app/live_ui.rs`: top toolbar, left channel panel, right trigger/statistics panel, and central plot.
- Current top-level integration in `src/app.rs` switches between Offline and Live workspaces and applies the global theme.
- The selected mock uses an IDE-like model: document tabs, one compact action toolbar, signal tree, large central plot, tabbed right inspector, and a shallow bottom event/link dock.
- Open-source references informed practical patterns: PulseView aligns labels with traces; Scopy uses a single contextual settings panel; OpenHantek groups direct numeric parameters; ngscopeclient uses dockable panels/tabs and multi-view layouts.
- The repository is a Rust 2021 native `eframe`/`egui` application that started this task at version 0.8.1 on branch `feature/live-dsp-scope-v1`.
- Repository governance requires major UI/default-layout changes to synchronize version values in Cargo, PowerShell packaging, WiX, and README artifacts.

## Technical Decisions
| Decision | Rationale |
|----------|-----------|
| Build a stable docked layout with resizable egui panels rather than a free-floating docking framework | No docking dependency exists and adding one would expand scope and risk; persistent resizable/collapsible panels reproduce the selected workflow |
| Use small textual/shape status indicators drawn with egui primitives | These are native UI states and waveform markers, not missing raster assets; the selected mock contains no custom illustration assets |
| Keep bottom event/link data derived from existing session state | Avoid inventing backend event persistence; present meaningful current-session milestones and health values |
| Make inspector and bottom dock tabs interactive | These are core interactions visible in the selected design and required by the Product Design build contract |
| Default to a 1-second live history window | The simulator's 50 Hz channels became solid color bands at 10 seconds; 1 second keeps signal shape readable while retaining meaningful recent context |

## Issues Encountered
| Issue | Resolution |
|-------|------------|
| Browser reference capture was unreliable | Grounded ideation using direct primary-source image assets and recorded the visual patterns here |

## Resources
- Selected mock: `/Users/wangxuwen/.codex/generated_images/019f501a-582a-7bc1-b4b2-9b6f331ad399/exec-a19468fc-eb91-4270-a355-8168e628089b.png`
- Main Live UI: `src/app/live_ui.rs`
- App shell: `src/app.rs`
- Live state/session: `src/live/state.rs`, `src/live/session.rs`
- Repository rules: `AGENTS.md`

## Visual/Browser Findings
- Selected mock is 1440x1024, dark theme, with compact native-desktop density rather than hardware-instrument skeuomorphism.
- Approximate major regions: top menus/tabs/toolbars 160 px; left signals dock 270 px; right inspector 300 px; bottom dock 210 px; central waveform gets the remaining dominant area.
- Left dock groups Analog and Digital signals, supports search, visibility, color and current value, and has collapse/pin affordances.
- Right inspector has tabs `触发 / 显示 / 诊断`; the trigger form uses labeled rows and one full-width Arm action.
- Bottom dock has `事件 / 链路` tabs; events are timestamped rows while link metrics are compact health summaries.
- Visual tokens: near-black graphite surfaces, subtle blue-gray dividers, teal selection/connection, amber trigger marker, channel colors limited to traces and tiny swatches, red for recording/error.
- Typography is compact (roughly 13-15 px), aligned, with minimal rounding and almost no elevation.

## Native QA Findings
- The optimized Rust 1.87 build launches successfully on macOS; the debug profile aborts in the vendored Objective-C selector verifier before application code runs.
- At 1370x768, all persistent controls remain visible and the left, right, and bottom docks can collapse and reopen.
- The initial 10-second history made fast analog channels visually merge into solid bands. A 1-second default resolved this primary-use P1 issue while keeping the history field user-editable.
- Trigger/Display and Events/Link tabs switch correctly, live values update, and the four linked plot lanes remain aligned to the same time cursor.
- Passing evidence and the complete comparison history are recorded in `design-qa.md`.
