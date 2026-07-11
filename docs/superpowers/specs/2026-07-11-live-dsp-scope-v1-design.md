# DSP 实时软件示波器 V1 设计

日期：2026-07-11
目标版本：0.8.0
目标分支：`feature/live-dsp-scope-v1`

## 1. 目标与范围

DSP 实时软件示波器 V1 在保留现有离线 CSV/DAT 分析、FFT、序分量、导出、配置和打包能力的前提下，增加一个独立实时工作区，形成以下闭环：

1. 客户端通过串口连接真实 DSP，或通过 TCP 连接配套模拟器。
2. 客户端与设备完成协议握手、通道表和采集参数协商。
3. 设备连续发送带序号、时间戳和 CRC32C 的采样批次。
4. 客户端进行有界缓冲、丢包统计、实时绘制和软件触发。
5. 原始采样帧可录制为可恢复的 `.scope` 文件。
6. `.scope` 通过独立离线 `DataSource` 适配器进入现有分析、FFT 和导出链路。
7. TCP 模拟器与自动化端到端测试验证协议、触发、录波和回放闭环。

V1 不声称已经完成 DSP 实物验证。真实硬件只有在目标板固件实现本文协议并完成板级测试后才可标记为已验证。

## 2. 方案比较与选择

### 方案 A：固定二进制帧、CRC32C、魔数重同步（采用）

- 同一帧格式运行在串口和 TCP 字节流上。
- 固定 28 字节头，变长负载，尾部 4 字节 CRC32C。
- 采样按通道描述符编码为 `i16`、`i32`、`f32` 或 `u8`，避免全部使用文本或 `f32` 的带宽浪费。
- 解析器可处理拆包、粘包、噪声和 CRC 错误，并扫描下一帧魔数恢复同步。
- DSP 端只需小端整数读写、CRC32C 和状态机，适合裸机/RTOS 固件。

### 方案 B：COBS 分帧与 CBOR 负载

串口边界恢复更直接，扩展性较好，但 DSP 端需要 COBS 和 CBOR 实现，采样热路径的编码成本和固件依赖更高。V1 不采用。

### 方案 C：换行 JSON

调试最直观，但高频采样的带宽、解析时间和内存开销不可接受，浮点文本也会引入格式差异。V1 不采用。

## 3. 传输层

### 3.1 串口

- 默认：921600 baud、8 data bits、1 stop bit、no parity、no flow control。
- UI 允许选择 115200、230400、460800、921600、1500000、2000000、3000000，并允许手工输入系统支持的正整数波特率。
- 读超时 100 ms；写超时 500 ms。
- V1 不使用串口硬件流控，依靠设备能力协商、批量采样和客户端丢帧统计控制负载。

### 3.2 TCP 模拟器

- 默认监听 `127.0.0.1:19090`。
- TCP 仅作为开发、演示和自动化测试传输；协议字节与串口完全相同。
- 测试使用 `127.0.0.1:0` 获取临时端口，避免固定端口冲突。

### 3.3 连接生命期

- 客户端连接后立即发送 `HELLO`。
- 设备回复 `HELLO_ACK` 和 `CHANNEL_TABLE` 后，连接进入 Ready。
- 客户端发送 `CONFIGURE`，收到成功的 `COMMAND_RESULT` 后可发送 `START`。
- 运行期间双方每秒发送 `PING/PONG`；连续 3 秒无有效帧视为断线。
- 实时采样不重传。序号跳变生成明确 gap 事件，避免旧数据重传阻塞实时性。
- 控制命令必须由 `COMMAND_RESULT` 引用请求序号并返回结果码。

## 4. V1 线协议

### 4.1 字节序与限制

- 所有多字节整数和 IEEE-754 浮点均为 little-endian。
- 协议版本固定为 1。
- 单帧 `payload_len` 上限为 1 MiB；超过上限的候选头视为噪声并继续重同步。
- UTF-8 字符串均使用显式长度，不包含结尾 NUL。

### 4.2 帧布局

每帧由 28 字节固定头、负载和 4 字节 CRC32C 组成：

| 偏移 | 长度 | 字段 | 说明 |
|---:|---:|---|---|
| 0 | 4 | magic | ASCII `SCP1`，字节 `53 43 50 31` |
| 4 | 1 | version | `1` |
| 5 | 1 | message_type | 消息类型 |
| 6 | 2 | flags | 位标志 |
| 8 | 4 | sequence | 发送方向各自单调递增，回绕允许 |
| 12 | 4 | payload_len | 负载字节数，最大 1 MiB |
| 16 | 4 | session_id | 握手后由设备分配；握手前为 0 |
| 20 | 8 | timestamp_ticks | 设备时钟 tick；不适用时为 0 |
| 28 | N | payload | 消息负载 |
| 28+N | 4 | crc32c | 覆盖头部偏移 4..28 与全部负载，不覆盖 magic |

CRC 使用 Castagnoli 多项式 CRC-32C。解析失败时不得直接清空输入：丢弃当前候选 magic 的第一个字节，继续扫描下一处 `SCP1`，从而处理串口噪声、丢字节、粘包和半帧。

### 4.3 消息类型

| 值 | 名称 | 方向 | 用途 |
|---:|---|---|---|
| `0x01` | HELLO | C→D | 客户端能力与最大帧 |
| `0x02` | HELLO_ACK | D→C | 设备身份、能力、会话和时钟 |
| `0x03` | CHANNEL_TABLE | D→C | 通道描述符 |
| `0x10` | CONFIGURE | C→D | 采样率、批量大小和通道掩码 |
| `0x11` | START | C→D | 开始连续采集 |
| `0x12` | STOP | C→D | 停止连续采集 |
| `0x13` | COMMAND_RESULT | 双向 | 控制命令确认或错误 |
| `0x14` | PING | 双向 | 心跳请求 |
| `0x15` | PONG | 双向 | 心跳响应 |
| `0x20` | SAMPLE_BATCH | D→C | 批量采样 |
| `0x21` | STATUS | D→C | 设备统计和状态 |
| `0x22` | ERROR | 双向 | 异步错误 |

未知消息类型在帧 CRC 有效时应被安全跳过并计数，便于向后扩展。

### 4.4 HELLO

负载：

- `client_capabilities: u32`
- `max_payload: u32`
- `client_name_len: u16`
- `client_name: [u8; client_name_len]`

V1 客户端能力位：bit 0 支持录波，bit 1 支持软件触发，bit 2 支持 gap 记录，其他位为 0。

### 4.5 HELLO_ACK

负载：

- `device_capabilities: u32`
- `max_payload: u32`
- `tick_hz: u64`
- `channel_count: u16`，范围 1..64
- `max_batch_samples: u16`，范围 1..4096
- `device_id: [u8; 16]`
- `firmware_name_len: u16`
- `firmware_name: [u8; firmware_name_len]`

设备在帧头 `session_id` 中分配非零会话号。客户端拒绝 `tick_hz == 0`、通道数越界或批量大小为 0 的应答。

### 4.6 CHANNEL_TABLE

负载以 `revision: u32` 和 `descriptor_count: u16` 开始，随后为描述符序列。每个描述符：

- `channel_id: u16`，V1 范围 0..63，表内唯一。
- `kind: u8`：0 analog，1 digital。
- `wire_format: u8`：1 `i16`，2 `i32`，3 `f32`，4 `u8`。
- `scale: f32`。
- `offset: f32`。
- `unit_len: u8`。
- `name_len: u8`。
- `unit: [u8; unit_len]`。
- `name: [u8; name_len]`。

整数工程值为 `raw * scale + offset`；`f32` 直接表示工程值，`scale` 和 `offset` 必须为 1 与 0。数字量通常使用 `u8`，有效值为 0 或 1。名称不得为空，UTF-8 必须有效。

### 4.7 CONFIGURE

负载：

- `sample_rate_hz: u32`
- `batch_samples: u16`
- `reserved: u16`，必须为 0
- `channel_mask: u64`

至少选择一个设备通道，批量大小不得超过 `HELLO_ACK.max_batch_samples`。设备可调整参数，但必须在 `COMMAND_RESULT` 的 detail 中返回实际采样率、批量大小和掩码。

### 4.8 START 与 STOP

V1 负载为空。`START` 只在成功配置后有效；重复 `START` 或 `STOP` 返回 `InvalidState`，但连接保持可用。

### 4.9 COMMAND_RESULT

负载：

- `request_sequence: u32`
- `result_code: u16`
- `detail_len: u16`
- `detail: [u8; detail_len]`

结果码：0 Ok，1 Unsupported，2 InvalidState，3 InvalidArgument，4 Busy，5 InternalError。

### 4.10 PING 与 PONG

负载均为 `nonce: u64`。`PONG` 必须回显 nonce；时间戳用于显示链路往返时间，不参与采样时间计算。

### 4.11 SAMPLE_BATCH

帧头 `timestamp_ticks` 是第一点时间。负载：

- `channel_table_revision: u32`
- `first_sample_index: u64`
- `sample_period_ticks: u32`
- `sample_count: u16`
- `selected_channel_count: u16`
- `channel_ids: [u16; selected_channel_count]`
- `sample_data: [u8]`

数据按 sample-major、通道 ID 列表顺序交错。每个值的宽度由当前通道表描述符决定。解析器必须在分配前用 checked arithmetic 计算期望长度，并拒绝修订号未知、通道重复、格式未知、长度不精确或 `sample_count == 0` 的批次。

`sample_period_ticks == 0` 非法。时间为：

`time_seconds = (timestamp_ticks + sample_offset * sample_period_ticks) / tick_hz`

客户端使用 `sequence` 检测帧级丢失，使用 `first_sample_index` 检测样点级缺口。两者不一致时以样点索引生成 gap，协议错误另行计数。

### 4.12 STATUS 与 ERROR

STATUS 负载：

- `state: u8`：0 idle，1 configured，2 streaming。
- `reserved: [u8; 3]`
- `produced_samples: u64`
- `dropped_samples: u64`
- `tx_overruns: u32`

ERROR 复用 `COMMAND_RESULT` 的结果码和 detail，但 `request_sequence` 可为 0，表示异步设备错误。

## 5. 客户端架构

新增 `src/live/`，按职责拆分：

- `protocol.rs`：帧、消息、CRC32C、增量解析、负载校验。
- `transport.rs`：串口/TCP 连接配置与统一 `Read + Write` 封装。
- `session.rs`：采集线程、命令/事件通道、握手、心跳、状态机和断线处理。
- `buffer.rs`：按样点容量限制的多通道环形缓冲、gap 和显示快照。
- `trigger.rs`：Auto/Normal/Single 软件触发状态机。
- `recording.rs`：`.scope` 顺序写入、干净关闭索引与中断恢复扫描。
- `scope_source.rs`：独立 `.scope` 离线 `DataSource`。
- `simulator.rs`：确定性信号发生器和 TCP 协议设备。
- `state.rs`：`LiveScopeState`，组合连接配置、会话句柄、缓冲、触发、录波和 UI 选择。

现有 `ScopeApp` 只新增 `pub live: LiveScopeState`。`src/app/live_ui.rs` 负责 egui 控件和绘图，不把实时线程、协议或录波字段平铺回 `ScopeApp`。

## 6. 并发与背压

- 一个采集线程独占串口或 TCP stream，避免 UI 线程阻塞。
- UI→采集线程使用有界命令通道；采集线程→UI 使用有界事件通道。
- SAMPLE_BATCH 事件队列满时允许丢弃显示批次，但必须增加 `host_dropped_batches` 并生成 gap；控制、错误和连接状态事件不得静默丢失。
- 录波使用独立有界写队列。队列满或写入失败时立即停止录波、保留可扫描文件并向 UI 报错，不允许继续生成看似完整的损坏录波。
- 断开或退出时发送 STOP，关闭 transport，join 工作线程；Drop 只做有限等待，避免桌面应用退出卡死。

## 7. 实时缓冲与绘图

- 缓冲容量由“保留秒数 × 实际采样率”计算，并设置硬上限防止错误配置耗尽内存。
- 每个通道使用共享时间轴和 `VecDeque<f32>`；批量写入时保持通道长度一致。
- gap 以断点记录，绘图不得跨缺口连线。
- UI 每帧只获取一个不可变快照；按像素预算生成 min-max 包络，避免把全部高频样点交给 egui_plot。
- 暂停显示只冻结视图，不停止采集或录波；用户可单独发送 STOP 停止设备。

## 8. 软件触发

V1 触发在客户端执行，不要求 DSP 固件实现触发：

- 模式：Auto、Normal、Single。
- 边沿：Rising、Falling、Either。
- 参数：源通道、level、hysteresis、pre-trigger samples、post-trigger samples、auto timeout。
- Rising 在样值从 `<= level - hysteresis/2` 到 `>= level + hysteresis/2` 时触发；Falling 相反。
- pre-trigger 由环形历史缓冲提供；触发后继续收集 post-trigger 样点，形成冻结 capture。
- Normal 未触发时持续等待；Single 完成一次 capture 后自动 Arm=false；Auto 超时后生成未命中边沿的滚动 capture，并明确标记 `auto_timeout=true`。
- gap 会重置边沿前值，禁止跨缺口误触发。

## 9. `.scope` 录波格式

### 9.1 文件头

- magic：8 字节 `SCOPEV1\0`。
- `format_version: u16 = 1`。
- `header_len: u16 = 32`。
- `metadata_len: u32`，最大 1 MiB。
- `created_unix_ns: u64`。
- `flags: u32`。
- `reserved: u32 = 0`。
- 紧随 UTF-8 JSON metadata，包含设备、tick_hz、通道表、实际采样参数和客户端版本。

### 9.2 记录

每条记录包含：

- magic：4 字节 `REC1`。
- `record_type: u8`：1 SampleFrame，2 Gap，3 Trigger，4 SessionEnd，5 Index。
- `flags: u8`。
- `reserved: u16`。
- `payload_len: u32`。
- `timestamp_ticks: u64`。
- payload。
- CRC32C，覆盖 record_type 至 payload，不覆盖 magic。

SampleFrame payload 保存完整且 CRC 已验证的 SCP1 `SAMPLE_BATCH` 帧字节，保留原始整数数据与协议证据。Gap 记录保存起始样点、缺失数量和原因。Trigger 保存触发参数与样点索引。

### 9.3 索引与恢复

- 干净停止时写 Index 记录，记录 sample index、时间戳和文件偏移，再写 SessionEnd。
- 打开文件时优先使用有效 Index；索引缺失、CRC 错误或录波中断时顺序扫描有效 `REC1`，在第一个不完整尾记录处安全停止并重建内存索引。
- 中间记录 CRC 错误标记文件损坏并返回错误，不静默跨越；只有不完整文件尾允许恢复。
- `ScopeRecordingDataSource` 实现现有 `DataSource` 的范围读取和 min-max 摘要，但实时 session 本身不实现 `DataSource`。

## 10. 模拟器

提供 `scope_dsp_simulator` 可执行程序：

- 默认 4 个模拟通道：正弦、相移正弦、锯齿和数字方波。
- 默认 10 kHz 采样率、每帧 100 点、1 MHz tick 时钟。
- 支持命令行设置监听地址、采样率、批量大小、实时/加速模式、确定性噪声种子、主动断线和每 N 帧损坏/丢弃一帧。
- 严格执行 HELLO→CONFIGURE→START 状态机，并返回真实命令结果。
- 自动化测试使用无睡眠加速模式；人工演示使用按墙钟节流的实时模式。

## 11. UI 接入

- 顶部增加“离线 / 实时”工作区切换，不改变现有离线默认行为。
- 实时工具栏包含传输类型、端口/地址、波特率、连接、配置、开始、停止、暂停显示、录波和打开录波。
- 左侧显示通道可见性、颜色、倍率和触发源；中央为实时波形；右侧显示触发参数、连接/帧/样点/丢包/录波统计。
- 错误以现有错误横幅和实时状态区呈现，连接失败不得导致 UI panic。
- 打开 `.scope` 后切回离线工作区，并复用现有光标、测量、FFT、序分量和导出链。

## 12. 错误处理与安全限制

- 所有长度、计数、乘法和偏移使用 checked arithmetic，先验证后分配。
- UTF-8、通道表、采样布局、session_id、revision 和消息状态均显式校验。
- 最大 64 通道、4096 点/批、1 MiB payload、1 MiB metadata。
- 协议噪声、CRC 错误、未知消息、序号缺口、设备丢样和主机事件丢弃分别统计。
- 录波文件绝不把未校验的 SAMPLE_BATCH 写成有效样本。
- 日志不得包含任意大 payload；仅记录帧类型、序号、长度和错误摘要。

## 13. 测试与验收

按 TDD 分里程碑：

1. 协议 golden bytes、CRC、拆包/粘包、噪声重同步、损坏帧、长度上限和采样解码。
2. 环形缓冲容量、gap、包络和多格式工程值。
3. Rising/Falling/Either、hysteresis、pre/post、Auto/Normal/Single 和 gap 重置。
4. `.scope` 往返、随机范围读取、摘要、索引、截断尾恢复和中间 CRC 损坏拒绝。
5. TCP 模拟器握手、配置、开始、采集、停止、丢帧统计和重连。
6. 客户端—模拟器—录波—DataSource 回放端到端闭环。
7. `ScopeApp` 默认离线行为、实时状态组合、版本同步和已有功能回归。

每个里程碑运行针对性测试与完整可运行测试，更新 `docs/implementation/live-scope-progress.md` 后独立提交。最终推送前运行格式化、clippy、完整测试和可用平台上的发布脚本；Windows 专属预检与真实 DSP 测试若当前环境不可用，必须如实标记未验证。

## 14. 版本与兼容性

- 该功能改变用户可见行为并新增 `.scope` 格式，版本从 0.7.1 升至 0.8.0。
- 同步 `Cargo.toml`、`Cargo.lock` 根包版本、`scripts/package-windows.ps1`、`scripts/ScopeAnalyzer.wxs` 和 README 产物名。
- 保留 Rust 2021、eframe/egui/egui_plot 0.27、wgpu/glow 双路径和全部 `[patch.crates-io]`。
- 不改动既有 CSV、DAT 或 JSON 配置格式。

## 15. 已知非目标

- V1 不提供远程固件升级、寄存器读写、JTAG 调试或设备控制脚本。
- V1 不保证跨多个独立 DSP 的硬件同步。
- V1 不实现设备侧触发或采样重传。
- V1 不把实时 session 伪装成文件型 `DataSource`。
- V1 不将未进行的硬件实测描述为已验证。
