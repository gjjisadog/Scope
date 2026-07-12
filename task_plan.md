# Task Plan: Live Scope Shared Analysis Architecture

## Goal
Enable an immutable Live Capture to enter the existing offline analysis workflow, then share channel presentation, plot interaction, annotation, and export behavior between Live and replay without destabilizing acquisition.

## Current Phase
Complete — implementation and verification audit passed

## Requirements
- [x] Add `SnapshotDataSource` for a frozen Live snapshot/capture.
- [x] Provide a user-visible “freeze current Capture → offline analysis” workflow.
- [x] Reuse cursor measurements, FFT/THD, and sequence analysis for triggered Captures.
- [x] Extract shared `ChannelPresentation` for names, colors, scales, and pane assignment.
- [x] Extract shared `PlotViewport` for cursors, zoom, pan, gap handling, and multi-pane behavior.
- [x] Reuse mainline annotation and export workflows from frozen Live data.
- [x] Preserve SCP1 acquisition, trigger, recording, and replay semantics.
- [x] Add targeted regression tests and pass formatting, clippy, normal tests, and relevant ignored baselines.
- [x] Keep version metadata synchronized because this adds user-facing workflow and configuration behavior.

## Phases

### Phase 1: Architecture Audit
- [x] Map callers and impact of `DataSource`, Live snapshots, channel state, plot state, and export entrypoints.
- [x] Define immutable snapshot conversion and shared state ownership.
- **Status:** complete

### Phase 2: Snapshot Analysis Bridge
- [x] Implement `SnapshotDataSource` with metadata, range reads, decimation, summaries, and validation.
- [x] Convert frozen/triggered Live snapshots without requiring a temporary file.
- [x] Load the snapshot as the primary offline dataset and initialize cursors/view/channel state.
- **Status:** complete

### Phase 3: Shared Analysis
- [x] Add Live actions for analyzing current/frozen Capture.
- [x] Verify cursor measurement, FFT/THD, and sequence analysis against known signals.
- **Status:** complete

### Phase 4: Shared Presentation and Viewport
- [x] Extract `ChannelPresentation` and migrate offline + Live callers.
- [x] Extract `PlotViewport` and migrate shared cursor/zoom/pan/gap/multi-pane state.
- **Status:** complete

### Phase 5: Annotation and Export
- [x] Route frozen Capture through existing image/data/DOCX annotation and export paths.
- [x] Verify export preview initialization uses the shared source, labels, scales, panes, cursors, and gap-separated prepared series.
- **Status:** complete

### Phase 6: Verification and Delivery
- [x] Run GitNexus change detection/impact review.
- [x] Run fmt, clippy, normal tests, relevant ignored performance tests, and release version sync checks.
- [x] Audit every requirement against source/test/runtime evidence.
- **Status:** complete

## Decisions
| Decision | Rationale |
|----------|-----------|
| Analyze immutable snapshots, never a mutating ring buffer | Keeps one analysis request internally consistent and avoids coupling analysis workers to acquisition timing |
| Reuse the existing `DataSource`-based offline pipeline | Avoids duplicate FFT, measurements, sequence, export, and caching implementations |
| Preserve `LiveScopeState` as the acquisition boundary | Shared UI/data abstractions must not flatten protocol/session state into `ScopeApp` |
| Preserve unrelated dirty files (`AGENTS.md`, `CLAUDE.md`, `.claude/skills`) | They predate this task and belong to the user |

## Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|
| `cargo test` remained idle for two minutes with no `rustc` or test child | 1 | Interrupted the stale Cargo runner; retry with a fresh process and visible output after confirming no compiler child exists |
| First post-worktree `cargo check` found binary/library import boundary and one multiline viewport reference missed by mechanical migration | 1 | Import shared viewport through `scope_analyzer` and fix the remaining nested field access |
| Live viewport bounds used private `PlotBounds` fields | 1 | Use the public `min()`/`max()` accessors exposed by the vendored egui_plot API |
| Tried to pass multiple positional test filters to `cargo test` | 1 | Cargo accepts one filter; use the complete library test target, then a separate binary integration-test filter |
| Integration test initially assumed `ScopeApp: Default` | 1 | Extract deterministic `from_recent_state` initialization and add a test-only constructor without filesystem recent-state reads |
| Strict clippy rejected a direct floating-span comparison | 1 | Compare the absolute span against epsilon, preserving the viewport invariant and satisfying the strict lint |
| Repository-wide `clippy -D warnings` exposes 30+ pre-existing strict lints in the large app/export modules | 1 | Fix all new lints from this change, then use the repository's standard `clippy --all-targets` gate; retain strict-output evidence as baseline debt rather than widening this refactor |
| Windows release preflight requires PowerShell, which is unavailable on the current macOS host | 1 | Run its constituent portable gates locally: synchronized-version unit test, fmt, standard clippy, full tests, ignored performance baselines, and both release binaries; leave Windows packaging itself for a Windows release host |
