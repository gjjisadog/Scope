# 多采样域 Snapshot 合约

Hybrid30K 的 FAST32K、CTRL8K 和 SLOW1K 是三个不同的执行域，不得被客户端重采样或合并为一张“同时”采样表。一个 SCP1 V2 stream 只属于一个 domain、一个固定采样率、一个固定 capture phase 和一个非零 consistency group。

DSP 必须在所属 phase 一次性冻结每个行的普通信号和 `SnapshotMeta`，然后发布整个行。客户端的职责是验证 descriptor 与行 metadata；它不会读取、缓存后拼接多个 CPU/CLA 变量来构造截面。

`row_sequence` 是本 stream 的单调逻辑行号。batch 内必须连续；batch 之间 gap 可报告，倒退与重叠是故障。`source_sequence` 表示计算采用的冻结输入，`applied_sequence` 表示已实际应用的命令/结果。它们表达因果而非跨 CPU 的物理同时性。

对于 STREAM_TABLE 中每个 `CausalRelation`，客户端只在同一 consistency group 内使用协议定义的逻辑控制周期校验：`result.source_sequence = input.logical_sequence + result_input_offset`，以及 `application.applied_sequence = result.logical_sequence + application_result_offset`。例如 `(0, 1)` 合法表示 input N → result(N) → application(N+1)。32 kHz、8 kHz 和 1 kHz 的本地 row number 不会被当成同一个物理时钟。

有效位必须清楚表明行是否有效、冻结、ADC 有效、source/applied 有效和（FAST32K）CLA 已完成。FAST32K 最低要求 valid/frozen/source/ADC/CLA；CTRL8K 最低要求 valid/frozen/source；SLOW1K 最低要求 valid/frozen。AppliedCommand role 或 CausalRelation 决定额外 source/applied 要求。无效行会保留在诊断计数中，绝不由客户端静默修复。

计时规则冻结为 `sample_period_ticks = tick_hz / fixed_sample_rate_hz` 且必须整除；不允许浮点近似。周期在一个 stream 或一个 Capture 内不得变化，时间戳必须随行序单调且 checked arithmetic 不溢出。
