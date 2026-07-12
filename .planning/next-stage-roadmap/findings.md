# Findings: Scope Analyzer Next-Stage Product Roadmap

## Scope and Authority
- Current worktree: `/Users/wangxuwen/Documents/Scope-live-scope-shared-analysis`.
- User explicitly revoked the previously supplied `AGENTS.md` instructions for this task.
- This phase is analysis-only: no proposed product feature is to be implemented.
- Existing uncommitted changes contain the completed shared Live/offline analysis work and are part of the current-repository baseline to scan.

## Onboarding
- Primary product is a Rust/egui desktop waveform and engineering-analysis application.
- Package version is 0.10.0; runtime entrypoints are the desktop application and `scope_dsp_simulator` binary.
- Supporting surfaces include a DSP simulator and a VS Code extension; CLI/automation details still require source verification.
- No repository architecture index or repository-local `SKILL.md` was found in the bounded initial scan.
- Core functional areas are visible as `data/*`, `live/*`, `fft`, `transforms`, `repid_derived`, plot/application state, PNG/SVG/DOCX exporters, `vscode_bridge`, simulator, packaging, and protocol documentation.
- The current worktree contains the just-completed `SnapshotDataSource`, shared `ChannelPresentation`, and shared `PlotViewport`; these must be treated as current support rather than future proposals.

## Capability Evidence
- README claims support for multiple CSV/DAT variants, lazy indexed loading, multiple overlaid datasets, multi-pane layouts, two cursors, interval measurement, FFT/THD, sequence components, PLL/dq0, derived curves, annotated PNG/SVG/DOCX export, TCP/serial Live acquisition, software triggering, `.scope` recording/replay, and a deterministic simulator.
- Current measurement copy explicitly lists Y1/Y2/dY/max/min; it does not claim RMS, peak-to-peak, mean, or frequency measurement.
- Dataset configuration supports group names, checked state, line style, time synchronization, and offsets. Existing automatic alignment is phase-based at the harmonic base frequency, not yet verified as general Reference/Compare alignment.
- Live diagnostics expose CRC/protocol/display/device-drop/recording counters. Trigger history, continuous segmented recording, measurement trends, and rule-based Pass/Fail are not claimed in current product documentation.
- Existing export supports an annotation workbench, batch time windows, PNG/SVG, and multi-image DOCX.
- Cursor measurement currently computes Y1, Y2, dY, min, max, dX, and `1/dX`. The displayed `1/dX` is cursor-spacing frequency, not automatic signal-frequency estimation.
- The general measurement worker deliberately carries only first/last/min/max. There is no window RMS, mean, peak-to-peak, crest factor, or zero-crossing/PLL frequency result in the measurement table.
- The math-derived engine supports arithmetic plus a small whitelist: `pseqp`, `pseqq`, active/reactive current, positive-sequence line/phase magnitudes, instantaneous three-value RMS, average, and absolute value.
- Existing positive-sequence power presets are pointwise derived curves with manual base-value normalization. They are useful ingredients but are not yet a first-class three-phase power measurement panel with P/Q/S/PF, averaging window, units, sign convention, and validity checks.
- FFT/sequence/PLL/dq0 currently run in the offline mainline and can consume frozen Live Capture through the shared source bridge. They are not continuously updated Live instrumentation.
- FFT shows harmonic orders 0–10 and a single THD number for the cursor interval. No full-spectrum trace, waterfall, spectrogram, harmonic trend, or Live cadence control is present in the inspected UI.
- Harmonics are evaluated at a configured nominal base frequency with Hann-window phasor projection; the implementation is calibrated and tested but does not first estimate the actual fundamental frequency. Off-nominal handling exists only insofar as projection/window behavior tolerates it.
- Sequence components are fundamental-frequency phasors over a selected interval, not sliding Live sequence meters.
- SRF-PLL and abc→dq0 are implemented and tested as derived sample arrays. PLL gains/bandwidth and frequency limits are fixed in code; the UI exposes source selection but not estimated frequency, lock quality, tuning, or a Live meter cadence.
- `DataSource` provides a clean adapter boundary, cancellation, range reads, summaries, and gap-separated blocks. This makes additional file formats feasible without changing analysis UI, but each format still needs metadata/time/unit semantics and regression fixtures.
- Software trigger supports Auto/Normal/Single, one source channel, rising/falling/either edge, level, hysteresis, pre/post sample counts, and Auto timeout. It is a single-condition analog edge trigger; there is no AND/OR condition tree, pulse-width/window/runt trigger, digital pattern trigger, holdoff, or trigger qualification.
- Runtime Live state keeps only `last_capture` for display/analysis. `.scope` records can contain multiple trigger records and gap records, but the product UI does not expose a browsable trigger-event history or a Capture list/timeline.
- SCP1 V1 negotiation/configuration covers channel table, sample rate, batch size, channel mask, start/stop, heartbeat, sample/status/error frames. It intentionally has no generic parameter read/write, firmware update, register access, or multi-device clock-synchronization contract.
- Current acquisition configuration is validated against device tick rate, maximum batch size, and channel table. There is no user-facing bandwidth/utilization calculation, serial baud feasibility estimate, payload overhead estimate, or recommended batch-size assistant.
- Recording is a single manually started/stopped file backed by a bounded writer queue, with recoverable-tail and CRC/index validation. Continuous rolling recording, size/time rotation, retention policy, disk-space guard, recording catalog, tags, and search are absent.
- The desktop executable exposes two machine-readable bridge commands: `--vscode-dataset` and `--vscode-fft`. They emit JSON for CSV/DAT dataset samples/summaries or one-channel FFT; they are not a general documented CLI/API for measurement, sequence, derived math, export, Live acquisition, project loading, or Pass/Fail automation.
- The VS Code extension can open CSV/DAT, search/select channels, draw waveforms, place X1/X2, show interval measurement, FFT harmonics, and THD. It depends on the Rust executable bridge when configured and does not match the full desktop analysis/live/export surface.
- The simulator is a valuable deterministic fault-injection tool: configurable sample rate/batch/seed plus periodic frame drop, corruption, and disconnect. It is not currently a scenario-script engine, expected-result oracle, or automated acceptance runner.
- No stable public JSON schema/version envelope, batch job definition, machine-readable error taxonomy, or headless report command was found. This is the main gap between the existing bridge and a durable CLI/AI automation interface.
- Configuration is deliberately split into names, display, shortcuts, and dataset files. Dataset config stores labels, visibility, line style, sync flag, selected sync channels, and offsets—but not the actual dataset paths as a restorable project bundle.
- Recent files are path lists only. There is no atomic engineering project/workspace file that restores primary/imported sources, `.scope` links, layout, viewport/cursors, formulas, analysis selections, trigger/acquisition settings, annotations, export presets, and missing-file relocation.
- Existing Reference/Compare behavior is “multiple imported datasets overlaid by channel index,” with per-dataset time offsets and optional phase-derived automatic time alignment. There is no explicit reference role, channel mapping, robust cross-correlation/event alignment, alignment confidence, difference dataset, tolerance band, or comparison verdict.
- Custom derived curves are first-class within one dataset’s channel namespace. The inspected config schema does not persist the complete custom curve collection in the display/dataset files, and expressions do not expose cross-dataset operands needed for direct reference-minus-test curves.
- Automatic time alignment matches up to three channel names and compares phase at the configured harmonic base frequency across the full common span. It produces one scalar offset per imported dataset; it has no confidence score and is unsuitable for non-periodic events, large multi-cycle ambiguity, drift, or different sample clocks.
- A richer formula-library design document exists, but the corresponding generalized `src/derived.rs`, broad function set, mapping workflow, and `scope-formulas.json` are not present in current source. Roadmap status must follow implemented code, not that design intent.
- Repository-wide search found no product rule engine, tolerance-band model, Pass/Fail result model, automated fault diagnosis pipeline, rolling recorder/rotation manager, or recording catalog. Performance-test thresholds are internal benchmarks, not user-authored waveform acceptance rules.
- Digital channels are imported/classified, grouped separately, validated as binary for SCP1, and bitfields can be expanded/merged for specific CSV workflows. They are displayed as waveform channels and excluded from analog FFT.
- No logic-analyzer-specific step rendering, edge list, bus grouping, digital pattern search, protocol decoding (UART/SPI/I²C/CAN), glitch/pulse-width measurement, or digital trigger condition was found. “Digital logic analysis” should therefore be rated partial/basic, not complete.
- Desktop import dispatch supports `.csv`, `.dat`, and `.scope`. CSV includes standard numeric, metadata-prefixed, cloud Content, and paired/indexed ADATA/DDATA conventions; DAT is a repository-specific binary reader. There is no COMTRADE, TDMS, MDF, HDF5/MAT, VCD, Parquet, or generic plugin discovery.
- Data export is broader than image export: current code can export dataset/range CSV-like records and cursor-range/batch selections, in addition to PNG/SVG/DOCX.

## Workflow Gaps
- Power-electronics/DSP debugging has five recurring workflows: live steady-state commissioning, transient fault capture, algorithm/reference regression, long-duration soak recording, and reproducible handoff/automation.
- The four daily-loop gaps are: no Live engineering meters, no multi-Capture event history, no complete restorable project, and no acquisition bandwidth assistant.
- The next layer is engineering verification: continuous Live power quality, multi-condition triggers, explicit Reference/Compare, stateful math, rules, and traceable reports.
- Long-duration recording and cataloging are high value but should build on the project/event models instead of becoming isolated file-management features.
- Full spectrum/time-frequency, logic analysis, and more formats are valuable extensions but do not close the most common current workflow before 1.0.
- Multi-device hard sync and general device control require external hardware/firmware contracts and would expand the product boundary prematurely.

## Prioritization Decisions
- P0/0.11.0: unified measurement center including three-phase power, Capture event history, `.scopeproj` workspace restore, and bandwidth/configuration assistant.
- P1/0.12.0: continuous Live THD/sequence/dq0, multi-condition trigger, Reference/Compare with difference/tolerance, and stateful formula/filters.
- P1/pre-1.0: Pass/Fail rules, one-click evidence report, rolling segmented recording, recording library, and stable CLI/JSON/AI service boundary.
- P2/1.x: full spectrum/waterfall/time-frequency, logic-analyzer fundamentals, demand-driven formats (COMTRADE first), and software multi-record drift alignment.
- Not recommended now: hardware multi-device synchronization, generic parameter/control platform, firmware update, JTAG, and complex register editing.
