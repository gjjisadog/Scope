# DSP Live Scope V1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a production-usable live DSP oscilloscope path with SCP1 serial/TCP protocol, bounded acquisition, software triggering, recoverable `.scope` recording, offline playback, simulator, egui integration, and regression coverage.

**Architecture:** Keep the existing file-oriented analyzer intact. New reusable live-domain modules live under `src/live/`; `ScopeApp` owns one composed `LiveScopeState`, and a small `src/app/live_ui.rs` adapter renders it. The acquisition worker owns the transport, emits validated batches through bounded channels, and records only validated protocol frames; completed `.scope` files enter the existing offline pipeline through a separate `ScopeRecordingDataSource`.

**Tech Stack:** Rust 2021, existing eframe/egui/egui_plot 0.27 patches, std networking and threads, `serialport`, `crossbeam-channel`, serde/serde_json, existing `DataSource`, Rust unit/integration tests.

---

## File Structure

- Create `src/lib.rs`: expose reusable `data` and `live` modules to the desktop and simulator binaries.
- Create `src/live/mod.rs`: focused public exports only.
- Create `src/live/protocol.rs`: SCP1 frames, CRC32C, payload messages, channel table, sample decoder, incremental stream parser.
- Create `src/live/buffer.rs`: bounded multi-channel history, gaps, display snapshots and min-max decimation.
- Create `src/live/trigger.rs`: Auto/Normal/Single software-trigger state machine and frozen captures.
- Create `src/live/transport.rs`: serial/TCP configuration, enumeration and `Read + Write` transport creation.
- Create `src/live/session.rs`: worker state machine, bounded commands/events, heartbeats and acquisition statistics.
- Create `src/live/recording.rs`: `.scope` writer, record CRC, clean index and interrupted-tail scanner.
- Create `src/live/scope_source.rs`: independent offline `DataSource` adapter.
- Create `src/live/simulator.rs`: deterministic SCP1 TCP device and signal generator.
- Create `src/live/state.rs`: composed `LiveScopeState`; no live fields are flattened into `ScopeApp`.
- Create `src/app/live_ui.rs`: toolbar, live channel/trigger panels and plotting adapter.
- Create `src/bin/scope_dsp_simulator.rs`: simulator CLI.
- Modify `src/main.rs`: use library data/live modules and preserve all renderer fallbacks.
- Modify `src/app.rs` and `src/app/state.rs`: add one live state and minimal offline/live routing.
- Modify `src/data/mod.rs`: export `.scope` source without changing existing sources.
- Modify Cargo/package/README files: dependencies, 0.8.0 version sync, live usage and protocol references.
- Update `docs/implementation/live-scope-progress.md` after every milestone.

## Task 1: SCP1 Frame Codec and Incremental Parser

**Files:**
- Create: `src/lib.rs`
- Create: `src/live/mod.rs`
- Create: `src/live/protocol.rs`
- Modify: `src/main.rs`
- Test: inline tests in `src/live/protocol.rs`

- [ ] **Step 1: Write failing CRC and golden-frame tests**

Add tests that define the public API before implementation:

```rust
#[test]
fn crc32c_matches_castagnoli_check_value() {
    assert_eq!(crc32c(b"123456789"), 0xe306_9283);
}

#[test]
fn frame_round_trip_matches_golden_layout() {
    let frame = Frame::new(0x14, 3, 7, 11, 13, 17_u64.to_le_bytes().to_vec());
    let encoded = frame.encode().unwrap();
    assert_eq!(&encoded[..4], b"SCP1");
    assert_eq!(u32::from_le_bytes(encoded[12..16].try_into().unwrap()), 8);
    assert_eq!(Frame::decode(&encoded).unwrap(), frame);
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo +1.87.0 test --lib live::protocol::tests::crc32c_matches_castagnoli_check_value
```

Expected: compile failure because `live::protocol`, `crc32c`, and `Frame` do not exist.

- [ ] **Step 3: Implement frame primitives and checked limits**

Implement these exact public types and constants:

```rust
pub const FRAME_MAGIC: [u8; 4] = *b"SCP1";
pub const PROTOCOL_VERSION: u8 = 1;
pub const FRAME_HEADER_LEN: usize = 28;
pub const FRAME_CRC_LEN: usize = 4;
pub const MAX_PAYLOAD_LEN: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub message_type: u8,
    pub flags: u16,
    pub sequence: u32,
    pub session_id: u32,
    pub timestamp_ticks: u64,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(message_type: u8, flags: u16, sequence: u32,
               session_id: u32, timestamp_ticks: u64, payload: Vec<u8>) -> Self;
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError>;
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError>;
}

pub fn crc32c(bytes: &[u8]) -> u32;
```

Use checked additions for total frame length, reject payloads over 1 MiB, validate magic/version/exact length, and cover header bytes 4..28 plus payload with CRC32C.

- [ ] **Step 4: Write fragmented/noisy/corrupt stream tests and verify RED**

```rust
#[test]
fn decoder_handles_fragmentation_noise_and_crc_recovery() {
    let first = Frame::new(0x14, 0, 1, 9, 0, vec![1]).encode().unwrap();
    let mut corrupt = Frame::new(0x20, 0, 2, 9, 4, vec![2, 3]).encode().unwrap();
    corrupt[29] ^= 0x55;
    let last = Frame::new(0x15, 0, 3, 9, 0, vec![4]).encode().unwrap();
    let mut decoder = FrameDecoder::default();
    decoder.push(b"noiseS");
    decoder.push(&first[..9]);
    decoder.push(&first[9..]);
    decoder.push(&corrupt);
    decoder.push(&last);
    let frames = decoder.drain_frames();
    assert_eq!(frames.iter().map(|f| f.sequence).collect::<Vec<_>>(), vec![1, 3]);
    assert!(decoder.stats().crc_errors >= 1);
    assert!(decoder.stats().discarded_bytes >= 5);
}
```

Run the targeted test; expected failure because `FrameDecoder` does not exist.

- [ ] **Step 5: Implement incremental resynchronizing parser**

Provide:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecoderStats {
    pub decoded_frames: u64,
    pub crc_errors: u64,
    pub malformed_headers: u64,
    pub discarded_bytes: u64,
}

#[derive(Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    ready: std::collections::VecDeque<Frame>,
    stats: DecoderStats,
}

impl FrameDecoder {
    pub fn push(&mut self, bytes: &[u8]);
    pub fn drain_frames(&mut self) -> Vec<Frame>;
    pub fn stats(&self) -> DecoderStats;
}
```

On a bad candidate discard one byte from its magic, never the entire input. Retain up to three possible magic-prefix bytes when no full magic exists.

- [ ] **Step 6: Verify Task 1**

Run:

```bash
cargo +1.87.0 test --lib live::protocol
cargo +1.87.0 fmt --check
```

Expected: all protocol tests pass and formatting is clean.

- [ ] **Step 7: Update progress and commit**

Commit `src/lib.rs`, `src/live/{mod,protocol}.rs`, `src/main.rs`, Cargo lock changes if any, and progress docs:

```bash
git commit -m "feat: add SCP1 frame codec"
```

## Task 2: SCP1 Messages, Channel Tables, and Sample Decoding

**Files:**
- Modify: `src/live/protocol.rs`
- Test: inline protocol tests

- [ ] **Step 1: Write failing message round-trip and validation tests**

Cover `Hello`, `HelloAck`, `ChannelTable`, `Configure`, `CommandResult`, `Ping`, `Status`, and `SampleBatch`. Required failure cases: invalid UTF-8, duplicate/out-of-range channel IDs, zero tick rate/sample period/count, unknown format, selected-channel duplicates, revision mismatch, truncated/extra sample bytes, and checked-arithmetic overflow.

Use this wished-for API:

```rust
let payload = Message::Hello(Hello {
    client_capabilities: 0b111,
    max_payload: MAX_PAYLOAD_LEN as u32,
    client_name: "ScopeAnalyzer".into(),
});
let encoded = payload.encode_payload().unwrap();
assert_eq!(Message::decode(MSG_HELLO, &encoded).unwrap(), payload);
```

- [ ] **Step 2: Run tests and verify RED**

Run the first message test and confirm it fails because `Message` is absent.

- [ ] **Step 3: Implement exact V1 message model**

Add constants for message types and these focused types:

```rust
pub enum WireFormat { I16, I32, F32, U8 }
pub enum ChannelKind { Analog, Digital }
pub struct ChannelDescriptor { pub channel_id: u16, pub kind: ChannelKind,
    pub wire_format: WireFormat, pub scale: f32, pub offset: f32,
    pub unit: String, pub name: String }
pub struct ChannelTable { pub revision: u32, pub channels: Vec<ChannelDescriptor> }
pub struct DecodedSampleBatch { pub revision: u32, pub first_sample_index: u64,
    pub sample_period_ticks: u32, pub timestamp_ticks: u64,
    pub channel_ids: Vec<u16>, pub channels: Vec<Vec<f32>>, pub raw_frame: Vec<u8> }
```

Keep byte readers private, length-bounded, and exact-consumption checked. Decode integer engineering values with `raw * scale + offset`; preserve `f32` values.

- [ ] **Step 4: Verify Task 2 and commit**

Run protocol tests, formatting, update progress, then commit:

```bash
git commit -m "feat: add SCP1 messages and sample decoding"
```

## Task 3: Bounded Live Buffer and Software Trigger

**Files:**
- Create: `src/live/buffer.rs`
- Create: `src/live/trigger.rs`
- Modify: `src/live/mod.rs`
- Test: inline tests in both modules

- [ ] **Step 1: Write failing bounded-buffer tests**

Tests must prove aligned channel lengths, capacity eviction, gap boundaries, sample-index continuity, snapshot time conversion, and min-max output preserving spikes.

```rust
let mut buffer = LiveBuffer::new(vec![0, 1], 5, 1_000_000).unwrap();
buffer.push_batch(batch(10, 100, 10, &[&[1., 2., 3.], &[4., 5., 6.]])).unwrap();
buffer.push_gap(13, 2, GapReason::SequenceLoss);
buffer.push_batch(batch(15, 130, 10, &[&[7., 8.], &[9., 10.]])).unwrap();
assert_eq!(buffer.len(), 5);
assert_eq!(buffer.gaps(), &[13]);
```

- [ ] **Step 2: Verify buffer RED, then implement minimal buffer**

Use a shared `VecDeque<u64>` sample-index/time axis and one `VecDeque<f32>` per channel. Reject channel/layout mismatch before mutation. Provide `snapshot(max_points)` returning line segments split at gaps and a min-max envelope.

- [ ] **Step 3: Write failing trigger state-machine tests**

Cover rising, falling, either, hysteresis chatter suppression, pre/post sample counts, gap reset, Normal waiting, Single disarm, and Auto timeout capture.

```rust
let mut trigger = TriggerEngine::new(TriggerConfig {
    mode: TriggerMode::Single, edge: TriggerEdge::Rising, source_channel: 0,
    level: 0.0, hysteresis: 0.2, pre_samples: 2, post_samples: 2,
    auto_timeout_samples: 100,
});
assert!(trigger.feed(batch_with_source(&[-1.0, -0.2, 0.2, 1.0])).is_some());
assert!(!trigger.is_armed());
```

- [ ] **Step 4: Verify trigger RED, implement and refactor**

Trigger crossing rules must match the design exactly. A gap clears the prior source value. A capture stores sample indices, channel values, trigger position and `auto_timeout`.

- [ ] **Step 5: Verify Task 3 and commit**

Run buffer/trigger tests plus all live library tests, format, update progress, commit:

```bash
git commit -m "feat: add live buffer and software trigger"
```

## Task 4: Recoverable `.scope` Recording and Offline DataSource

**Files:**
- Create: `src/live/recording.rs`
- Create: `src/live/scope_source.rs`
- Modify: `src/live/mod.rs`
- Modify: `src/data/mod.rs`
- Test: inline recording/source tests

- [ ] **Step 1: Write failing recording round-trip tests**

Use a unique temp directory and write metadata plus two validated sample frames, a gap and a trigger. Assert scan results, clean index, sample count, time range and channel metadata.

- [ ] **Step 2: Verify RED and implement file header/records/writer**

Implement `RecordingMetadata`, `ScopeWriter::create`, `write_sample_frame`, `write_gap`, `write_trigger`, `finish`, and Drop-safe flush. Record CRC covers bytes after `REC1`; metadata and record payloads use explicit caps.

- [ ] **Step 3: Write failing recovery/corruption tests**

Test clean index use, truncated final record recovery, missing SessionEnd recovery, bad middle-record CRC rejection, oversized metadata rejection and invalid embedded SAMPLE_BATCH rejection.

- [ ] **Step 4: Implement scanner and index rebuild**

Expose a scanner that returns valid sample records and `RecoveredTail` only for incomplete EOF. Do not skip middle corruption.

- [ ] **Step 5: Write failing `DataSource` tests**

Open a `.scope` file through `ScopeRecordingDataSource::open`, then assert metadata, decimated `read_range`, exact first/last samples, selected-channel ordering, `summarize_range` min/max and cancellation.

- [ ] **Step 6: Implement independent offline adapter**

Build an in-memory record index at open, decode only intersecting frames during reads, use existing `RangeSummary`, and never expose the live session as a `DataSource`.

- [ ] **Step 7: Verify Task 4 and commit**

Run recording/source/data tests, formatting, update progress, commit:

```bash
git commit -m "feat: add scope recording and playback"
```

## Task 5: Transport, Acquisition Session, and TCP Simulator

**Files:**
- Create: `src/live/transport.rs`
- Create: `src/live/session.rs`
- Create: `src/live/simulator.rs`
- Create: `src/live/state.rs`
- Create: `src/bin/scope_dsp_simulator.rs`
- Modify: `src/live/mod.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Test: inline and TCP loopback tests

- [ ] **Step 1: Add dependencies without changing GUI major versions**

Add compatible `crossbeam-channel = "0.5"` and `serialport = "4"`. Keep all existing eframe/egui/wgpu declarations and `[patch.crates-io]` unchanged.

- [ ] **Step 2: Write failing transport-config and simulator-handshake tests**

Test validation for baud/address/timeouts and run a simulator on `127.0.0.1:0`. A raw client must complete HELLO, receive HELLO_ACK/CHANNEL_TABLE, CONFIGURE, START, at least two SAMPLE_BATCH frames, STOP, and successful command results.

- [ ] **Step 3: Implement serial/TCP transport and deterministic simulator**

`TransportConfig` has `Serial { port, baud }` and `Tcp { address }`. The simulator generates four documented channels and supports real-time and unthrottled test clocks plus deterministic drop/corrupt/disconnect fault settings.

- [ ] **Step 4: Write failing acquisition-session tests**

Test public `LiveSession::connect`, commands, state events, heartbeat, sequence/sample gaps, validated batch events, bounded display drops, clean disconnect and reconnect. Confirm malformed/CRC frames never reach the batch event.

- [ ] **Step 5: Implement worker and composed state**

Use bounded crossbeam channels. The worker owns transport and protocol decoder; UI-side `LiveScopeState::poll` drains events into buffer, trigger and optional recorder. Control/error state is never silently discarded; display overflow increments counters and creates a gap.

- [ ] **Step 6: Add simulator CLI and end-to-end record/replay test**

CLI flags: `--listen`, `--sample-rate`, `--batch-samples`, `--accelerated`, `--seed`, `--drop-every`, `--corrupt-every`, `--disconnect-after`. End-to-end test connects, acquires, records, stops, opens `.scope`, and compares decoded sample values/counts.

- [ ] **Step 7: Verify Task 5 and commit**

Run all live tests, simulator binary build, format/clippy for changed targets, update progress, commit:

```bash
git commit -m "feat: add live session and DSP simulator"
```

## Task 6: egui Live Workspace Integration

**Files:**
- Create: `src/app/live_ui.rs`
- Modify: `src/app.rs`
- Modify: `src/app/state.rs`
- Modify: `src/main.rs`
- Test: inline state/UI helper tests

- [ ] **Step 1: Write failing composition and default-mode tests**

Assert `ScopeApp` owns exactly one `LiveScopeState`, defaults to Offline, current startup/source-loading behavior is unchanged, and mode switching does not drop loaded offline datasets or an active live buffer.

- [ ] **Step 2: Verify RED and add minimal composition**

Add `pub live: LiveScopeState` and initialize it once in `ScopeApp::new`. Add `WorkspaceMode::{Offline, Live}` inside live state; do not add protocol/session/trigger fields to `ScopeApp`.

- [ ] **Step 3: Implement live UI adapter**

Add offline/live selector, serial/TCP settings, port refresh, connect/configure/start/stop, display pause, record controls, open-recording action, channel controls, trigger controls, statistics and central plot. Keep existing offline panels and shortcuts active only in Offline mode.

- [ ] **Step 4: Integrate `.scope` open path**

Add `.scope` to file dialogs and dispatch to `ScopeRecordingDataSource`; opening a recording switches to Offline and uses existing `set_source`/analysis caches.

- [ ] **Step 5: Verify Task 6 and commit**

Run targeted app tests, all live/data tests, formatting and clippy. Manually launch desktop plus simulator when the environment can render; otherwise record UI runtime as unverified. Update progress and commit:

```bash
git commit -m "feat: integrate live scope workspace"
```

## Task 7: Version, Documentation, Regression, and Push

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `scripts/package-windows.ps1`
- Modify: `scripts/ScopeAnalyzer.wxs`
- Modify: `README.md`
- Create: `docs/protocols/scp1-live-scope-v1.md`
- Modify: `docs/implementation/live-scope-progress.md`

- [ ] **Step 1: Write failing version-sync/script tests**

Extend or add a repository test/script assertion that reads all four version locations and README artifact names, expecting 0.8.0. Run it first and confirm it fails on 0.7.1.

- [ ] **Step 2: Synchronize version 0.8.0**

Update Cargo package/lock root package, PowerShell `$version`, WiX Product Version and README artifact names. Do not change dependency major versions or `[patch.crates-io]`.

- [ ] **Step 3: Document operator and firmware contract**

README must include simulator quick start, TCP and serial workflows, trigger modes, recording/replay, limits and troubleshooting. Protocol doc must reproduce byte layouts, CRC coverage, message payloads and a golden frame hex example suitable for DSP firmware implementers.

- [ ] **Step 4: Run fresh verification**

Run with the repository-local Rust 1.87 toolchain:

```bash
cargo +1.87.0 fmt --check
cargo +1.87.0 clippy --all-targets --quiet
cargo +1.87.0 test --quiet
cargo +1.87.0 build --release --bin scope_analyzer --bin scope_dsp_simulator
```

On Windows also run `scripts/release-check.ps1` and offline packaging. On macOS, report Windows packaging and real serial hardware as unverified, never passed.

- [ ] **Step 5: Reconcile baseline failures**

Compare full test results with the recorded 129-pass/4-fail/5-ignore baseline. Fix only failures caused by live changes. Existing platform-specific failures must remain explicitly listed unless a small, separately tested portability correction is required to make the release gate meaningful.

- [ ] **Step 6: Final progress update and commit**

Record exact commands/counts, simulator E2E status, hardware status, Windows status and limitations. Commit:

```bash
git commit -m "docs: complete live DSP scope V1"
```

- [ ] **Step 7: Push without PR or force**

Confirm branch/status/log, then normal push only:

```bash
git push github feature/live-dsp-scope-v1
```

Do not merge main, force push or create a pull request.
