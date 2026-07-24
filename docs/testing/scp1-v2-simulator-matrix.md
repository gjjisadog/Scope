# SCP1 V2 确定性模拟器矩阵

所有 V2 preset 都要求 `scope_dsp_simulator --protocol v2 --preset <name>`，不依赖随机概率。V1 模式没有变化。

| preset | 客户端预期 |
| --- | --- |
| `30k-normal` | 三个 stream descriptor 可用；FAST32K 合法冻结行 |
| `30k-causal-delay` | `source=N, applied=N-1` 不报错 |
| `30k-cla-stale` | 无效 snapshot 行（缺 CLA_RESULT_VALID） |
| `30k-row-gap` | `row_sequence_gaps` 增加 |
| `30k-row-reorder` | `row_sequence_reorders` 增加 |
| `30k-phase-mismatch` | SAMPLE_BATCH_V2 被拒绝 |
| `30k-group-mismatch` | SAMPLE_BATCH_V2 被拒绝 |
| `30k-unfrozen-row` | 无效 snapshot 行（缺 FROZEN_ROW） |
| `30k-capture-manual` | 手动 Capture 完整上传 |
| `30k-capture-edge` | Edge Capture 完整上传 |
| `30k-capture-fault` | FaultFlag Capture 完整上传 |
| `30k-capture-timeout` | Capture 失败，不标记 complete |
| `30k-capture-chunk-loss` | 检测缺块，Capture 失败 |
| `30k-capture-chunk-reorder` | 检测乱序块 |
| `30k-device-reset` | 检测 DeviceReset，Capture 失败 |

0.15.1 的自动化端到端矩阵逐项执行“TCP simulator → LiveSession V2 → 帧解码 → Stream/Snapshot/Capture 校验 → 最终 SessionEvent”；所有 15 项通过。`live-inspect` 对快照诊断或协议拒绝返回非零退出码，`capture-inspect` 对 CaptureFailure 或失败状态返回非零退出码；正常预设返回 JSON `ok=true`。

这些是 TCP simulator/client 验证，绝不是 Hybrid30K DSP 硬件验证。
