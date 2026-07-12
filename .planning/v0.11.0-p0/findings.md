# Findings: Scope Analyzer 0.11.0 P0 Implementation Planning

## Baseline
- Current product baseline is the valid current repository Worktree `/Users/wangxuwen/Documents/Scope-v0.11.0-p0` on `codex/v0.11.0-p0`, with shared `SnapshotDataSource`, `ChannelPresentation`, `PlotViewport`, and gap-aware plotting/export.
- User selected the complete P0 scope (“A”).
- Planning only; no product code changes are authorized in this phase.
- SCP1 V1 and `.scope` compatibility should be preserved unless planning proves a blocker.

## Cross-Epic Architecture
- The shared service boundary should be: immutable sample window → measurement result; Capture/event → immutable SnapshotDataSource; project document → validated restore transaction; channel table/configure → link budget result.
- Stable project references need explicit dataset IDs, channel identities, Capture IDs, and relative paths rather than array positions alone.
- Project loading must stage and validate all referenced sources before replacing the active workspace.
- Live analytics must never execute on the acquisition thread; use bounded snapshots and a coalescing worker cadence.

## Open Design Details
- Measurements currently use `AutoMeasurement`, measurement job keys/caches/workers, and the Analysis panel in `src/app.rs`; Live has a separate latest-value surface. The plan must replace the narrow result model without regressing async cancellation/cache behavior.
- Live Capture ownership is localized in `LiveScopeState::last_capture`, trigger event handling, snapshot conversion, and the Live document/event panels. This is a tractable replacement with a dedicated bounded history model.
- Existing `.scope` writes trigger records and can recover them; event navigation can be added without changing the recording format.
- Acquisition inputs are `Configure { sample_rate_hz, batch_samples, channel_mask }`, the negotiated `ChannelTable`, and `TransportConfig::Serial { baud }`/TCP. The bandwidth engine can remain a pure host-side function.
- Bandwidth assistance must be advisory for TCP and deterministic for serial. Protocol legality and link-budget policy should remain separate result layers.
- SCP1 SampleBatch payload overhead is exactly 20 bytes before channel IDs, plus 2 bytes per selected channel, plus `batch_samples × Σ(selected wire widths)` sample bytes; each frame then adds the existing 28-byte frame header and 4-byte CRC.
- Serial wire utilization must multiply framed bytes by 10 bits/byte for 8-N-1 and add a configurable safety policy (recommended warning at 70%, block-by-default at 90%, expert override above that). TCP reports payload and host-side throughput estimates but no guaranteed physical-link utilization.
- The current `ScopeApp` state mixes persistent domain settings and transient caches/workers. `.scopeproj` serialization must use explicit DTOs; it must never serialize `ScopeApp` directly.
- Measurement semantics selected for planning: true RMS including DC; robust hysteretic rising-crossing frequency; total active P; fundamental reactive Q; effective RMS apparent S; true PF=P/S.
- Live measurement defaults to a 10-nominal-cycle window and 5 Hz coalesced refresh, isolated from acquisition.
- Capture history defaults to 100 entries and 128 MiB, evicting oldest unpinned entries.
- Durable in-memory Captures use companion `.scope` V1 assets instead of embedding samples in JSON or changing `.scope`.
- `.scopeproj` uses explicit DTOs, stable source/channel/Capture references, partial fingerprints, staged source opening, and atomic commit.
- Project restore never auto-connects or starts Live acquisition.
- Serial budget policy is Safe ≤70%, Warning 70–90%, Critical >90%; expert override applies only to budget policy, not protocol-invalid configuration.
- Recovered-worktree baseline: 205 non-ignored tests and all four release-relevant performance baselines pass before 0.11.0 product edits.
