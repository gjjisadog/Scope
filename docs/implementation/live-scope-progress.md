# DSP 实时软件示波器 V1 + V2 实施进度

最后更新：2026-08-09（Scope Analyzer 0.15.4）

## 当前里程碑

里程碑 13：SCP1 V2 R1/R2 二进制冻结与三 Stream 实时闭环。

## V2 本阶段状态

- V1 帧、golden frame、默认 GUI、LiveBuffer 与 `.scope V1` 不变；V2 只能由显式 Session/CLI/模拟器入口选择。
- 已实现 FAST32K/CTRL8K/SLOW1K fixed stream、owner/role 通道 metadata、domain/phase/group 校验、`row/source/applied` 元数据与客户端诊断计数。
- 已实现 V2 hardware Capture 配置、状态、内存组装和完整性验证；本阶段不写入 `.scope V1`。
- TCP simulator 提供可重复的 `30k-*` V2 预设；这是 simulator/client 验证，不是 DSP 硬件验证。
- 现有 LAUNCHXL-F28P65X V1 客户端实测仍是 V1 证据；真实 Hybrid30K 的 CPU1/CPU2/CLA V2 冻结实现、DMA/RAMGS 与真机验收均未进行。
- 异步 V1 录波 writer 的实际有界队列容量为 **1024** 项；V2 Capture 不进入该队列。
- 0.15.1 完成 V2 加固：CausalRelation 跨 stream 逻辑序号、Capture CRC32C/边界、精确 stream/binding 集合、固定 period、双向心跳、STATUS/ERROR 和主机背压诊断均已由单元及 15 preset 会话矩阵覆盖。
- 0.15.2 将 `logical_cycle_sequence` 与 stream-local `row_sequence` 分离，加入有界乱序因果匹配、Capture 失败后会话恢复与终态缓存释放、严格 row/timestamp 映射、多 nonce 心跳窗口，并把 15 preset 会话断言细化为逐项语义检查。
- 0.15.3 将 R1 固定为 28-byte/0x32，并以 0x33/34/35/47 建立 R2；实现 1..8 Stream 原子订阅、32 kHz logical time（step 1/4/32）、正负 offset 和 watermark deadline。
- 0.15.4 增加 Hybrid30K R2 Machine Profile、静态 DSP Golden Vectors，以及 handshake/CTRL8K/multistream/link-stress 验收模式；TCP 结果只作为 software smoke。
- R2 Snapshot 默认使用 48-byte affine base + sparse override；自动带宽门禁覆盖 4 Mbaud 70% 阈值。Capture R2 使用纯长度、相邻块检查、Arc block 与增量 CRC，失败后连接可复用。
- DeviceReset 清空全部 session-local 状态并拒绝旧 id，等新 HELLO_ACK/ChannelTable/StreamTableR2 后回到 Ready，不自动恢复订阅或 Streaming。heartbeat 分离统计 RTT/timeout/unknown/duplicate/overflow。
- CI 增加 focused live、simulator 和独立 100 万行 causal bounded job；这些仍是软件模拟证据，不是真实 Hybrid30K 硬件验证。

## 已完成内容

- 已确认 GitHub 仓库为 `gjjisadog/Scope`。
- 已执行 `git fetch origin`；当前分支为 `feature/live-dsp-scope-v1`，且开始工作时与 `origin/main`、`github/main` 同为提交 `97d82e4`。
- 已确认开始工作时工作区干净，没有删除、覆盖、重置或 stash 用户修改。
- 已盘点现有 `ScopeApp`、后台任务、离线 `DataSource` 与绘图接入点。
- 已确认实时会话不能复用文件型 `DataSource`；完成的 `.scope` 录波文件将使用独立离线适配器。
- 已确认该用户可见重大功能需要从 `0.7.1` 升级版本，并同步 Cargo、PowerShell、WiX 和 README 产物名。
- 用户已授权由本项目定义 V1 协议。
- 已定义串口/TCP 共用的 SCP1 二进制帧、CRC32C、握手、通道表、配置、采样、状态和错误消息。
- 已定义客户端软件触发、并发背压、可恢复 `.scope` 格式、独立离线 DataSource、TCP 模拟器和 UI 组合边界。
- 已新增 library 边界，桌面程序复用同一 `data` 模块，避免产生两套不兼容的 `DataSource` trait。
- 已实现 SCP1 固定帧头、1 MiB 负载上限、精确长度校验、Castagnoli CRC32C 和帧往返编解码。
- 已实现增量字节流解析，可处理噪声、拆包、粘包、坏 CRC，并逐字节重同步到下一帧 magic。
- 已实现 HELLO/ACK、CHANNEL_TABLE、CONFIGURE、START/STOP、COMMAND_RESULT、PING/PONG、STATUS、ERROR 和 SAMPLE_BATCH 负载。
- 已实现 1..64 通道表校验、UTF-8/精确长度/保留位检查，以及 `i16`、`i32`、`f32`、`u8` 混合采样到工程值的解码。
- 已实现通道表修订匹配、数字量范围、样本数据精确长度，以及样点索引和时间戳 checked arithmetic。
- 已实现多通道对齐环形缓冲、容量淘汰、自动/显式 gap、按 gap 分段的显示快照和尖峰保留 min-max 采样。
- 已实现 Auto/Normal/Single 软件触发、Rising/Falling/Either 边沿、hysteresis、pre/post capture、Auto 超时和 gap 状态重置。
- 缓冲与触发在布局、索引或时间戳非法时先返回错误，不产生部分状态更新。
- 已实现 `.scope` 文件头、JSON 会话元数据、带 CRC32C 的 SampleFrame/Gap/Trigger/Index/SessionEnd 记录。
- 录波只接受通过协议和通道表校验的 SAMPLE_BATCH；干净结束写入索引和结束标记。
- 已实现顺序扫描恢复：截断的最终记录可恢复，文件中间 CRC 错误会拒绝打开，不静默跳过。
- 已实现独立 `ScopeRecordingDataSource`，支持范围读取、抽取、min-max 摘要和取消令牌，并复用现有离线分析 trait。
- 已加入有界 `crossbeam-channel` 和 `serialport`，实现可配置 TCP/串口 transport、串口枚举、8N1/无流控和读写超时。
- 已实现采集 worker：握手、通道表、配置、Start/Stop、心跳、session_id 校验、采样解码、gap/统计和 3 秒失联检测。
- UI 事件背压丢弃显示批次时会累计 HostBackpressure gap，不静默跨缺口连接波形。
- 已实现确定性 TCP DSP 模拟器和独立 CLI，支持实时/加速、采样率、批量大小、seed、周期丢帧/损坏和主动断线。
- 已实现 `LiveScopeState` 组合层，持有 session、buffer、trigger、recording 和统计；端到端测试完成连接、采集、录波、停止和 DataSource 回放。
- 已通过单一 `live: LiveScopeState` 字段把实时模块组合进 `ScopeApp`，没有把会话、触发、缓冲和录波状态继续平铺到主应用。
- 已实现离线/实时工作区切换；实时工作区包含 TCP/串口连接、采集配置、Start/Stop、暂停显示、通道可见性/颜色、触发配置、实时统计和分段波形显示。
- 已实现 `.scope` 文件对话框入口；录波回放通过独立 `ScopeRecordingDataSource` 进入既有离线分析工作区。
- 已把已连接时的界面刷新节流为约 60 Hz；采集线程和协议处理不在 egui 绘制线程中阻塞。
- 已将重大功能版本从 0.7.1 同步升级到 0.8.0，覆盖 Cargo manifest/lock、Windows PowerShell 打包、WiX Product Version 和 README 产物名。
- 已发布 `docs/protocols/scp1-live-scope-v1.md`：冻结帧布局、CRC32C、消息方向、握手/状态机、所有 payload、采样交织方式、gap 和错误处理要求。
- README 已加入实时工作区、模拟器、TCP/串口、触发、录波与回放操作说明，并明确真实硬件尚未验证。
- 已修复最近文件标签在非 Windows 主机读取反斜杠路径时不截取文件名的问题。
- 已把 Mesa helper、窗口 maximized 和导出光标表测试改为平台路径/builder 真实语义/字体度量不变量，消除测试对 Windows 路径解析和特定系统字体像素值的错误假设。
- 已将 `feature/live-dsp-scope-v1` 正常推送到 `github`（`gjjisadog/Scope`），未 force push、未合并 main、未创建 Pull Request。
- 完成性复审发现初版 CHANNEL_TABLE 字符串长度排列与冻结设计不一致、协议文档缺少固件 golden frame、配置未对设备协商上限做统一校验；已以新测试先复现并修正。
- CHANNEL_TABLE descriptor 已冻结为“固定字段、unit/name 两个长度、unit/name 两段字节”的布局；PING 完整 golden frame 和 CRC 已写入固件协议文档。
- 已新增设备约束校验和 CONFIGURE 成功 detail 的规范编码/解码，覆盖 tick_hz、最大批量、通道 mask 和协商 payload 上限。
- 历史初版曾使用 128 项 writer 队列；当前有效 V1 录波 writer 容量为 **1024** 项。队列满、worker 写入失败和 worker panic 均有显式错误，录波立即停止且只保留可恢复前缀。
- Trigger 记录升级为固定 48 字节完整记录，包含 mode、edge、源通道、level、hysteresis、pre/post、Auto timeout、触发样点和超时标志。
- 干净文件打开时会逐项核对 Index 与实际 SampleFrame 索引/时间戳/文件偏移；有效 CRC 但内容不一致的 Index 也会拒绝。
- `LiveScopeState` 已接入异步录波统计、pending 数量和 worker 故障轮询；显式结束录波仍等待 Index/SessionEnd 与 `sync_all` 完成，应用异常退出则不无限等待并由恢复扫描处理。
- 采集 worker 现在通过可克隆 `RecordingIngress` 在显示队列之前录制已验证原始帧；显示背压只影响实时画面，不再造成 `.scope` 缺帧。
- 会话控制事件与高频 Batch/Stats 分离为两个有界通道，样点 gap 不再以阻塞发送卡住 Disconnect；命令/控制通道均有 100 ms 上限。
- CONFIGURE 成功会解析设备返回的实际采样率、批量与 mask，再次对 HELLO_ACK/CHANNEL_TABLE 校验后发布 `Configured`；客户端只在该事件后允许 Start/录波并重建缓冲。
- 模拟器会拒绝超出 tick、最大批量、payload 或通道表的配置；新增连接/命令/批次统计用于验证状态机。
- Streaming 正常断开会先发送 STOP；显式 Disconnect join worker，Drop 只发非阻塞 Disconnect 并 detach，避免串口驱动或满队列造成应用退出无限等待。
- 新增 CRC/malformed/discarded/unknown/device drop/tx overrun 统计；坏 CRC 帧只计数，不会到达 Batch 消费者。
- 异常断线会立即结束录波状态、保留未写 SessionEnd 的可恢复文件前缀并在 UI 错误区说明。
- 修正默认 UI 采集 mask：收到 CHANNEL_TABLE 后自动收敛到设备已知通道，默认参数可直接 Configure；用户可分别控制“是否采集”和“是否显示”。
- 实时通道 UI 已补齐每通道显示倍率；串口提供 7 个设计预设并保留手工正整数波特率输入。
- 触发 UI 已补齐 Auto timeout 样点数、命中样点/超时标记和统一重新布防；Normal/Single 使用完整 pre/post capture 冻结显示，倍率只影响绘图不改变工程值触发判定。
- 链路统计已补齐 CRC、discarded bytes、unknown messages、device drops/overruns；录波统计显示已写 SampleFrame/Gap/Trigger、总记录和 pending，并在结束录波后保留最终值。
- 实时快照的 `max_points` 现在是所有 gap segment 共享的总预算；多段历史不再各自使用完整预算。触发完成后继续消费同一 SAMPLE_BATCH 的剩余样点，保留下一次 pre-trigger 历史。
- 客户端与模拟器现已在会话建立后双向每秒主动发送 PING，并分别回复同 nonce 的 PONG；Streaming 状态不暂停心跳，3 秒有效帧超时规则保持不变。
- 模拟器 `--accelerated` 已改为不做定时 sleep 的加速时钟，只通过 `yield_now` 让出线程；按墙钟演示仍使用默认实时模式。
- 补齐 `.scope` 防御性与集成验证：超限元数据在分配前拒绝、内外 CRC 正确但 SAMPLE_BATCH 内容非法仍拒绝、回放保持调用方请求的通道顺序，桌面应用扩展名分派实际返回独立 `ScopeRecordingDataSource`。
- SCP1 增量解析器新增超大 payload 候选后的重同步验证，非法候选不会阻塞其后的有效帧。
- 真实 `LAUNCHXL-F28P65X` + XDS110 Application/User UART 已完成 SCP1 串口实测；验收配置为 115200 baud、四通道、500 Hz、每批 10 点。
- 已定位三秒失联的根因是 DSP 发送路径内递归解析 RX 导致 C28x 卡在编码栈；独立 TI Demo 固件已改为 TX 期间只暂存 RX、主循环统一解析。
- 0.8.1 修复客户端触发回差算法：边沿状态会跨多个渐进样点保持施密特锁存，不再要求相邻样点一次跨过整个回差带。
- 链路序号统计改为检查所有设备帧的全局 sequence；合法插入的 PING/PONG/STATUS 不再被误报为样本缺口。
- 异常断线后会释放已结束的 `LiveSession`，用户可直接重新连接，不再出现 Disconnected 状态下仍提示 already connected。
- 实时默认采集参数调整为真机安全的 500 Hz / 每批 10 点；切换串口默认 115200 baud。
- “应用并开始”会在需要时自动 CONFIGURE 并等待设备确认后 START；录波只在 Streaming 时启用，防止空录波。
- 结束录波后新增“回放刚才录波”，可直接通过 `ScopeRecordingDataSource` 进入离线工作区。
- CJK 字体搜索扩展到 Windows、macOS 和常见 Linux 字体位置，并支持 `SCOPE_CJK_FONT` 显式覆盖；CJK 字体作为 fallback 保留默认拉丁字体。
- 版本已同步升级到 0.8.1，覆盖 Cargo、Cargo.lock、PowerShell、WiX、README 和版本同步测试。

## 测试结果

- 主机开始时没有 Rust 工具链；已把官方 rustup/Cargo 隔离安装到被忽略的 `target/codex-toolchain/`，没有修改全局 shell 配置。
- Rust 1.87.0 可编译并执行现有基线测试。
- 基线命令：`cargo +1.87.0 test --quiet`。
- 基线结果：138 个测试中 129 个通过、4 个失败、5 个忽略。
- 失败均发生在未修改的基线：
  - `app::tests::export_overlay_annotations_gate_variable_labels_but_not_cursor_table`
  - `app::tests::recent_file_label_keeps_menu_compact`
  - `tests::mesa_renderer_uses_isolated_helper_executable`
  - `tests::scope_window_starts_resizable_with_taskbar_friendly_bounds`
- 上述 4 个基线失败已在里程碑 8 按平台语义修正；当前全量非 ignored 测试通过。
- 里程碑 1 TDD RED：测试因 `Frame`/`crc32c` 缺失失败；实现后 2 个测试通过。
- 里程碑 1 TDD RED：流式测试因 `FrameDecoder` 缺失失败；实现后协议测试 3/3 通过。
- `cargo +1.87.0 fmt --all --check`：通过。
- `cargo +1.87.0 test --no-run`：通过，library 和桌面二进制测试目标均编译成功。
- 里程碑 2 TDD RED：控制消息/通道表/采样测试因负载类型缺失编译失败；实现后核心协议测试 6/6 通过。
- 里程碑 2 TDD RED：样点索引溢出测试在缺少 checked arithmetic 时断言失败；补充索引与时间戳校验后通过。
- `cargo +1.87.0 test --lib live::protocol::tests`：12/12 通过。
- `cargo +1.87.0 clippy --lib --quiet`：退出码 0；仅有 vendor eframe 既有警告。
- 里程碑 3 TDD RED：缓冲/触发测试因类型与状态机缺失编译失败；实现后新增测试全部通过。
- `cargo +1.87.0 test --lib live::`：20/20 通过。
- 里程碑 3 的 `cargo +1.87.0 fmt --all --check` 与 `cargo +1.87.0 clippy --lib --quiet`：退出码 0，仅有 vendor eframe 既有警告。
- 里程碑 4 TDD RED：录波往返测试因 writer、scanner 和 DataSource 不存在而编译失败；实现后录波测试通过。
- `cargo +1.87.0 test --lib live::`：23/23 通过，包括截断尾恢复和中间 CRC 损坏拒绝。
- 里程碑 4 的 `cargo +1.87.0 fmt --all --check` 与 `cargo +1.87.0 clippy --lib --quiet`：退出码 0，仅有 vendor eframe 既有警告。
- 里程碑 5 TDD RED：TCP session 测试因 transport/simulator/session 类型缺失编译失败；实现后握手与连续采样测试通过。
- 里程碑 5 TDD RED：组合闭环测试因 `LiveScopeState` 缺失编译失败；实现后客户端—模拟器—录波—回放通过。
- `cargo +1.87.0 test --lib live::`：27/27 通过，包括丢帧 gap 与端到端录波回放。
- `cargo +1.87.0 test --bin scope_dsp_simulator`：1/1 通过。
- 里程碑 6 TDD RED：`ScopeApp` 组合测试在缺少 `LiveScopeState` 字段和初始化时编译失败；接入后通过。
- `cargo +1.87.0 test --bin scope_analyzer app::tests::scope_app_live_state_defaults_to_offline_workspace`：1/1 通过。
- `cargo +1.87.0 clippy --all-targets --quiet`：退出码 0；仅有 vendor eframe 和既有离线应用警告。
- `cargo +1.87.0 test --no-run`：通过，library、桌面客户端和模拟器全部测试目标编译成功。
- 已实际启动 `scope_dsp_simulator --accelerated` 并监听 `127.0.0.1:19090`。桌面二进制能进入 renderer launcher，但本机窗口渲染状态见“未验证内容”，不再以进程存在推断 UI 成功。
- 里程碑 7 TDD RED：版本同步测试期望 0.8.0，首次运行在 `CARGO_PKG_VERSION=0.7.1` 失败；同步所有版本位置后通过。
- `cargo +1.87.0 test --lib release_tests::live_scope_release_version_is_synchronized`：1/1 通过。
- 回归修复前稳定复现原基线 4 个失败；逐项修正后 4 个目标测试均单独通过。
- `cargo +1.87.0 test --quiet`：全部通过；library 51/51、桌面客户端 111/111（另 5 个显式 ignored）、模拟器 1/1、doc tests 0。
- 最终 `cargo +1.87.0 fmt --all --check`：通过。
- 最终 `cargo +1.87.0 clippy --all-targets --quiet`：退出码 0；保留 vendor eframe 与既有离线模块的非阻断警告，没有新增实时模块警告。
- 最终 `cargo +1.87.0 test --quiet -- --ignored`：5/5 ignored 性能/兼容性测试通过。
- 最终 `cargo +1.87.0 build --release --bins`：通过，生成 release 客户端与 DSP 模拟器二进制。
- 里程碑 9 TDD RED：CHANNEL_TABLE golden layout 测试实际得到 `unit_len, unit, name_len, name`，与冻结设计不符；调整 codec 后通过。
- `cargo +1.87.0 test --lib live::protocol::tests`：15/15 通过，包括完整帧 golden bytes、descriptor golden bytes 和设备配置协商约束。
- 录波 TDD RED：Trigger 完整配置 API/读取结果和 Index 内容一致性均缺失；实现后两个目标测试通过。
- 异步录波 TDD RED：`AsyncScopeRecorder`、QueueFull/WorkerFailed 和统计 API 缺失；实现后正常完成、确定性队列溢出、worker 故障传播 3 个测试通过。
- `cargo +1.87.0 test --lib live::recording::tests`：8/8 通过。
- `cargo +1.87.0 test --lib live::state::tests::simulator_acquisition_records_and_replays`：通过，异步 writer 至少落盘 3 帧后完成客户端—模拟器—回放闭环。
- 会话 TDD RED：Configured 实际参数事件、CRC 统计、模拟器命令统计和 STOP 断开证据均缺失；实现后全部目标测试通过。
- `cargo +1.87.0 test --lib live::session::tests`：8/8 通过，覆盖配置协商、非法 mask、坏 CRC、丢帧、STOP、满显示队列有界断开和重新连接。
- `cargo +1.87.0 test --lib live::state::tests`：4/4 通过，包括显示队列 700 ms 不消费时录波样点仍显著多于显示样点，以及异常断线可恢复录波。
- 显示/触发 TDD RED：跨 gap snapshot 超出总预算、Normal capture 后丢弃批次尾部、默认 mask 保持 `u64::MAX`；修正后三个目标测试通过。
- `cargo +1.87.0 test --lib live::buffer::tests`：4/4 通过；`cargo +1.87.0 test --lib live::trigger::tests`：6/6 通过。
- `cargo +1.87.0 test --lib live::state::tests`：5/5 通过，包括 Normal capture 冻结显示、倍率计算和重布防清除 capture。
- `cargo +1.87.0 test --bin scope_analyzer app::tests::scope_app_live_state_defaults_to_offline_workspace`：通过，完整桌面测试目标编译成功。
- 双向心跳 TDD RED：测试因模拟器缺少主动 PING/PONG 计数而编译失败；实现后客户端主动心跳、设备主动心跳及双方响应均通过 TCP loopback 验证。
- `.scope` 新增超限元数据、非法内嵌 SAMPLE_BATCH、重排 wire channel/请求顺序测试；协议新增超大候选帧重同步测试，目标测试全部通过。
- `cargo +1.87.0 test --bin scope_analyzer opens_scope_recording_through_the_application_dispatch_path`：1/1 通过，验证 `.scope` 主应用分派、元数据和工程值读取。
- 无睡眠加速模式首次使 700 ms UI 背压测试超过模拟器 500 ms TCP 写超时；该测试改用墙钟实时模式来隔离“显示队列满但采集/录波继续”这一目标后通过，加速模式仍由其他 loopback/故障测试覆盖。
- `cargo +1.87.0 test --lib live::`：53/53 通过。
- 最终 `cargo +1.87.0 fmt --all --check`：通过。
- 最终 `cargo +1.87.0 clippy --all-targets --quiet`：退出码 0；仅保留 vendor eframe 和既有离线模块警告，新增实时模块无 clippy 警告。
- 最终 `cargo +1.87.0 test --quiet`：library 77/77、桌面客户端 112/112（另 5 个显式 ignored）、DSP 模拟器 1/1、doc tests 0，全部普通测试通过。
- 最终 `cargo +1.87.0 test --quiet -- --ignored`：5/5 性能/LibreOffice 兼容性测试通过。
- 最终 `cargo +1.87.0 build --release --bin scope_analyzer --bin scope_dsp_simulator`：通过，0.8.0 客户端和模拟器 release 二进制均生成成功。
- 0.8.1 全量普通测试：library 80/80、桌面客户端 115/115（另 5 个 ignored）、DSP 模拟器 1/1、doc tests 0，全部通过。
- 新增渐进信号跨非零回差带触发测试，修复前无法命中，修复后通过。
- 新增 CONFIGURE+START 单动作测试、异常断线 session 释放测试，以及控制帧/采样帧交织时 sequence gaps 为 0 的回归测试，全部通过。
- macOS 实际加载 `/System/Library/Fonts/Supplemental/Arial Unicode.ttf`，egui 字体系统确认包含“中文实时触发录波回放”全部字形。
- 0.8.1 真机客户端验收：300 个批次、3000 在线样本、CRC 0、协议错误 0、sequence gaps 0、主机/设备丢样 0、TX overrun 0。
- 0.8.1 真机使用 0.2 工程值非零回差完成 23 次真实 Normal 上升沿触发；录波包含 301 个 SampleFrame、3010 个连续样本、23 个 Trigger、0 个 Gap，并以干净 SessionEnd 结束。
- 0.8.1 真机 `.scope` 通过独立 DataSource 回放四通道，读取 1506 个抽取点；硬件验收结论为 `SCOPE HARDWARE CLIENT ACCEPTANCE: PASS`。
- 0.8.1 `cargo clippy --all-targets -- -D warnings`：通过；`cargo test -- --ignored`：5/5 通过；release 客户端与模拟器构建通过。

## 未验证内容

- 尚未在 Windows 实机验证 0.8.1 串口枚举、系统字体选择与安装包运行。
- 尚未运行 Windows `scripts/release-check.ps1` 和离线安装包构建。
- 本轮桌面控制通道无法初始化，因此未保存 0.8.1 UI 截图；中文字体已通过实际字体解析和 egui glyph 测试验证，实时功能已通过同一客户端状态机的真机验收。

## 后续任务

1. 在 Windows 构建机运行 `scripts/release-check.ps1`、离线 MSI/ZIP 打包及安装后串口/字体验证。
2. 发布前补拍中文实时界面、触发命中、录波统计和回放工作区截图。
3. 产品接入时用真实 ADC/变量采集替换 TI Demo 的确定性信号生成器，保留已验收 SCP1 传输层。

## 硬件实测状态

已实测。硬件为 `LAUNCHXL-F28P65X`（TMS320F28P650DK9）与板载 XDS110，串口为 `/dev/cu.usbmodemCL6500011`、115200 8N1。CPU1 RAM 镜像完成握手、配置、持续采样、双向心跳、非零回差触发、录波、停止和回放；CPU2、Flash 和 RAMGS 所有权未操作。最终 DSP 镜像 SHA-256 为 `01964ef65c3077cae3a8ff7235691bd62c4ebfb68a484831d3aa097906569c1f`。

## 已知限制

- Rust 1.97.0 会因更严格的浮点类型推断在现有 `src/app.rs` 中编译失败；Rust 1.87.0 能执行基线测试。
- TI Demo 当前生成确定性合成通道，尚未接入产品 ADC 或固件变量。
- 当前真机镜像为 CPU1 RAM 运行，断电或复位后需要重新加载。
