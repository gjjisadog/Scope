# 多采样域 Snapshot 合约（SCP1 V2 R2）

Hybrid30K 的 FAST32K、CTRL8K、SLOW1K 是独立执行域。客户端不得把不同 CPU/CLA 时刻的变量拼成“物理同时”行。DSP 在每个固定 CapturePhase 一次性冻结信号和 SnapshotMeta，再发布整行。

## 两类序号

`row_sequence` 仅表示本 Stream 行号；`logical_cycle_sequence` 是 consistency group 内的统一因果时基。R2 group 1 的时基为 32 kHz，FAST/CTRL/SLOW 的 step 分别为 1/4/32，因此 row N、K、M 对应 cycle N、4K、32M。精确条件是 `logical_cycle_step = logical_cycle_rate_hz / sample_rate_hz`，必须整除。行间隔 D 对应 logical 增量 `D * step`。

`source_sequence` 表示 Result 实际采用的 Input cycle；`applied_sequence` 表示 Application 实际采用的 Result cycle。它们不是 row number，也不要求等于当前 Application logical cycle。

## 因果公式

同一 consistency group 内：

```text
expected_input = result.source_sequence - result_input_offset
expected_result = application.applied_sequence - application_result_offset

result.source_sequence = input.logical_cycle_sequence + result_input_offset
application.applied_sequence = result.logical_cycle_sequence + application_result_offset
```

所有有符号运算检查上下溢。Result/Application 可以先到并进入 pending。只有 `source_watermark > expected_source + max_reorder_cycles` 才确认缺失。不同 group 的缓存、水位和 deadline 完全隔离。

正常查找/插入使用按 `(stream, logical cycle)` 和 deadline 排序的 BTree 索引，接近 O(log n)。cached row 与 pending relation 各有 4,096 硬上限；超限返回 `CausalWindowOverflow`，不静默覆盖。匹配完成立即释放 pending；表 revision、session id 或 DeviceReset 改变时清空窗口。诊断分别记录 cached、pending、timeout、eviction、overflow 与 duplicate logical cycle。

## 行与时间戳

每个 Stream 的 `sample_period_ticks = tick_hz / sample_rate_hz` 必须精确整除。跨批次：

```text
current_timestamp = previous_last_timestamp
                  + (current_first_row - previous_last_row) * sample_period_ticks
```

周期改变、row overlap/reorder、timestamp drift 与 arithmetic overflow 都被拒绝。有效位缺失只增加明确诊断，不由客户端修补。

这是 Scope Analyzer 和 TCP 模拟器冻结的固件接口；真实 Hybrid30K DSP 尚未验证。
