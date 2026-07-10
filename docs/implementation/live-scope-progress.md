# DSP 实时软件示波器 V1 实施进度

最后更新：2026-07-11

## 当前里程碑

里程碑 1：SCP1 帧编解码与流式重同步。

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
- 当前不能声明仓库全量测试通过；实时功能尚无生产代码或新增测试。
- 里程碑 1 TDD RED：测试因 `Frame`/`crc32c` 缺失失败；实现后 2 个测试通过。
- 里程碑 1 TDD RED：流式测试因 `FrameDecoder` 缺失失败；实现后协议测试 3/3 通过。
- `cargo +1.87.0 fmt --all --check`：通过。
- `cargo +1.87.0 test --no-run`：通过，library 和桌面二进制测试目标均编译成功。

## 未验证内容

- 尚未验证 DSP 实物串口通信。
- 尚未验证 Windows 串口枚举、连接、断线恢复与安装包运行。
- 尚未运行 Windows `scripts/release-check.ps1` 和离线安装包构建。
- 尚未验证客户端—模拟器端到端采集、触发、录波和回放闭环。

## 后续任务

1. 实现 SCP1 握手、通道表、控制消息和多格式采样解码。
2. 按 TDD 实施实时缓冲、触发、录波、回放、模拟器和 UI。
3. 每个可验收里程碑执行测试、更新本文件并独立提交。
4. 完成全量回归、版本同步和正常推送。

## 硬件实测状态

未进行硬件实测。当前没有 DSP 板卡型号、物理接口、串口参数或既有帧格式资料，不能把模拟器验证描述为硬件验证。

## 已知限制

- 当前仓库基线在 macOS 上已有 4 个失败测试，详见“测试结果”。
- Rust 1.97.0 会因更严格的浮点类型推断在现有 `src/app.rs` 中编译失败；Rust 1.87.0 能执行基线测试。
- 仓库没有现成的实时采集协议、串口模块、触发模块或 `.scope` 格式。
- 当前只完成帧层；握手消息、采样负载、连接会话、触发、录波和 UI 尚未实现。
