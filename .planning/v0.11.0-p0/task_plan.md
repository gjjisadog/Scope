# Task Plan: Scope Analyzer 0.11.0 P0 Implementation Planning

## Goal
Implement the user-approved complete 0.11.0 P0 scope: unified engineering measurements, Capture event history, complete `.scopeproj` workspace restore, and acquisition bandwidth/configuration assistance, following the approved staged design and verification gates.

## Current Phase
Phase 11 — M5 integration and release (complete)

## Approved Scope
1. Unified measurements for Offline, frozen Capture, and Live: RMS, average, positive/negative/absolute peak, peak-to-peak, actual frequency, and three-phase P/Q/S/PF.
2. Bounded Live Capture history and `.scope` trigger-event navigation.
3. Versioned `.scopeproj` project file with recovery and missing-source relocation.
4. SCP1 acquisition bandwidth calculator and configuration assistant.

## Non-Goals
- Continuous Live THD/sequence/dq0.
- Multi-condition trigger.
- Reference/Compare difference and tolerance workflow.
- Stateful formula/filter expansion.
- Pass/Fail, rolling recording, recording library, or public CLI redesign.
- Hardware synchronization or generic device control.

## Phases

### Phase 1: Refine Existing Architecture
- [x] Map exact state/data/job/UI/persistence touchpoints for the four P0 epics.
- [x] Define shared IDs, ownership boundaries, migration rules, and cross-epic dependencies.
- **Status:** complete

### Phase 2: Technical Design
- [x] Specify measurement semantics and algorithms.
- [x] Specify Capture/event model and replay mapping.
- [x] Specify `.scopeproj` schema, restore transaction, autosave, and relocation.
- [x] Specify bandwidth formulas, guard levels, and UI behavior.
- **Status:** complete

### Phase 3: Implementation Breakdown
- [x] Produce ordered work packages with files, APIs, tests, and completion criteria.
- [x] Define incremental integration and rollback points.
- **Status:** complete

### Phase 4: Verification and Release Plan
- [x] Define unit, integration, performance, compatibility, and native QA matrices.
- [x] Define version/documentation/release gates.
- **Status:** complete

### Phase 5: Plan QA and Delivery
- [x] Confirm every approved capability has implementation tasks and acceptance evidence.
- [x] Deliver design and implementation plan for approval before coding.
- **Status:** complete

### Phase 6: M0 Baseline and Repository Recovery
- [x] Recover a valid Git worktree and preserve the completed 0.10.0 changes.
- [x] Run the complete 0.10.0 baseline verification in the recovered worktree.
- [x] Freeze implementation evidence and performance/test counts.
- **Status:** complete

### Phase 7: M1 Pure Domain Foundations
- [x] Implement and test the shared measurement engine.
- [x] Implement and test the SCP1 link-budget engine.
- [x] Add project IDs and schema DTO skeleton with validation tests.
- **Status:** complete

### Phase 8: M2 Measurements and Bandwidth UI
- [x] Integrate engineering measurements into Offline and frozen Capture.
- [x] Add three-phase P/Q/S/PF configuration and results.
- [x] Add coalesced Live measurements and the bandwidth assistant.
- **Status:** complete

### Phase 9: M3 Capture History
- [x] Implement bounded CaptureHistory and all-completed-trigger delivery.
- [x] Add Live event navigation and `.scope` trigger-event navigation.
- [x] Add companion `.scope` Capture asset writer.
- **Status:** complete

### Phase 10: M4 Project Save/Restore
- [x] Implement `.scopeproj` conversion, validation, atomic save, and staged restore.
- [x] Add relocation, annotations, Capture assets, dirty state, and autosave recovery.
- **Status:** complete

### Phase 11: M5 Integration and Release
- [x] Run cross-feature, compatibility, performance, native, and release verification.
- [x] Synchronize 0.11.0 version metadata and documentation.
- **Status:** complete

## Initial Decisions
| Decision | Rationale |
|---|---|
| Build four vertical slices on shared IDs and services, not four UI-only features | Enables Live/offline consistency and project persistence without duplicating algorithms |
| Keep SCP1 V1 and individual `.scope` format unchanged in 0.11.0 | All approved capabilities can be host-side; avoids unnecessary device/recording compatibility risk |
| Introduce `.scopeproj` as a separate versioned application file | A project references raw recordings rather than mutating them |
| Ship 0.11.0 through internal milestones M1–M5 | Keeps the complete P0 release reviewable and testable despite four epics |

## Errors Encountered
| Error | Attempt | Resolution |
|---|---:|---|
| None | — | — |
| Full-file `dd | rg` scan of large `src/app.rs` stalled on the current filesystem | 1 | Interrupted safely; rely on already captured state/touchpoints and bounded line reads, and keep the plan at module/API granularity rather than fragile line numbers |
| First phase-status update used an incorrect expected status token | 1 | Read the current plan and applied a narrowly matched correction; no plan content was lost |
| First structural QA searched for literal `missing-source`, while the plan uses “missing source” wording | 1 | Correct the audit term; the required relocation design and work package are already present |
| Second structural QA still assumed the phrase “missing source”; documents consistently use “missing file”/“source relocation” | 1 | Audit the actual concepts (`missing file`, `relocation`) separately instead of enforcing prose wording |
| Third literal QA term ignored Markdown backticks around `.scope` | 1 | Stop the brittle combined loop; use focused regex checks for approved scope, compatibility, work-package count, and remaining checklist items |
| Migration command ran `git status` from the old broken Worktree after rsync | 1 | Verify rsync results and Git state from the newly created valid Worktree; migration succeeded and the old directory remains untouched |
| Targeted Cargo test command supplied two positional filters | 1 | Cargo accepts one filter; run the complete library test target, which is fast and covers both new pure modules |
| Three-phase test moved its time vector while a closure still borrowed it | 1 | Materialize all six phase vectors before constructing the SampleBlock and moving the time vector |
| Payload-limit test expected batch 15 although the documented suggestion policy chooses the smallest safe batch | 1 | Correct expectation to batch 1; retain Critical severity for the current oversized batch |
| `ProjectWorkspace` derived Rust `Default`, producing zero layout axes despite serde defaults | 1 | Replace the derived implementation with an explicit valid 1×1 workspace default and add cross-platform absolute-path rejection coverage |
| First application compile after replacing the narrow measurement helper found a Live UI test using the legacy helper | 1 | Keep a test-only compatibility wrapper around the shared measurement engine |
| Legacy Live UI assertions accessed `AutoMeasurement.min/max` directly | 1 | Retain compatibility fields while the production table reads the richer shared statistics |
| Capture-history test kept the insert outcome instead of its Capture ID | 1 | Extract `.id` before comparing selected history state |
| Strict Clippy with `-D warnings` failed on 30+ pre-existing export/render warnings outside the approved P0 scope | 1 | Remove all warnings introduced by 0.11.0, then use the repository baseline standard Clippy gate; standard Clippy passes with only the documented pre-existing/vendor warnings |
| Full-worktree formatting/status scans intermittently stalled on the current filesystem | 2 | Interrupt safely, verify the changed 0.11.0 paths directly, and rely on the successful full format/diff gates already run before the final isolated atomic-save change |
