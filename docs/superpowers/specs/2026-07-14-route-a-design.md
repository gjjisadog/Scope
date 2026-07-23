# Route A：工程闭环与发布基线设计

**状态：** 已获用户指示进入实施；按阶段审查与验收

## 目标

把 Scope 从“功能丰富的离线/实时波形分析工具”推进为可复现的工程验证工具，形成：

> 导入或采集 → 分析 → Reference/Compare → 规则判定 → 证据报告 → CLI/CI 自动化

路线 A 不采用一次性重写。每个阶段都必须保留现有 SCP1 V1、`.scope` V1 和已有离线分析能力，并提供可独立回归的产物。

## 当前基础与边界

- `DataSource` 是离线、Live Snapshot 和录波回放共用的数据边界，保持兼容。
- `ScopeApp` 继续作为 eframe UI 外壳；新增业务逻辑不得继续堆入 `src/app.rs`。
- `.scopeproj` 当前为 schema v1；新增对比、规则和报告配置使用迁移后的 v2 模型。
- 现有 `src/measurements.rs`、`src/fft.rs`、`src/transforms.rs`、`src/repid_derived.rs` 的算法语义优先复用，避免 UI 和 CLI 各自实现。
- 桌面端、CLI 和 VS Code 扩展最终调用同一组无 UI 应用服务。

非目标：多设备硬件时钟同步、通用 JTAG/寄存器控制、远程固件升级、插件市场和微服务化。

## 阶段划分

### 阶段 0：0.11.1 稳定基线

交付以下内容：

1. 修复 DOCX 游标表真实值输出和 P/Q/S 单位显示。
2. VS Code 扩展在未信任工作区禁用 workspace binary bridge，并拒绝仅凭“文件存在”执行工作区二进制。
3. 增加 CI：fmt、check、普通测试、严格 Clippy、性能门禁和 Windows 打包检查。
4. 固定 Rust/WiX 工具链；完整构建使用 `--locked --offline`，并补齐依赖缓存或 vendor 方案。
5. 固定 Mesa/ANGLE 输入哈希，产物生成 SHA256、SBOM、第三方许可和构建 provenance。
6. 建立 `.scope`、`.scopeproj`、SCP1 V1 的兼容样本；协议、录波、工程文件和 bridge 输入增加 fuzz/property 测试。
7. 完成 Win10/Win11 安装、升级、卸载、普通用户、WARP/Mesa/ANGLE 和 RDP 启动烟测。

出口条件：所有 PR required checks 通过；干净断网 Windows runner 能生成 ZIP/MSI；旧文件可打开；本阶段新增行为均有回归测试。

### 阶段 1：0.12 Reference/Compare MVP

新增纯核心模块：

- `compare/model`：Reference/Test 角色、通道映射、比较配置和结果模型；
- `compare/alignment`：手动偏移、触发点、阈值事件和基频相位四种对齐方式，附带置信度；
- `compare/metrics`：差值、绝对误差、相对误差、上/下容差、超差区间和摘要。

首版只支持同一工程内的可解释对比，不承诺跨设备时钟漂移补偿。不同采样率使用明确的重采样策略；任何 gap 不得被静默连接或当作有效零值。

工程文件升级为 schema v2，提供 v1→v2 显式迁移，不接受“只改版本号”的隐式兼容。

出口条件：已知延时/幅值误差 fixture、不同采样率 fixture、gap fixture 全部通过；GUI、PNG/SVG、DOCX、JSON 的比较结果一致；保存、重载和迁移后结果不变。

### 阶段 2：0.13 规则、报告和 CLI

新增确定性规则引擎：

- 规则范围：绝对时间窗、事件相对时间窗；
- 输入：原始信号、派生信号或 MeasurementEngine 指标；
- 比较：阈值、容差、持续时间和严重度；
- 结果：Pass、Fail、Invalid，附实际值、证据时间窗和数据质量原因。

新增一键工程报告，包含数据来源哈希、应用/schema 版本、触发与 gap 质量、关键测量、Compare 摘要、规则结果和证据图。自动结论只能来自已计算结果，不生成无证据的自然语言判断。

新增稳定 `scope-cli`：`inspect`、`analyze`、`compare`、`test`、`report`、`validate-recording`、`project` 均使用带 schema 版本的 JSON envelope、稳定错误码和适合 CI 的退出码；其中 `test` 在保留完整 `ok: true` 规则结果的同时，若 `passed` 为 false 返回退出码 5。分析命令复用现有 DataSource/MeasurementEngine，工程命令支持 V1→V2 迁移后的校验与写回。

`validate-recording` 即使能够恢复不完整的文件尾，也必须把缺少 clean
`SessionEnd` 或存在 `recoveredTail` 的录波标记为 `valid: false` 并返回退出码
5；JSON envelope 仍保留扫描到的记录和质量字段，供诊断使用。

VS Code 扩展降级为 CLI/应用服务的薄客户端，保留现有能力并增加 bridge 版本协商；不得继续维护独立分析语义。

出口条件：同一工程 GUI 与 CLI 输出逐字段一致；Golden corpus 全部通过；Invalid 数据不会误报 Pass；批处理可在无 UI 环境运行并生成报告。

### 阶段 3：1.0 RC

冻结 SCP1 V1、`.scope` V1、工程迁移、CLI JSON、规则 JSON 的兼容策略；完成 Windows 原生安装与升级矩阵、硬件采集 smoke、性能回归、签名和发布证据归档。

1.0 的定义是：一份数据或 Capture 可以被保存为工程，经过 Compare 和规则判定，生成可追溯报告，并可由 CLI 在另一台机器复现相同结果。

## 架构原则

1. UI → application services → core algorithms；core 不依赖 egui/rfd。
2. 新功能必须落在独立模块，`src/app.rs` 不再承载新的核心算法。
3. 每个持久化字段必须有 round-trip、迁移和缺失字段测试。
4. 每个外部输入解析器必须有兼容 fixture、拒绝路径和资源上限测试。
5. 先抽取纯函数和数据模型，再接 UI；UI 只负责选择、调度和呈现。
6. 保持单仓库和现有后台任务模型，避免为路线 A 引入全局 async 或服务拆分。

## 首轮实施顺序

1. 0.11.1 缺陷修复、VS Code 信任边界和版本/文档同步。
2. CI、工具链和真正离线构建；补齐兼容 fixture。
3. 对 `ScopeApp` 的现有行为建立 characterization tests，并按 Workspace、Analysis、Export、Project、Jobs 分组状态；此步不改变用户行为。
4. 先实现 Compare 纯核心和 Golden fixtures，再接工程文件迁移，最后接 UI 和导出。

## 质量门禁

- 新增行为必须遵循 Red → Green → Refactor；先看到失败测试，再写生产代码。
- 协议、录波、工程文件和 Compare 核心的覆盖率目标为 90%；整体目标为 75%。
- 外部输入 fuzz 持续运行至少 100 万次样本，无 panic 或 OOM。
- 性能基线中加入 Live measurement p95；固定 runner 回退超过 10% 时阻断。
- 发布包必须有版本、源码 commit、工具链、依赖、运行时哈希和测试证据。

## 风险与取舍

- Compare 先支持“同一工程、可解释对齐”，不提前承诺跨设备硬同步。
- 规则首版只做确定性结构化规则，不做自然语言执行。
- 公式库、滚动录波、COMTRADE、全频谱/时频和更复杂触发安排在 1.0 后，除非真实样本证明它们是当前阻塞项。
- 若现场调试是主要 KPI，可在阶段 1 后插入 Live THD/序分量/dq0，但不能跳过阶段 0 的发布和信任边界。
