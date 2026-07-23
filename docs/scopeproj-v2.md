# `.scopeproj` V2

`.scopeproj` 是 Scope Analyzer 的 UTF-8 JSON 工程文件。V2 在 V1 的数据组、布局、分析、Live、Capture 和导出状态之上增加了可持久化的 Reference/Compare 配置；原始波形仍保存在引用的数据文件或 `.scope` 资产中，不嵌入工程 JSON。

## Compare 配置

- `referenceDatasetId` 和 `testDatasetId` 指向两个不同的数据组。
- `channelMappings` 保存参考通道与测试通道的对应关系。
- `alignment` 支持路线 A 的 `manualOffset`、`triggerPoint`、`thresholdEvent` 和 `fundamentalPhase`，并兼容保留已有 `anchor`；事件/相位模式额外保存 `confidence`，加载时校验有限浮点数、置信度范围和正周期。
- `tolerance` 支持绝对和相对阈值；`relativeFloor` 防止参考值接近零时相对误差失真。
- `enabled` 默认为 `false`。V1 工程迁移到 V2 时自动写入禁用的 Compare 配置，不会改变原有视图或数据源。

Compare 核心只在同一分段内做线性插值，跨越 Capture 或记录中的时间空洞时保留无效点，不会用插值伪造连续数据。误差结果包含有效/无效点、RMS、最大绝对/相对误差、连续超差区间和对齐置信度。

CLI 通过 `--alignment manual|anchor|trigger|threshold|phase` 暴露相同语义；报告输出包含应用/schema 版本、CRC32C 来源哈希、gap-aware 数据质量、规则结果和确定性 SVG 证据图。

## 兼容与迁移

- `scopeProjectType` 必须为 `scope-analyzer-project`。
- `schemaVersion: 1` 会在读取时显式迁移为 V2；未知版本会明确拒绝。
- V2 写回的工程始终使用 `schemaVersion: 2`。
- ID、引用、有限浮点数、布局、通道绑定、Compare 阈值和相对路径均在恢复前校验。

V1 文档格式说明仍保留在 [scopeproj-v1.md](scopeproj-v1.md)，用于追溯历史工程文件。
