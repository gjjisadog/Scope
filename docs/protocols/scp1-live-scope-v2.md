# SCP1 Live Scope V2 R1/R2 frozen protocol

Status: frozen by Scope Analyzer 0.15.3 and validated with the deterministic TCP simulator. The real Hybrid30K DSP firmware has not implemented or validated this protocol yet. SCP1 V1 framing, message numbers, golden frames, the `.scope V1` format, and the default desktop path are unchanged.

All integers and `f32` values are little-endian. Implementations encode fields explicitly and use checked arithmetic for every size, offset, and affine reconstruction.

## Revision selection

R1 and R2 are selected only by capability bits and distinct message identifiers. Payload length is never used to guess a revision.

| Capability | Frozen value |
| --- | ---: |
| `CAPABILITY_V2_STREAMS_R1` | `1 << 3` (`0x00000008`) |
| `CAPABILITY_V2_STREAMS_R2` | `1 << 4` (`0x00000010`) |
| `CAPABILITY_V2_MULTI_STREAM` | `1 << 5` (`0x00000020`) |
| `CAPABILITY_V2_COMPRESSED_METADATA` | `1 << 6` (`0x00000040`) |
| `CAPABILITY_V2_HARDWARE_CAPTURE_R2` | `1 << 7` (`0x00000080`) |

| Message | R1 | R2 |
| --- | ---: | ---: |
| Stream table | `0x30 STREAM_TABLE_R1` | `0x35 STREAM_TABLE_R2` |
| Configure | `0x31 CONFIGURE_STREAM_R1` | `0x34 CONFIGURE_STREAMS_R2` |
| Sample batch | `0x32 SAMPLE_BATCH_V2_R1` | `0x33 SAMPLE_BATCH_V2_R2` |
| Capture data | `0x45 CAPTURE_DATA_R1` | `0x47 CAPTURE_DATA_R2` |

Common Capture control remains `0x40..0x44` and `0x46`. R1 peers reject R2 messages and R2 peers reject R1 stream messages. `LiveSession::connect_v2_r1` is the explicit compatibility entry point; `connect_v2_r2` is the frozen R2 entry point and `connect_v2` is its alias.

R1 retains its 28-byte per-row metadata exactly:

```text
u64 row_sequence
u64 source_sequence
u64 applied_sequence
u32 valid_flags
```

The 0.15.2 prototype placed a 36-byte row containing `logical_cycle_sequence` behind message `0x32`. It was never frozen. A receiver detecting that layout returns exactly `unsupported pre-release SCP1 V2 layout`; it does not reinterpret the payload as R1 or R2.

## R2 stream table and logical time

Each consistency group carries:

```text
u16 consistency_group
u16 max_reorder_cycles
u32 logical_cycle_rate_hz
```

Each R2 stream carries its fixed domain, phase, sample rate, consistency group, `logical_cycle_step`, and channel ids. The following identity must hold using exact integer division:

```text
logical_cycle_step = logical_cycle_rate_hz / sample_rate_hz
```

The 30K contract uses group 1 at 32,000 logical cycles/s:

| Stream | Rate | Phase | Step | Row-to-cycle mapping |
| --- | ---: | --- | ---: | --- |
| FAST32K (1) | 32,000 Hz | `AfterClaComplete` | 1 | row N → cycle N |
| CTRL8K (2) | 8,000 Hz | `ControlCycleEnd` | 4 | row K → cycle 4K |
| SLOW1K (3) | 1,000 Hz | `LogicTaskEnd` | 32 | row M → cycle 32M |

Adjacent rows advance by the step. A row gap of D advances logical time by `D * logical_cycle_step`. `row_sequence` remains stream-local and is never used as a cross-domain causal key. Different consistency groups have independent logical clocks.

## Atomic multi-stream configuration

`CONFIGURE_STREAMS_R2` contains a nonzero transaction id and 1..=8 subscriptions:

```text
u32 transaction_id
u16 subscription_count
u16 reserved = 0
repeat subscription_count {
    u16 stream_id
    u16 batch_samples
    u64 channel_mask
}
```

Stream ids must be unique, masks may select only channels bound to that stream, and each batch must fit both device and negotiated payload limits. Validation completes before state mutation. Any invalid member rejects the whole transaction; the previous subscription set remains active. `CommandResult.detail` returns the accepted transaction and final subscription set. START starts the entire set, STOP stops the entire set but preserves the session, and a later transaction atomically replaces the set.

## R2 compressed sample metadata

`SAMPLE_BATCH_V2_R2` uses message `0x33`. Its 36-byte fixed sample header is:

```text
u16 stream_id
u32 stream_revision
u8  domain
u8  capture_phase
u16 consistency_group
u64 first_row_sequence
u16 row_count
u32 sample_period_ticks
u16 channel_count
u8  metadata_encoding
u8  reserved = 0
u32 sample_data_len
u32 metadata_len
```

It is followed by `channel_count * u16` channel ids, sample bytes, then exactly `metadata_len` bytes. Row and logical steps live in the affine metadata base below rather than being duplicated in the fixed header. Continuous traffic uses `AffineWithOverrides`.

The 48-byte affine base is:

```text
u64 first_row_sequence
u32 row_sequence_step
u64 first_logical_cycle_sequence
u32 logical_cycle_step
i64 source_delta_from_logical
i64 applied_delta_from_logical
u32 common_valid_flags
u16 override_count
u16 reserved = 0
```

Rows reconstruct as:

```text
row = first_row + row_offset * row_step
logical = first_logical + row_offset * logical_step
source = logical + source_delta
applied = logical + applied_delta
```

Each override begins with `u16 row_offset, u16 override_mask`, followed in bit order by optional row, logical, source, applied (`u64`) and flags (`u32`). Offsets are strictly increasing, unique, and in range; unknown bits and arithmetic overflow are rejected. `Explicit` encodes the five fields as 36 bytes per row and is supported for exceptional Capture data, but its full cost is included in link-budget decisions.

For 16 I16 channels, 8 kHz, 128 rows/frame, UART 4,000,000 baud, 8N1, the automated calculation for affine metadata (including frame overhead) is at most 70%; sparse overrides remain at most 70%. Explicit metadata exceeds 70% and is reported unsafe. Eight I16 channels at 32 kHz also exceeds 70%, so FAST32K continuous UART streaming is not claimed; it is reserved for DSP-local Capture followed by upload.

## Causal relations and watermarks

The simulator freezes the real three-stream relation FAST32K Input → CTRL8K Result → SLOW1K Application. The normal relation uses `result_input_offset=0` and `application_result_offset=32`; dedicated presets validate positive and negative result offsets.

Matching uses logical-cycle units and checked signed arithmetic:

```text
expected_input_cycle = result.source_sequence - result_input_offset
expected_result_cycle = application.applied_sequence - application_result_offset

result.source_sequence = input.logical_cycle_sequence + result_input_offset
application.applied_sequence = result.logical_cycle_sequence + application_result_offset
```

Results and applications may arrive first and enter ordered pending maps. For a relation source, absence becomes final only when:

```text
source_watermark > expected_source_cycle + max_reorder_cycles
```

Normal lookup and insertion are O(log n). Cached rows and pending relations each have a hard 4,096-entry limit. A hard-limit breach returns `CausalWindowOverflow`; it never overwrites silently. Completed pending entries are removed immediately. Diagnostics expose cached rows, pending matches, match timeouts, evictions, overflows, and duplicate logical cycles. A table revision, session id change, or DeviceReset clears every causal window.

## DeviceReset

`CaptureStatus::DeviceReset` clears Capture blocks, validator/watermarks, timing, heartbeat nonces, pending commands, subscriptions, ChannelTable, StreamTable, configuration, streaming state, and the old statistics context. State becomes `DeviceResetHandshake`; neither streaming nor Capture nor subscriptions are restored automatically.

The device then sends a new nonzero and different session id with `HELLO_ACK → CHANNEL_TABLE → STREAM_TABLE_R2`. Only after all three validate does the client enter Ready. Frames carrying an old session id are rejected. UI and CLI surface “设备已复位，等待重新握手”.

## Hardware Capture R2

R2 Capture data uses `0x47` and may use affine or explicit metadata. The assembler computes payload length without clone/encode, passes the already validated wire payload as `Arc<[u8]>`, checks only index-adjacent BTreeMap predecessor/successor blocks on insertion, and advances CRC32C whenever a contiguous prefix becomes available. Once a block enters that contiguous CRC prefix its wire-payload Arc is released; the decoded batch remains available for ordered delivery.

The frozen integrity value is incremental and equivalent to:

```text
CRC32C(little_endian(capture_id) +
       CAPTURE_DATA_R2 payloads in block_index order)
```

No combined 64 MiB buffer is created. Push is O(log n), CRC work is linear in the current block, and finish moves ordered batches. Completion, failure, timeout, cancellation, invalid configuration, buffer overrun, and reset release all blocks and accounting. A Capture failure emits `CaptureFailure` while keeping the V2 connection usable for heartbeat and a later ARM.

## Heartbeat and diagnostics

Both peers retain up to eight `(nonce, sent_at)` entries. PONG may arrive out of order. RTT is measured per match; timeouts occur only at three seconds. Unknown PONG, duplicate PONG, timeout, and window overflow are independent counters; overflow is not a timeout and timeout is not a protocol error. DeviceReset clears the window. Statistics include pending count, round-trip count, timeout, unknown, duplicate, overflow, last RTT, and max RTT.

CLI examples:

```text
scope_dsp_simulator --protocol v2-r2 --streams fast32k,ctrl8k,slow1k --preset 30k-causal-in-order --accelerated
scope-cli live-inspect --protocol v2-r2 --address 127.0.0.1:19090 --rows 32
scope-cli live-inspect --protocol v2-r1 --address 127.0.0.1:19090 --stream-id 1
scope-cli capture-inspect --protocol v2-r2 --address 127.0.0.1:19090 --stream-id 1
```

The JSON contains the protocol revision/session, active streams, metadata mode, estimated 4 Mbaud utilization, causal gauges/timeouts/overflows, heartbeat RTT gauges, capture completion, and reset count. A diagnostic contract failure returns `ok=false`, a stable error category, and a nonzero exit code.

GitHub CI runs format, check, Clippy, all targets, focused live tests, simulator tests, and an isolated ignored one-million-row bounded-causal job on hosted runners. These are simulator/software results, not Hybrid30K hardware acceptance. Physical DSP UART and Capture validation remains a separate self-hosted hardware workflow.
