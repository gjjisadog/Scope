# SCP1 DSP 实时示波器协议 V1

状态：V1 冻结；对应 Scope Analyzer 0.8.0。

SCP1 是 DSP 与 Scope Analyzer 之间的双向二进制字节流协议。TCP 和串口使用完全相同的帧格式；多字节整数与 IEEE-754 `f32` 均为 little-endian。字符串为 UTF-8，不带结尾零字符。

## 1. 传输约束

- TCP：面向字节流，不依赖一次 `send` 对应一次帧。
- 串口：8 数据位、无校验、1 停止位、无流控；波特率由用户配置。
- 接收端必须支持拆包、粘包和 magic 重同步。
- 单帧 payload 最大 1 MiB；通道数 1..64；单批样点数 1..4096。
- 空闲连接每秒发送一次 PING；客户端连续 3 秒收不到有效帧即判定失联。

## 2. 帧格式

每帧为 `28-byte header + payload + 4-byte CRC32C`：

| 偏移 | 长度 | 字段 | 说明 |
| ---: | ---: | --- | --- |
| 0 | 4 | magic | ASCII `SCP1`，不参与 CRC |
| 4 | 1 | version | 固定为 `1` |
| 5 | 1 | message_type | 见消息表 |
| 6 | 2 | flags | V1 发送端写 0；接收端保留 |
| 8 | 4 | sequence | 发送方向独立递增的 `u32`，允许回绕 |
| 12 | 4 | payload_len | payload 字节数，最大 1,048,576 |
| 16 | 4 | session_id | HELLO 为 0；设备分配的非零会话号用于其余消息 |
| 20 | 8 | timestamp_ticks | 设备 tick；不适用时为 0 |
| 28 | N | payload | 消息负载 |
| 28+N | 4 | crc32c | Castagnoli CRC32C，覆盖 `[version..payload末尾]` |

CRC 参数与标准检查值：多项式 Castagnoli，`CRC32C("123456789") = 0xE3069283`。长度、版本或 CRC 不合法的帧必须丢弃；接收器继续逐字节寻找下一处 `SCP1`。

固件 golden frame：`PING(nonce=17)`，flags=3、sequence=7、session_id=11、timestamp_ticks=13 的完整 40 字节帧为：

```text
53 43 50 31 01 14 03 00 07 00 00 00 08 00 00 00
0b 00 00 00 0d 00 00 00 00 00 00 00 11 00 00 00
00 00 00 00 1d 23 97 cb
```

末尾 CRC32C 的 little-endian 数值为 `0xCB97231D`。固件实现应首先用该向量验证字节序、CRC 覆盖范围和最终异或。

## 3. 消息类型与方向

| 值 | 名称 | 方向 | 用途 |
| ---: | --- | --- | --- |
| `0x01` | HELLO | Client → DSP | 建立会话 |
| `0x02` | HELLO_ACK | DSP → Client | 能力与设备身份 |
| `0x03` | CHANNEL_TABLE | DSP → Client | 通道定义 |
| `0x10` | CONFIGURE | Client → DSP | 设置采集参数 |
| `0x11` | START | Client → DSP | 开始采集 |
| `0x12` | STOP | Client → DSP | 停止采集 |
| `0x13` | COMMAND_RESULT | DSP → Client | 命令结果 |
| `0x14` | PING | 双向 | 心跳 nonce |
| `0x15` | PONG | 双向 | 原样返回 nonce |
| `0x20` | SAMPLE_BATCH | DSP → Client | 多通道交织采样 |
| `0x21` | STATUS | DSP → Client | 设备状态与丢样统计 |
| `0x22` | ERROR | DSP → Client | 异步错误 |

任何 payload 解码后有剩余字节均视为协议错误。V1 客户端 capability：bit 0=录波、bit 1=软件触发、bit 2=gap 记录；其他位必须发送 0。接收端必须忽略未知 capability 位。

## 4. 会话状态机

1. Client 连接 TCP/打开串口，发送 session_id=0 的 HELLO。
2. DSP 分配非零 session_id，依次返回 HELLO_ACK 和 CHANNEL_TABLE。
3. Client 发送 CONFIGURE，DSP 返回引用请求 sequence 的 COMMAND_RESULT。
4. CONFIGURE 成功后 Client 发送 START；成功后 DSP 连续发送 SAMPLE_BATCH，可穿插 STATUS/PONG。
5. STOP 成功后停止发送新 SAMPLE_BATCH，保留连接并回到 Configured。
6. 连接断开或心跳超时会终止会话；V1 客户端不自动重连。

除 HELLO 外，会话内收到不匹配的 session_id 必须拒绝。START 仅在 Configured 状态成功；STOP 仅在 Streaming 状态成功。

## 5. Payload 定义

以下字段按表格顺序紧密排列，不做隐式对齐。`str8` 为 `u8 byte_len + UTF-8 bytes`，`str16` 为 `u16 byte_len + UTF-8 bytes`。

### HELLO (`0x01`)

`u32 client_capabilities, u32 max_payload, str16 client_name`

### HELLO_ACK (`0x02`)

`u32 device_capabilities, u32 max_payload, u64 tick_hz, u16 channel_count, u16 max_batch_samples, u8 device_id[16], str16 firmware_name`

`tick_hz`、通道数和最大批量必须非零且在全局限制内。`device_id` 是原始 16 字节稳定标识。

### CHANNEL_TABLE (`0x03`)

头部：`u32 revision, u16 descriptor_count`。

每个 descriptor 的固定部分和两个长度字段必须连续出现，随后才是两个字符串：

`u16 channel_id, u8 kind, u8 wire_format, f32 scale, f32 offset, u8 unit_len, u8 name_len, u8 unit[unit_len], u8 name[name_len]`

- `channel_id` 唯一，范围 0..63；名称非空。
- kind：0=Analog，1=Digital。
- wire_format：1=`i16`，2=`i32`，3=`f32`，4=`u8`。
- 整数工程值：`raw * scale + offset`。
- `f32` 已是工程值，必须声明 `scale=1, offset=0`。
- Digital `u8` 只允许 0 或 1。

### CONFIGURE (`0x10`)

`u32 sample_rate_hz, u16 batch_samples, u16 reserved_zero, u64 channel_mask`

mask bit N 选择 channel_id N；至少选择一个通道。采样率不得超过 `tick_hz`，批量不得超过 `max_batch_samples`，预计 SAMPLE_BATCH payload 不得超过协商的 `max_payload`。DSP 不支持参数时以 COMMAND_RESULT 拒绝，不得静默替换。

### START / STOP (`0x11` / `0x12`)

payload 长度为 0。

### COMMAND_RESULT / ERROR (`0x13` / `0x22`)

`u32 request_sequence, u16 result_code, str16 detail`

result_code：0=Ok，1=Unsupported，2=InvalidState，3=InvalidArgument，4=Busy，5=InternalError。异步 ERROR 的 `request_sequence` 可为 0。

CONFIGURE 成功时 detail 必须返回设备实际采用的参数，固定 ASCII 格式如下（字段次序固定，mask 为 16 位小写十六进制）：

```text
sample_rate_hz=20000;batch_samples=64;channel_mask=0x000000000000000f
```

### PING / PONG (`0x14` / `0x15`)

`u64 nonce`。PONG 必须原样返回收到的 nonce。

### SAMPLE_BATCH (`0x20`)

`u32 channel_table_revision, u64 first_sample_index, u32 sample_period_ticks, u16 sample_count, u16 selected_channel_count, u16 channel_ids[selected_channel_count], u8 sample_data[]`

sample_data 使用“样点优先、通道次序固定”的交织布局：

```text
sample0.ch0, sample0.ch1, ... sample0.chN,
sample1.ch0, sample1.ch1, ... sample1.chN, ...
```

每个值的宽度由对应 CHANNEL_TABLE descriptor 决定。payload 必须恰好等于头部、channel_ids 与 `sample_count * sum(channel_width)` 之和。帧头 `timestamp_ticks` 是首个样点时间，后续样点时间为：

```text
timestamp_ticks + sample_offset * sample_period_ticks
```

`first_sample_index` 是本会话单调递增的全局样点号。下一批应从 `first_sample_index + sample_count` 开始；跳号会被记录为 gap，倒退/重叠批次会被拒绝。若通道定义变化，DSP 必须先发送新 revision 的 CHANNEL_TABLE，再用该 revision 发送采样。

### STATUS (`0x21`)

`u8 state, u8 reserved_zero[3], u64 produced_samples, u64 dropped_samples, u32 tx_overruns`

state：0=Idle，1=Configured，2=Streaming。

## 6. 错误与完整性要求

- 发送端不得产生 NaN/Infinity 的 scale/offset；`f32` 样值由应用层决定是否展示。
- 所有长度、样点索引和 tick 计算必须检查整数溢出。
- CRC 错误、未知消息、未知枚举、保留位非零、截断 payload、重复通道和数据长度不符均不得进入实时缓冲或录波。
- 客户端 UI 队列背压造成的丢批次必须生成 HostBackpressure gap，不允许用连线掩盖缺口。
- DSP 自身丢样应体现在 `first_sample_index` 跳变及 STATUS 计数中。

## 7. 参考实现与一致性测试

- Rust codec：`src/live/protocol.rs`
- 采集状态机：`src/live/session.rs`
- TCP 参考模拟器：`src/live/simulator.rs`
- 模拟器 CLI：`src/bin/scope_dsp_simulator.rs`
- `.scope` 容器：`src/live/recording.rs`

执行以下命令验证 codec、拆包/粘包/坏 CRC 恢复、混合采样格式、索引溢出、session、gap、录波与回放：

```bash
cargo test --lib live::
cargo test --bin scope_dsp_simulator
```

SCP1 V1 的兼容性规则是：V1 帧布局和现有消息字段不得原地改变；新增可选行为应通过 capability 位或新 message_type 引入，破坏性修改必须升级 protocol version。
