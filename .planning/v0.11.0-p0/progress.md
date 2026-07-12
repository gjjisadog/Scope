# Progress Log: Scope Analyzer 0.11.0 P0 Implementation Planning

## 2026-07-11

### Phase 1: Architecture and Dependency Refinement
- **Status:** in progress
- User approved the complete 0.11.0 P0 scope.
- Reloaded `brainstorming` and `planning-with-files` instructions.
- Created a dedicated planning scope for 0.11.0 so the completed repository scan remains unchanged.
- Established P0 non-goals and initial compatibility constraints.
- Mapped initial measurement, Capture, recording, and acquisition configuration touchpoints.
- Derived the exact SCP1 SampleBatch byte-budget formula from the encoder.
- Confirmed project persistence must use dedicated DTOs because application state contains sources, textures, caches, and worker handles.
- Completed the P0 technical design, including alternatives and the semantic approval gate.
- Completed the implementation breakdown: six milestones, ordered work packages/gates, file touchpoints, tests, performance/resource checks, risk register, release procedure, and definition of done.
- Initial structural audit confirmed 369 design lines, 700 implementation-plan lines, and 35 work packages; one overly literal audit term was corrected.
- Refined the QA check to validate concepts rather than one exact prose phrase.
- Replaced brittle prose-string QA with focused structural/compatibility checks.
- Final QA passed: 35 work packages, explicit SCP1 V1/`.scope` V1 compatibility, one design approval gate, complete definition of done, and no unplanned P1 capability in the approved scope.
- **Final result:** 0.11.0 P0 design and implementation plan ready for user approval; no product code was changed.

### Phase 6: M0 Baseline and Repository Recovery
- **Status:** in progress
- Located the intact original repository at `/Users/wangxuwen/Developer/Scope`.
- Created valid Worktree `/Users/wangxuwen/Documents/Scope-v0.11.0-p0` on branch `codex/v0.11.0-p0`.
- Mechanically migrated the completed 0.10.0 shared-analysis changes and all scoped planning artifacts.
- Verified the new Worktree has valid Git status/diff and preserved the old broken directory as a recovery copy.
- M0 formatting, `git diff --check`, and all-target compile passed.
- Standard Clippy passed with only the documented pre-existing/vendor warnings.
- Normal baseline passed: 86 library tests, 118 application tests, 1 simulator test, 0 doc tests; 5 application tests remained intentionally ignored.
- Four relevant performance baselines passed: Cloud, CSV+FFT, DAT+FFT, and PNG export. Recorded timings include Cloud full plot 1915.78 ms, CSV full plot 1661.17 ms, DAT full plot 105.50 ms, and PNG write 587.79 ms.
- **Phase 6 complete. Phase 7 M1 started.**
- Added the shared measurement engine with scalar statistics, robust frequency estimation, segmented-gap semantics, three-phase P/Q/S/PF, and analytical tests.
- Added the pure SCP1 link-budget engine with exact encoded-byte accounting, serial policy, TCP advisory mode, batch suggestion, and encoder-equality tests.
- Added the explicit `.scopeproj` V1 DTO/schema skeleton with stable IDs, reference validation, finite-number checks, safe relative paths, size limits, and round-trip tests.
- Fixed the workspace constructor to produce a valid 1×1 default and hardened project paths against Windows drive and UNC forms on non-Windows hosts.
- M1 verification passed: 103 library tests, zero failures. Phase 7 is complete; Phase 8 M2 integration started.
- M2 completed: Offline/frozen-Capture tables now use the shared gap-aware engine; Live measurements refresh at 4 Hz from an exact recent-sample snapshot; three-phase P/Q₁/S/PF bindings work in Offline and Live; the serial/TCP budget assistant reports frame size, throughput, latency, utilization, safe batch suggestions, and guarded expert override.
- M2 full verification passed with 104 library tests, 118 application tests, and 1 simulator test.
- M3 completed: bounded pinned-aware Capture history, all-trigger delivery per incoming batch, Live selection/pinning/clear confirmation, `.scope` trigger navigation, and atomic companion `.scope` V1 Capture writing.
- M3 verification passed with 108 library tests including exact recent snapshot, pinned eviction, all-trigger delivery, and companion replay tests. Phase 10 M4 project integration started.
- M4 completed: explicit `.scopeproj` V1 conversion, atomic replace-safe writes, staged source opening, missing-source relocation, metadata mismatch reporting, multi-dataset/workspace/analysis/Live/export annotation restore, dirty tracking, 30-second autosave recovery, and Live-only Capture projects.
- Project integration tests cover full offline workspace restore without auto-connect and companion Capture asset materialization/restoration.
- Synchronized Cargo, release test, WiX, PowerShell packaging, README, and artifact names to 0.11.0.
- M5 gates passed: formatting, `git diff --check`, all-target compile, baseline-standard Clippy, 110 library tests, 120 application tests, 1 simulator test, and all 4 performance tests.
- Performance results: Cloud full plot 1306.30 ms, CSV full plot 1668.07 ms, DAT full plot 106.74 ms, PNG write 617.14 ms. No relevant baseline regression.
- **0.11.0 P0 implementation complete.** SCP1 V1 and `.scope` V1 remain unchanged; `.scopeproj` is a new separate schema.
- Native macOS smoke test compiled 0.11.0 and entered the GUI event loop successfully; the process was then intentionally stopped.
- Final Windows-safe project replacement path was re-tested after adding backup/restore semantics; the atomic overwrite round trip passes.

## 2026-07-12

### Release-candidate reliability and real-workflow validation
- Re-audited the completed P0 implementation against the approved engineering workflow instead of relying on the earlier completion declaration.
- Added a real TCP simulator end-to-end test covering SCP1 streaming, deliberate dropped batches, automatic triggering, multiple Capture-history entries, `.scope` recording, clean close, recorded gaps/triggers, replay segmentation, and replayed frequency/RMS measurement.
- Moved production Live engineering measurements to a 4 Hz background worker. Snapshot/config signatures reject stale results after presentation or binding changes; the UI reports refresh activity/age.
- Added a 131,072-point, six-channel Live measurement performance gate. Measured p95 is 128.08 ms against the 250 ms refresh interval.
- Strengthened Live/Freeze consistency tests: Offline and Live share presentation scaling and produce matching RMS values; stale background results are rejected and current results accepted.
- Made Critical-bandwidth expert override one-shot and reset it after start; added a visible session notice and a focused regression test.
- Added staged-project failure coverage proving a corrupt imported source cannot mutate the active workspace.
- Restored recording-backed trigger references and selected trigger navigation when reopening `.scopeproj` projects whose primary source is `.scope`.
- Added user-facing documentation for `.scopeproj` V1, engineering measurements, Capture history, and the SCP1 acquisition bandwidth assistant; linked these from README and in-app Help.
- Final format and whitespace checks passed. Full regression passed: 112 library tests (1 ignored performance gate), 122 application tests (5 intentionally ignored), and 1 simulator test; no failures.
- Real performance suite passed: Cloud full plot 1115.35 ms, CSV full plot 1269.62 ms, DAT full plot 78.77 ms, PNG write 573.09 ms, Live measurement p95 128.08 ms.
- Standard `cargo clippy --all-targets` passes with existing warnings. A strict `-D warnings` run remains unsuitable as a release gate because pre-existing/vendor future-compatibility and style warnings are still present.
- Native macOS GUI startup entered and remained in the event loop without crashing. Interactive click-through automation was unavailable in this environment; Windows packaging/native execution also requires a Windows runner or machine.
- Completion audit found and closed a Capture workflow gap: the Live event panel now supports previous/next navigation, editing durable label/note metadata, and removing the selected event. Model tests verify navigation boundaries, metadata persistence state, pinned edits, removal, and non-stale selection.
- Strengthened recording-backed project coverage from one trigger to two trigger records, including restoring the second selected event and its auto-timeout metadata.
- Installed the `x86_64-pc-windows-gnu` Rust target and passed `cargo check --all-targets` for Windows. A full optimized Windows link also passed, producing a PE32+ GUI `scope_analyzer.exe` (11 MiB, SHA-256 `2f885e9ec55b2df33a4f7ae74caba29e15b4f27ca242fa16fdacdf6d439d6121`) and console `scope_dsp_simulator.exe` (380 KiB, SHA-256 `19c07c7b4cec38a7a7b48f87d3809f2a95998218c84d9013a8c7f7760b463ff5`). Native execution and WiX MSI installation still require Windows.
- Replaced blocking UI project saves with a background save worker shared by explicit Save/Save As and autosave. The project menu exposes progress and cancellation, concurrent saves are rejected, and the UI schedules completion polling without user input.
- Cancellation now reaches the companion `.scope` batch-writing loop. A cancelled large Capture removes its temporary asset and never publishes project JSON; successful background Capture save/reopen and newer-autosave recovery are covered directly.
- Post-change regression passes with 113 library tests plus 1 intentionally ignored performance gate, 125 application tests plus 5 intentionally ignored tests, and 1 simulator test. Standard Clippy and Windows all-target checks pass.
- Rebuilt optimized Windows PE artifacts after the asynchronous-save changes: `scope_analyzer.exe` SHA-256 `6cb866bf9d7795d66203d940b64d1b5911928cf8ff6ab0c20e366c17da2db957`; `scope_dsp_simulator.exe` SHA-256 `9fa7a8fbca664ca2b058a853d300d320be233ca5c17814ecd421b9acb6b45473`.
- Re-ran the macOS native startup smoke after the save-worker change; the GUI entered the event loop and remained stable for 5 seconds before intentional termination.
- Rebuilt the Worktree GitNexus index including the large `src/app.rs`. Focused upstream impact is LOW for `start_project_save` (3 symbols), `write_project_input` (7), cancellable Capture writing (10), and Capture removal (2). Whole-branch impact is CRITICAL—328 changed symbols and 145 affected flows—so full regression and native release gates remain mandatory.
