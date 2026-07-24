# SCP1 Live Scope V2

状态：已实现客户端、协议和确定性 TCP 模拟器；真实 Hybrid30K DSP 固件尚未实现或验证。

V2 是独立协议版本（帧头 `version=2`）。SCP1 V1 的帧头、V1 消息 payload、CRC32C、`.scope V1` 和默认 GUI 行为均冻结不变。V2 peer 必须通过既有 HELLO capability bit 3 (`CAPABILITY_V2_STREAMS`) 明确协商；V1 peer 不会把 V2 帧或 V2 message type 当作 V1 消息解析。

所有整数和 `f32` 字段均为 little-endian；实现不得直接序列化 Rust struct。长度、乘法、索引和偏移均以 checked arithmetic 校验。

## 固定采样域与截面

| Domain | 固定采样率 | 唯一 CapturePhase | 冻结截面 |
| --- | ---: | --- | --- |
| `Fast32k` (0) | 32,000 Hz | `AfterClaComplete` (0) | CPU1/CLA 完成快速链路后的同一行 |
| `Control8k` (1) | 8,000 Hz | `ControlCycleEnd` (1) | CPU1 控制周期结束后的同一行 |
| `Slow1k` (2) | 1,000 Hz | `LogicTaskEnd` (2) | CPU2 逻辑任务结束后的同一行 |

一个 `StreamDescriptor` 包含 `stream_id`、domain、capture phase、固定频率、非零 consistency group 和非空且去重的 `channel_ids`。`stream_id` 必须非零且唯一。`STREAM_TABLE` 的每个 channel binding 还传输 owner (`CPU1`、`CPU1_CLA1`、`CPU2`) 和 role (`PhysicalSample`、`ControlInput`、`ControlOutput`、`Command`、`AppliedCommand`、`State`、`Fault`、`Diagnostic`、`Metadata`)；结合 V1 `CHANNEL_TABLE` 基础 descriptor 即构成 `ChannelDescriptorV2`。

客户端不从不同域的零散变量拼接一行，也不会把跨 CPU 的因果序号称为物理同时。

## V2 message allocation

| 值 | 名称 | 方向 |
| ---: | --- | --- |
| `0x30` | `STREAM_TABLE` | DSP → Client |
| `0x31` | `CONFIGURE_STREAM` | Client → DSP |
| `0x32` | `SAMPLE_BATCH_V2` | DSP → Client |
| `0x40` | `ARM_CAPTURE` | Client → DSP |
| `0x41` | `MANUAL_TRIGGER` | Client → DSP |
| `0x42` | `CANCEL_CAPTURE` | Client → DSP |
| `0x43` | `CAPTURE_STATUS` | DSP → Client |
| `0x44` | `CAPTURE_BEGIN` | DSP → Client |
| `0x45` | `CAPTURE_DATA` | DSP → Client |
| `0x46` | `CAPTURE_END` | DSP → Client |

`CONFIGURE_STREAM` chooses one stream and a nonempty subset of its channels; it carries no rate because the rate is fixed by the descriptor. A mixed-domain mask is rejected.

## SAMPLE_BATCH_V2

The payload is:

```text
u16 stream_id
u32 stream_revision
u8 domain
u8 capture_phase
u16 consistency_group
u64 first_row_sequence
u16 row_count
u32 sample_period_ticks
u16 selected_channel_count
u16 channel_ids[selected_channel_count]
u8 interleaved_sample_data[]
SnapshotMeta[row_count]
```

`SnapshotMeta` is encoded once for every row:

```text
u64 row_sequence
u64 logical_cycle_sequence
u64 source_sequence
u64 applied_sequence
u32 valid_flags
```

Known validity bits are `SNAPSHOT_VALID` (0), `SOURCE_SEQUENCE_VALID` (1), `APPLIED_SEQUENCE_VALID` (2), `CLA_RESULT_VALID` (3), `ADC_SAMPLE_VALID` (4), and `FROZEN_ROW` (5). Unknown bits are rejected. `row_sequence` is only the local stream row number and must be continuous inside a batch；`logical_cycle_sequence` is the independent cross-stream causal key. Across batches an explicit row gap is legal and is recorded；overlap or reversal is recorded as a reorder. Frame domain, phase, group, revision, selected channels, metadata count and exact data length must match `STREAM_TABLE`/`CHANNEL_TABLE`.

`CausalRelation` is evaluated in its declared consistency group using the protocol's logical control-cycle sequence, not a physical-time comparison of local row counters. For every relation, the client checks:

```text
result.source_sequence == input.logical_sequence + result_input_offset
application.applied_sequence == result.logical_sequence + application_result_offset
```

The client records `missing_causal_source`, `causal_source_mismatch`, `causal_application_mismatch`, `causal_sequence_reorder`, `causal_group_mismatch`, and `causal_cache_evictions` separately. Result/application rows may arrive before their causal source；matching is delayed instead of immediately reporting a missing source. Both cached rows and pending matches are bounded to 4,096 entries, with deterministic eviction and an observable missing-source/eviction diagnostic. A legal `(0, 1)` relation therefore means “result uses input N, application occurs at N+1”; it does not imply that CPU1, CPU2 and CLA were physically simultaneous. FAST32K requires `SNAPSHOT_VALID|FROZEN_ROW|SOURCE_SEQUENCE_VALID|ADC_SAMPLE_VALID|CLA_RESULT_VALID`; CTRL8K additionally requires source validity, and `APPLIED_SEQUENCE_VALID` is required when an `AppliedCommand` role or causal application needs it. SLOW1K always requires a frozen valid row and requires source/applied validity only when its role or a causal contract uses that sequence.

For all domains `sample_period_ticks == tick_hz / sample_rate_hz` is exact. `tick_hz` must be divisible by the fixed stream rate; V2 does not round. Across batches of the same stream, `current_timestamp == previous_last_timestamp + (current_first_row_sequence - previous_last_row_sequence) * sample_period_ticks` must hold exactly. Zero, a non-integral timer, a mismatched period, an in-session period change, row reversal/overlap, timestamp drift, or overflow is rejected.

The `MANUAL_TRIGGER` golden frame is regression-tested as fixed bytes (including CRC):

```text
53 43 50 31 02 41 00 00 44 33 22 11 04 00 00 00
88 77 66 55 08 07 06 05 04 03 02 01 d4 c3 b2 a1
30 c8 de cb
```

## DSP hardware Capture

`ARM_CAPTURE` carries a nonzero `capture_id`, `stream_id`, trigger type (`Manual`, `Edge`, `FaultFlag`), trigger channel/level/edge, pre/post rows and `timeout_samples`. The state model is:

```text
Idle → Armed → Triggered → PostCapture → Complete → Uploading → Idle
```

Terminal/exception states are `Cancelled`, `Timeout`, `BufferOverrun`, `InvalidConfig`, and `DeviceReset`.

`CAPTURE_BEGIN` binds upload to a capture id, stream and expected row total. `CAPTURE_DATA` contains indexed nested `SAMPLE_BATCH_V2` chunks. Every arriving block is checked before insertion: capture id, stream id/revision, domain, phase, consistency group, ordered channel ids, block index, per-block row count, cumulative rows and cumulative encoded payload bytes. Rows must be continuous inside and, after sorting by block index, between chunks; the trigger row must be within the captured range.

`CAPTURE_END` succeeds only when state is `Complete`, `uploaded_rows == total_samples == received rows == CAPTURE_BEGIN.row_count`, `dropped_rows == 0`, and its actual block count equals `total_blocks`. Its frozen `integrity_summary` algorithm is:

```text
CRC32C(little_endian(capture_id) +
       CAPTURE_DATA encoded payloads sorted by block_index)
```

The client recomputes this CRC32C and rejects a mismatch. Protocol allocation limits are `MAX_CAPTURE_ROWS=1,048,576`, `MAX_CAPTURE_BLOCKS=4,096`, `MAX_CAPTURE_BLOCK_ROWS=4,096`, and `MAX_CAPTURE_PAYLOAD_BYTES=64 MiB`; all are checked before allocation. Errors/diagnostics distinguish `CaptureTooLarge`, `CaptureTooManyBlocks`, `CaptureRowOverflow`, `CaptureIntegrityMismatch`, `CaptureRowDiscontinuity`, and `CaptureDescriptorMismatch`. Every `CAPTURE_END` and every terminal/exception `CAPTURE_STATUS` releases buffered blocks and payload accounting. A Capture-specific decode, descriptor, integrity, timeout, loss, or reset failure emits `CaptureFailure`/`CaptureStatus` but does not tear down the negotiated V2 connection, so a later Capture or heartbeat can continue. Capture remains in memory in this stage and is never written to `.scope V1`.

## V2 connection health and host backpressure

After HELLO, client and simulator send `PING` once per second and reply immediately with the same `PONG` nonce. The client retains an eight-entry outstanding nonce window, accepts valid PONGs out of order, expires each nonce after three seconds, and never lets a newer PING overwrite an older outstanding one. Any valid V2 frame refreshes the three-second liveness deadline; CRC-invalid frames do not. The session records `last_pong_nonce`, round trips and actual expirations. `STATUS` updates device state, DSP dropped rows and TX overruns; `ERROR` is delivered as `SessionEvent::Error`.

Control events (`State`, `Error`, `CommandResult`, stream table and Capture status/result) use the bounded control path and fail explicitly on sustained backpressure. Continuous snapshots may be dropped only at the host UI queue, where `host_dropped_v2_batches`, `host_dropped_v2_rows`, queue overruns and the affected stream/row range are recorded. These host drops never increment DSP row-gap diagnostics. Capture data is assembled in the worker and is never routed through the UI snapshot queue.

## First-stage clients

The desktop app still uses SCP1 V1 by default. Tests and CLI explicitly select V2:

```text
scope_dsp_simulator --protocol v2 --preset 30k-normal --accelerated
scope-cli live-inspect --address 127.0.0.1:19090 --stream-id 1 --rows 16
scope-cli capture-inspect --address 127.0.0.1:19090 --stream-id 1 --rows 16
```

The CLI JSON includes protocol version, stream/domain/phase/group, row and causal diagnostics, and capture-complete/missing/duplicate/reordered chunk fields.
