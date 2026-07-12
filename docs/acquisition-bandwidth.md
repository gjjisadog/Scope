# SCP1 采集带宽助手

带宽助手依据实际 SCP1 V1 SampleBatch 编码计算，不使用“通道数 × 4”之类的近似值。

设通道数为 `C`，每个通道线格式字节宽度之和为 `W`，每批样点数为 `B`：

- 样点数据：`B × W` 字节。
- SampleBatch payload：`20 + 2×C + B×W` 字节。
- 完整 SCP1 frame：payload 再加 28 字节帧头和 4 字节 CRC。
- 帧率：`sample_rate / B`。
- 批次延迟：`B / sample_rate` 秒。

串口按 8-N-1 计算，即每字节占 10 bit：

`utilization = frame_bytes × frames_per_second × 10 / baud`

策略阈值：

- Safe：不超过 70%。
- Warning：大于 70%，不超过 90%。
- Critical：超过 90%，或单帧 payload 超过协商上限。

Critical 串口配置默认不能开始。安全批次建议会搜索满足 payload 上限和 70% 阈值的最小批次，用户需要显式采用。专家覆盖只对下一次开始操作有效，并记录在事件状态中；它不会绕过设备协商、协议大小或通道合法性检查。

TCP 在未提供预期链路容量时显示流量和延迟，但严重度为 Advisory，不假定某个固定网速。
