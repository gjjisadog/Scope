# SCP1 DSP 实时示波器协议 V2：采样域与一致性截面

状态：V2；对应 Scope Analyzer 0.14.0。V2 是对 [SCP1 V1](scp1-live-scope-v1.md) 的新增版本，不修改任何 V1 帧布局或 V1 message payload。

## 1. 目的与兼容性

V1 的通道表和 `SAMPLE_BATCH` 没有描述采样域、固定截面或跨 CPU 因果关系。V2 使实时流成为明确的采集单位：

```text
one stream = one sample domain = one fixed sample rate = one fixed capture phase
```

- V2 帧头 `version` 固定为 `2`，其余帧头、字节序、CRC32C、长度限制和重同步规则与 V1 相同。
- `HELLO` / `HELLO_ACK`、`CHANNEL_TABLE`、`START`、`STOP`、心跳、状态和命令结果沿用 V1 的字节布局，但放在 version=2 帧中。
- 双方必须在 V1 既有 `client_capabilities` / `device_capabilities` 中声明 bit 3 (`CAPABILITY_V2_STREAMS`) 后，才发送本规范的新消息。
- 不声明 bit 3 或仅接受 version=1 的设备继续使用完整 V1 流程；V1 不会接收或解析 V2 的新增 message type。
- V2 的 `CHANNEL_TABLE` 仍只说明数值类型、缩放和名称；通道的 stream、CPU/CLA 所属和因果语义由 `STREAM_TABLE` 单独声明。

## 2. 固定采样域与截面

| `SampleDomain` | 值 | 固定采样率 | 唯一允许的 `CapturePhase` | 截面语义 |
| --- | ---: | ---: | --- | --- |
| `Fast32k` | 0 | 32,000 Hz | `AfterClaComplete` (0) | CPU1/CLA 快速链路中 CLA 完成后 |
| `Control8k` | 1 | 8,000 Hz | `ControlCycleEnd` (1) | CPU1 完整控制周期结束后 |
| `Slow1k` | 2 | 1,000 Hz | `LogicTaskEnd` (2) | CPU2 LogicTask 结束后 |

收端必须拒绝域、频率和阶段三者不匹配的 descriptor。`consistency_group` 必须非零；同一逻辑快照或因果链中的 stream 使用同一个 group。

## 3. 新消息

| 值 | 名称 | 方向 | 用途 |
| ---: | --- | --- | --- |
| `0x30` | `STREAM_TABLE` | DSP → Client | V2 stream、通道归属、CPU/CLA 归属与因果关系 |
| `0x31` | `CONFIGURE_STREAM` | Client → DSP | 选择一个 stream 及其子集通道 |
| `0x32` | `STREAM_SAMPLE_BATCH` | DSP → Client | 一个 stream 的样点批次 |

### `STREAM_TABLE` (`0x30`)

头部：`u32 revision, u16 stream_count, u16 binding_count, u16 causal_relation_count, u16 reserved_zero`。

每个 `StreamDescriptor`：

```text
u16 stream_id, u8 sample_domain, u8 capture_phase,
u32 sample_rate_hz, u16 consistency_group, u16 reserved_zero
```

每个 `StreamChannelBinding`：

```text
u16 channel_id, u16 stream_id, u8 producer, u8 reserved_zero
```

`producer`：0=CPU1、1=CLA、2=CPU2。每个实时 `channel_id` 必须恰好绑定到一个 stream；每个 stream 至少有一个通道。这样客户端能同时判断信号所属 CPU/CLA 和它所在的固定截面。

每个 `CausalRelation`：

```text
u16 input_stream_id, u16 result_stream_id, u16 application_stream_id,
i16 result_input_offset, i16 application_result_offset
```

三个 stream 必须存在且属于同一个 `consistency_group`。两个 offset 的单位是该组共享的逻辑控制周期号，不是各 stream 的本地 sample index。`result_input_offset=0, application_result_offset=1` 表示：CPU1 输入第 N 拍，结果基于第 N 拍，CPU1 在第 N+1 拍应用结果。跨 32 kHz、8 kHz 与 1 kHz 的精确时间对齐仍以帧头 `timestamp_ticks` 为准。

### `CONFIGURE_STREAM` (`0x31`)

```text
u16 stream_id, u16 batch_samples, u64 channel_mask
```

采样率不在此消息中：它只能使用 `STREAM_TABLE` 中该 stream 固定的 `sample_rate_hz`。`channel_mask` 至少选一个通道，所有选中的通道都必须绑定到 `stream_id`；包含另一个 stream 的任意 channel 必须以 `InvalidArgument` 拒绝，不能静默拆分或重采样。

### `STREAM_SAMPLE_BATCH` (`0x32`)

```text
u16 stream_id, u32 channel_table_revision, u32 sample_period_ticks,
u16 sample_count, u16 selected_channel_count,
u16 channel_ids[selected_channel_count], u8 sample_data[],
StreamRowMetadata row_metadata[sample_count]
```

布局仍是“样点优先、通道次序固定”。`channel_ids` 必须都绑定到 `stream_id`，所以一个批次绝不混入 FAST32K、CTRL8K 与 SLOW1K。`sample_period_ticks` 必须对应本 stream 的固定频率；实现可以用设备 `tick_hz` 验证。

`StreamRowMetadata` 在普通信号数据之后逐行编码；它是下列**逻辑元数据通道**的唯一副本，不会为每个普通信号通道重复一遍：

```text
u64 row_seq, u64 source_seq, u64 applied_seq, u32 valid_flags,
u64 cla_completed_seq
```

| 逻辑元数据通道 | 字段 | 语义 |
| --- | --- | --- |
| `META_ROW_SEQ` | `row_seq` | 当前采样域中严格递增的行序号 |
| `META_SOURCE_SEQ` | `source_seq` | 本行计算所用的冻结源快照序号 |
| `META_APPLIED_SEQ` | `applied_seq` | CPU1 在本行截面已经应用的最新命令或结果序号 |
| `META_VALID_FLAGS` | `valid_flags` | 截面完整性、ADC、源和应用序号的有效性 |
| `META_CLA_COMPLETED_SEQ` | `cla_completed_seq` | 本截面已经完成的最新 CLA 序号 |

`row_metadata.len()` 必须恰好等于 `sample_count`，并且批次内 `row_seq` 必须严格递增。由于每一行自行带序号，设备可在控制周期边界、跳拍或不同速率域的映射边界拆批，不需要虚构线性的因果索引。

`valid_flags` 位定义如下：

| 位 | 名称 | 含义 |
| ---: | --- | --- |
| 0 | `CLA_COMPLETE` | `cla_completed_seq` 已在该截面完成 |
| 1 | `ADC_VALID` | ADC 输入有效 |
| 2 | `DATA_FROZEN` | 该行由 DSP 冻结后才发布 |
| 3 | `SOURCE_VALID` | `source_seq` 可用于因果对齐 |
| 4 | `APPLIED_VALID` | `applied_seq` 可用于因果对齐 |

客户端必须把这些关系显示为**因果关系**，而不是物理同时性：若相应 valid 位未置，显示“无效”；若 `related_seq == row_seq`，显示“同拍”；若 `related_seq + 1 == row_seq`，显示“上一拍”；未来序号或其他差值显示“序号不匹配”。`source_seq` 用于源快照对齐，`applied_seq` 用于应用结果对齐。所有这些判断只说明 DSP 已冻结的逻辑截面；跨 CPU 的物理时间关系仍以时间戳和 DSP 设计保证为准。

### DSP 侧高速录波消息 (`0x40`–`0x46`)

| 值 | 名称 | 方向 | 用途 |
| ---: | --- | --- | --- |
| `0x40` | `ARM_CAPTURE` | Client → DSP | 在一个 V2 stream 上建立本地环形录波与触发条件 |
| `0x41` | `MANUAL_TRIGGER` | Client → DSP | 触发已 arm 的指定 capture |
| `0x42` | `CANCEL_CAPTURE` | Client → DSP | 取消指定 capture |
| `0x43` | `CAPTURE_STATUS` | DSP → Client | 上报 ARM、触发、冻结和上传进度 |
| `0x44` | `CAPTURE_BEGIN` | DSP → Client | 声明冻结录波的 stream、行数和触发行序号 |
| `0x45` | `CAPTURE_DATA` | DSP → Client | 分块上传冻结的 V2 stream 数据 |
| `0x46` | `CAPTURE_END` | DSP → Client | 完成、取消或失败的终态 |

`ARM_CAPTURE`：

```text
u32 capture_id, u16 stream_id, u16 reserved_zero,
u32 pretrigger_rows, u32 posttrigger_rows,
u8 trigger_kind, u8 reserved_zero, u16 channel_id,
f32 level, f32 hysteresis, u32 flag_mask, u32 flag_value
```

`capture_id` 必须非零；`pretrigger_rows + posttrigger_rows + 1` 不得超过 1,048,576。`trigger_kind`：0=模拟上升沿、1=模拟下降沿、2=硬件标志、3=手动。模拟触发使用 `channel_id`、`level` 和非负 `hysteresis`；硬件标志触发要求 `flag_mask` 非零。`MANUAL_TRIGGER` 和 `CANCEL_CAPTURE` 的 payload 均为 `u32 capture_id`。

`CAPTURE_STATUS` 与 `CAPTURE_END`：

```text
u32 capture_id, u8 state, u8 reserved_zero[3],
u32 captured_or_uploaded_rows, u32 dropped_rows
```

`state`：0=Armed、1=Triggered、2=Frozen、3=Uploading、4=Completed、5=Cancelled、6=Failed。`CAPTURE_BEGIN`：

```text
u32 capture_id, u16 stream_id, u16 reserved_zero,
u32 row_count, u64 trigger_row_seq
```

`CAPTURE_DATA`：

```text
u32 capture_id, u32 block_index, u32 nested_batch_len,
u8 nested_stream_sample_batch[nested_batch_len]
```

嵌套批次使用上面的 `STREAM_SAMPLE_BATCH` 布局，因此高速录波也保留逐行元数据和同一截面语义。客户端必须校验 `capture_id`、`stream_id`、块序号、行数和所有元数据后再持久化或显示。

## 4. 状态机与错误

1. V2 client 使用 version=2 的 `HELLO` 并声明 capability bit 3。
2. V2 device 使用 version=2 的 `HELLO_ACK` 宣布 bit 3，然后按顺序发送 `CHANNEL_TABLE`、`STREAM_TABLE`。
3. client 验证两张表并发送一个 `CONFIGURE_STREAM`；成功后发送 V1 布局的 `START`。
4. device 只能发送该配置 stream 的 `STREAM_SAMPLE_BATCH`，可穿插 `STATUS` 和心跳。

高速模式不依赖 UART 连续实时传完每一行：client 发送 `ARM_CAPTURE`，DSP 在本地 RAM 中持续环形采样；触发条件满足后，DSP 继续采够 post-trigger 行、冻结缓冲区，依次发送 `CAPTURE_STATUS(Frozen)`、`CAPTURE_BEGIN`、一个或多个 `CAPTURE_DATA`、`CAPTURE_END(Completed)`。传输期间可发送 `CAPTURE_STATUS(Uploading)`。`MANUAL_TRIGGER` 和 `CANCEL_CAPTURE` 只改变 DSP 侧 capture 状态；它们不允许客户端制造或补写“同一截面”。

软件触发仍适用于低速连续 V1 流及低通道数模式；32 kHz 的故障前触发、硬件故障标志和 UART 无法实时传完的录波必须使用 DSP 侧 capture。

未知枚举、保留字段非零或未知 `valid_flags` 位、表 revision 不匹配、重复绑定、未绑定通道、跨 stream 选通道、跨 stream batch、因果关系跨 group、行元数据数量不符、`row_seq` 不递增、未置 `DATA_FROZEN` 的行、FAST32K 未置 `CLA_COMPLETE` 的行、数据长度不符和索引溢出均为协议错误，不能进入实时缓冲或录波。

## 5. 参考实现

- V1 codec 与 frame 支持：`src/live/protocol.rs`
- V2 stream codec 和验证：`src/live/protocol_v2.rs`
- V1 兼容规范：`docs/protocols/scp1-live-scope-v1.md`

运行：

```bash
cargo test --lib live::protocol
cargo test --lib live::protocol_v2
```
