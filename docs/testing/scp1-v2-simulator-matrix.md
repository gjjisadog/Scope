# SCP1 V2 确定性模拟器矩阵

所有 V2 preset 都要求 `scope_dsp_simulator --protocol v2 --preset <name>`，不依赖随机概率。V1 模式没有变化。

| preset | 客户端预期 |
| --- | --- |
| `30k-normal` | 三个 stream descriptor 可用；FAST32K 合法冻结行 |
| `30k-causal-delay` | `source=N, applied=N-1` 不报错 |
| `30k-cla-stale` | 无效 snapshot 行（缺 CLA_RESULT_VALID） |
| `30k-row-gap` | `row_sequence_gaps` 增加 |
| `30k-row-reorder` | row/timestamp 精确映射拒绝倒退行 |
| `30k-phase-mismatch` | SAMPLE_BATCH_V2 被拒绝 |
| `30k-group-mismatch` | SAMPLE_BATCH_V2 被拒绝 |
| `30k-unfrozen-row` | 无效 snapshot 行（缺 FROZEN_ROW） |
| `30k-capture-manual` | 手动 Capture 完整上传 |
| `30k-capture-edge` | Edge Capture 完整上传 |
| `30k-capture-fault` | FaultFlag Capture 完整上传 |
| `30k-capture-timeout` | Capture 失败，不标记 complete |
| `30k-capture-chunk-loss` | 检测缺块，Capture 失败；同一 V2 会话继续响应 PING |
| `30k-capture-chunk-reorder` | Capture 成功且两个非期望到达位置令 `capture_reordered_chunks == 2` |
| `30k-device-reset` | 精确报告 DeviceReset 并释放 Capture 缓存 |

0.15.2 的自动化端到端矩阵逐项执行“TCP simulator → LiveSession V2 → 帧解码 → Stream/Snapshot/Capture 校验 → 最终 SessionEvent”。断言不再只检查宽泛事件类别：每个 preset 分别固定 logical/source/applied 关系、精确诊断计数、拒绝原因、Capture id/块数/乱序计数和终态字段；chunk-loss 还在失败后以新 nonce 验证连接恢复。`live-inspect` 对快照诊断或协议拒绝返回非零退出码，`capture-inspect` 对 CaptureFailure 或失败状态返回非零退出码；正常预设返回 JSON `ok=true`。

这些是 TCP simulator/client 验证，绝不是 Hybrid30K DSP 硬件验证。
