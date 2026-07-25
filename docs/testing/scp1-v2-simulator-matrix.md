# SCP1 V2 冻结模拟器矩阵

入口：`scope_dsp_simulator --protocol v2-r1|v2-r2 --streams fast32k,ctrl8k,slow1k --preset <name>`。`v2` 是 R2 别名，V1 默认不变。所有结果都是 TCP 软件模拟证据，不是 Hybrid30K 硬件验证。

## Revision 与多 Stream

- R1 client + R1 simulator：0x30/31/32/45、28-byte metadata。
- R2 client + R2 simulator：0x35/34/33/47、compressed metadata。
- R1/R2 混用和 0.15.2 的 36-byte/0x32 原型必须明确拒绝。
- R2 一次配置 FAST32K 32 行、CTRL8K 8 行、SLOW1K 1 行；START/STOP 作用于整个原子集合，三者独立维护 row/logical/timestamp/背压诊断。

## 三 Stream Causal presets

| preset | 精确期望 |
| --- | --- |
| `30k-causal-in-order` | Input→Result→Application，offset `(0,32)`，无 mismatch/timeout |
| `30k-causal-result-first` | Result pending，Input 在 64-cycle 窗口内消解 |
| `30k-causal-application-first` | Application 与 Result pending，Input 最后到仍消解 |
| `30k-causal-source-timeout` | Input watermark 越过 deadline，timeout/missing 增加 |
| `30k-causal-nonzero-offset` | result offset `+1` 正确匹配 |
| `30k-causal-negative-offset` | result offset `-1` 正确匹配 |
| `30k-causal-duplicate-cycle` | duplicate logical cycle 明确计数 |
| `30k-causal-watermark-eviction` | source 跳过窗口后确定性 timeout，缓存保持有界 |

旧的 15 个 R1 presets 保留显式兼容入口，覆盖正常、CLA stale、row gap/reorder、phase/group mismatch、unfrozen row、manual/edge/fault Capture、timeout、chunk loss/reorder 与 DeviceReset。它们不冒充 R2 multi-stream 证据。

## Capture、Reset、Heartbeat、带宽

- R2 Capture 正常上传；乱序块的增量 CRC 等于一次性定义。
- 首次 chunk loss 产生 CaptureFailure、缓存归零、连接保持；第二次 ARM 成功。
- DeviceReset 清空订阅/Capture/causal/timing/heartbeat/pending/table，拒绝旧 session frame，再以新 id 完成 HELLO_ACK→ChannelTable→StreamTableR2→Ready；不自动恢复 streaming。
- heartbeat 覆盖顺序/乱序、1–3 秒延迟、3 秒 timeout、unknown、duplicate、overflow、RTT/max RTT 与 reset clear。
- 16×I16×8 kHz×128 在 4 Mbaud 下 affine 与 sparse override ≤70%；Explicit >70%；8×I16×32 kHz >70%。

CI 在 Ubuntu/Windows 跑 format/check/Clippy/all-target/live/simulator，并在独立 Ubuntu job 显式执行 ignored 的 100 万行 bounded causal test。GitHub 上尚未发生的 workflow run 不得写成已通过；真实 UART、DSP Capture 和 Windows 安装验收仍由相应受控 runner 提供证据。
