# Findings & Decisions

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
