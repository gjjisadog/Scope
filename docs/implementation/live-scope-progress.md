# DSP 实时软件示波器 V1 实施进度

最后更新：2026-07-11

## 当前里程碑

里程碑 8 已完成：跨平台测试基线修复与发布前全量回归。

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
- 已实际启动 `scope_dsp_simulator --accelerated`，监听 `127.0.0.1:19090`；桌面 `scope_analyzer` 也成功启动且无启动期错误。
- 里程碑 7 TDD RED：版本同步测试期望 0.8.0，首次运行在 `CARGO_PKG_VERSION=0.7.1` 失败；同步所有版本位置后通过。
- `cargo +1.87.0 test --lib release_tests::live_scope_release_version_is_synchronized`：1/1 通过。
- 回归修复前稳定复现原基线 4 个失败；逐项修正后 4 个目标测试均单独通过。
- `cargo +1.87.0 test --quiet`：全部通过；library 51/51、桌面客户端 111/111（另 5 个显式 ignored）、模拟器 1/1、doc tests 0。
- 最终 `cargo +1.87.0 fmt --all --check`：通过。
- 最终 `cargo +1.87.0 clippy --all-targets --quiet`：退出码 0；保留 vendor eframe 与既有离线模块的非阻断警告，没有新增实时模块警告。
- 最终 `cargo +1.87.0 test --quiet -- --ignored`：5/5 ignored 性能/兼容性测试通过。
- 最终 `cargo +1.87.0 build --release --bins`：通过，生成 release 客户端与 DSP 模拟器二进制。

## 未验证内容

- 尚未验证 DSP 实物串口通信。
- 尚未验证 Windows 串口枚举、连接、断线恢复与安装包运行。
- 尚未运行 Windows `scripts/release-check.ps1` 和离线安装包构建。
- 未完成人工点击桌面 UI 的运行验证：当前开发二进制没有 macOS `.app` 包装，辅助功能层无法识别其窗口；已有编译、单元测试、端到端 session/录波测试及两个进程启动证据。

## 后续任务

1. 正常推送 `feature/live-dsp-scope-v1`。
2. 获得目标 DSP 板卡和 Windows 测试机后执行硬件/安装包验收。

## 硬件实测状态

未进行硬件实测。当前没有 DSP 板卡型号、物理接口、串口参数或既有帧格式资料，不能把模拟器验证描述为硬件验证。

## 已知限制

- Rust 1.97.0 会因更严格的浮点类型推断在现有 `src/app.rs` 中编译失败；Rust 1.87.0 能执行基线测试。
- 当前已完成无硬件依赖的客户端—模拟器闭环和 egui 实时工作区；真实 DSP 串口硬件实测尚未完成。
