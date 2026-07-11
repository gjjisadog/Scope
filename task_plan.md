# Task Plan: Live Scope Dockable Engineering Studio

## Goal
Implement the selected Product Design option 2 in the existing Rust/egui application without changing live acquisition semantics, then verify behavior and visual fidelity against the selected mockup.

## Current Phase
Complete

## Phases

### Phase 1: Requirements & Repository Discovery
- [x] Resolve selected visual target
- [x] Load repository governance and Product Design build/QA rules
- [x] Map current Live UI state, event data, and layout integration points
- **Status:** complete

### Phase 2: UI Architecture & Version Plan
- [x] Define egui layout/state changes
- [x] Decide feasible fidelity adaptations for a native immediate-mode UI
- [x] Plan synchronized version bump
- **Status:** complete

### Phase 3: Implementation
- [x] Implement dockable-style Live workspace, tabs, toolbar, signal tree, inspector, and event/link dock
- [x] Add working collapse/tab/layout interactions
- [x] Synchronize version 0.9.0 across packaging files and README
- [x] Add/update tests for durable UI state and helpers
- **Status:** complete

### Phase 4: Verification & Design QA
- [x] Run formatting, clippy, and tests
- [x] Launch simulator and native app in the target Live state
- [x] Capture implementation at the available 1370x768 native desktop viewport
- [x] Compare source and implementation together; fix P0/P1/P2 findings
- [x] Save passing design-qa.md
- **Status:** complete

### Phase 5: Delivery
- [x] Review diff and working tree
- [x] Confirm planning records and QA evidence
- [x] Hand off changed files, verification, and remaining P3 polish
- **Status:** complete

## Key Questions
1. Which parts of the selected dockable studio can be implemented with egui 0.27 without destabilizing streaming?
2. How can the selected connected/streaming state be reproduced deterministically for screenshot QA?
3. Which UI states should persist as display settings versus remain session-only?

## Decisions Made
| Decision | Rationale |
|----------|-----------|
| Treat displayed ideation image 2 as source truth | User selected option 2 unambiguously from the most recent three-image set |
| Implement in the existing native app rather than a separate web prototype | The requested product is the current Rust/egui Live Scope and the user asked to design that interface |
| Bump to 0.9.0 | Default layout and user-facing workflow materially change, triggering AGENTS.md version rules |
| Preserve acquisition/session modules | The task is UI redesign; protocol, buffering, triggering, and recording semantics are out of scope |

## Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|
| In-app browser tabs detached during visual reference capture | 1-2 | Used authoritative project documentation and downloaded the referenced open-source screenshots directly for ideation grounding |
| `cargo` was not available on the non-login shell PATH | 1 | Use the installed toolchain at `$HOME/.cargo/bin/cargo` explicitly |
| Rust 1.97 rejected an existing ambiguous float inference in export preview code | 1 | Add the explicit `f32` type already implied by egui painter APIs |
| First Live UI compile found six mutable/immutable self-borrow conflicts | 1 | Resolve translated labels before borrowing individual UI state fields mutably |
| `cargo test --quiet` remained idle with no child test process for over six minutes | 1 | Interrupt the stalled runner, verify targeted tests first, then rerun the suite with visible output to identify any specific blocker |
| Full test suite found the release-sync unit test still expected 0.8.1 | 1 | Update the test's authoritative version constant to 0.9.0 and rerun the suite |
| Rust 1.97 build aborted during macOS window creation in `icrate` NSScreen ABI code | 1 | Reuse the repository's preloaded Rust 1.87 toolchain that produced the known-good native release binary |
| First Rust 1.87 rebuild hit a transient stale NFS handle reading Cargo.toml | 1 | Confirm the file remains readable and retry the same deterministic build once |
| Rust 1.87 debug build still enabled the Objective-C selector type check and aborted | 1 | Build the optimized release profile, matching the existing working native binary; native QA then launched successfully |
| Raw native executable was not discoverable by the Computer Use accessibility service | 1 | Wrap the QA binary in a temporary macOS `.app` bundle with a unique bundle identifier |

## Notes
- Source visual: `/Users/wangxuwen/.codex/generated_images/019f501a-582a-7bc1-b4b2-9b6f331ad399/exec-a19468fc-eb91-4270-a355-8168e628089b.png`
- Do not modify live protocol/data behavior unless required to expose existing state to the redesigned UI.
