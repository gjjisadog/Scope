# Hybrid30K SCP1 V2 R2 第一轮 DSP Bring-up

本文面向 Hybrid30K DSP 固件开发者，定义第一轮真板联调所需的最小 wire contract 和验收顺序。Scope 0.15.5 没有修改 SCP1 V2 R2 语义，也不规定 DSP 的 RAM、DMA、GPIO 或 UART/SCI 实现；这些资源由 Hybrid30K 工程自行分配。

## 1. 第一轮 DSP 最小实现清单

DSP 端本轮只需完成：SCP1 V2 framing 与 CRC32C、HELLO/HELLO_ACK、CHANNEL_TABLE、STREAM_TABLE_R2、PING/PONG、一个原子 `CONFIGURE_STREAMS_R2` transaction、START/STOP、CTRL8K sample batch，以及可选的 SLOW1K sample batch。STOP 成功后必须保持同一 session 的 Ready 状态。

本轮不要求 FAST32K 连续 streaming、Capture、DeviceReset、自动重连、DMA 串口、长时间录波或新的协议 revision。TCP simulator 结果不能作为真板证据。

## 2. R2 capability 要求

HELLO_ACK 必须返回非零 `session_id`，并同时声明：

- `CAPABILITY_V2_STREAMS_R2`
- `CAPABILITY_V2_MULTI_STREAM`
- `CAPABILITY_V2_COMPRESSED_METADATA`

`CAPABILITY_V2_HARDWARE_CAPTURE_R2` 不是本轮 smoke 的通过条件。设备报告的 CRC error、protocol error、drop 和 TX overrun 必须保持为零。

## 3. 握手顺序

严格顺序为：`HELLO → HELLO_ACK → CHANNEL_TABLE → STREAM_TABLE_R2 → PING/PONG → Ready`。Profile 校验在三个表完成后执行；任何 capability、stream 或 channel 契约不匹配都会以稳定的 `profile_*` 错误码退出。

## 4. CTRL8K 最小 Stream

CTRL8K 属于 consistency group 1，`sample_rate_hz = 8000`，`logical_cycle_step = 4`。第一轮 required channels 为 `Ia`、`Ib`、`Ic`、`Vdc`、`SampleValid`；`P`、`Q`、`Freq`、`PllAngle`、`Va`、`Vb`、`Vc` 为 optional，固件不必一次全部实现。

缩放必须与现有 Hybrid30K ABI 一致：电压 `0.1 V/LSB`，电流 `0.01 A/LSB`，频率 `0.01 Hz/LSB`，有功 `1 W/LSB`，无功 `1 var/LSB`，角度为 Q16 turns。`SampleValid` 使用 `U8`、scale 1。

## 5. SLOW1K 最小 Stream

SLOW1K 属于 consistency group 1，`sample_rate_hz = 1000`，`logical_cycle_step = 32`。首批 optional channels 为 `RunState`、`FaultFlags`、`CtrlEnabled`、`CommandAckSeq`、`ParamAppliedVersion`；前期允许仅实现其中一部分。multistream smoke 需要存在一个可订阅的 SLOW1K channel。

## 6. 32 kHz logical cycle

consistency group 1 的 `logical_cycle_rate_hz` 固定为 32000。FAST32K、CTRL8K、SLOW1K 的 step 分别为 1、4、32。每个 stream 独立维护连续的 row sequence 和 timestamp；同一原子配置中的批大小应覆盖相同 logical-cycle 窗口，例如 CTRL8K 8 行对应 SLOW1K 1 行。

每行 `logical_cycle_sequence` 必须按 stream step 推进，frame timestamp 必须与首行 row sequence 和 `sample_period_ticks` 对应。无效 snapshot、row reorder、host/device drop 或 TX overrun 都会使 smoke 失败。

## 7. Golden Vectors

静态 fixture 位于 `tests/fixtures/scp1-v2-r2/`：

- `hello_ack.bin`
- `channel_table.bin`
- `stream_table_r2.bin`
- `configure_streams_r2.bin`
- `sample_batch_ctrl8k_affine.bin`
- `sample_batch_slow1k_affine.bin`
- `ping.bin`
- `pong.bin`
- `expected.json`

这些 `.bin` 由 Scope 0.15.4 正式 encoder 一次性生成并冻结。Hybrid30K C 测试应逐字节比较完整 frame，并核对 `expected.json` 中的 message type、payload/frame length、CRC32C 和关键字段。

## 8. Machine Profile

协议契约位于 `profiles/hybrid30k-r2.json`。发布的 `scope-hardware-smoke.exe` 已将该 JSON 编译期嵌入，`--profile hybrid30k` 与 `--profile hybrid30k-r2` 不访问运行机器的源码目录；只有传入自定义 JSON 路径时才读取文件系统。Profile 只包含 capability、causal group、stream、channel、wire format、scale 和 unit，不包含源文件、变量地址、共享 RAM、引脚或外设实例信息。

Profile 校验错误会区分：`profile_missing_stream`、`profile_stream_rate_mismatch`、`profile_logical_step_mismatch`、`profile_missing_channel`、`profile_channel_format_mismatch`、`profile_channel_scale_mismatch`、`profile_channel_unit_mismatch` 和 `profile_capability_mismatch`。

## 9. Hardware Smoke 与带宽门禁

CTRL8K 默认 `--channel-set required`，只订阅 `Ia/Ib/Ic/Vdc/SampleValid`；`--channel-set all` 才会加入设备实际存在的 optional channels。multistream 默认使用 CTRL8K required 加 SLOW1K 第一个实际存在的 optional channel，不会自动打开全部 SLOW1K optional channels。

Serial 在 CONFIGURE 确认后、START 前计算理论链路占用。估算包含 SCP1 frame header、CRC、R2 fixed sample header、channel-id table、实际 wire format、affine metadata、batch 频率，以及 8N1 每字节 10 bit；超过 70% 返回 `link_budget_exceeded`，错误包含 `required_baud`、`configured_baud` 和 `estimated_utilization`。TCP 也输出估算，但不应用串口拒绝门禁。

阶段 A（115200）仅做 handshake、PING/PONG、ChannelTable、StreamTableR2 和 CONFIGURE 命令验证，不推荐 START 标准 CTRL8K。若显式运行数据模式，仍会先经过预算门禁：

```powershell
scope-hardware-smoke --protocol v2-r2 --profile hybrid30k --serial-port COM7 --baud 115200 --mode handshake --duration-ms 3000 --output evidence\handshake.json
```

阶段 B（921600）继续控制面与低带宽自定义流试验；不得宣称标准 Hybrid30K CTRL8K required 一定可运行。阶段 C（2M）首次尝试 CTRL8K required-only。阶段 D（4M）以 CTRL8K required、CTRL8K+SLOW1K minimal 和 link-stress 为目标：

```powershell
scope-hardware-smoke --protocol v2-r2 --profile hybrid30k --serial-port COM7 --baud 2000000 --mode ctrl8k --channel-set required --duration-ms 3000 --output evidence\ctrl8k-2m.json
scope-hardware-smoke --protocol v2-r2 --profile hybrid30k --serial-port COM7 --baud 4000000 --mode multistream --channel-set required --duration-ms 3000 --output evidence\multistream-4m.json
scope-hardware-smoke --protocol v2-r2 --profile hybrid30k --serial-port COM7 --baud 4000000 --mode link-stress --channel-set required --duration-ms 10000 --output evidence\link-stress-4m.json
```

`link-stress` 要求至少 5 秒，真板推荐至少 10 秒。每个订阅 Stream 必须同时有 batch 和 row；否则返回 `stream_no_data`。link-stress 还按 Stream 检查最低 95% 吞吐率，并允许 START/STOP 边界少一个 batch；否则返回 `stream_throughput_below_minimum`。

也可用 `--tcp host:port` 做 software smoke，但其 JSON 不能归档为 hardware PASS。成功 JSON 包含 `channel_set`、`link_budget`，以及每个 Stream 的选择 mask、expected/received rows、throughput ratio 和 drop/sequence 诊断。成功时 stdout 为 `ok=true` 且退出码为 0；失败时 stdout 仍为结构化 `ok=false` JSON，并携带稳定 `error.code`。

## 10. 推荐波特率升级顺序

真板按冻结策略逐级执行：阶段 A `115200` 只做 handshake/control-plane；阶段 B `921600` 做控制面和低带宽试验；阶段 C `2M` 首次尝试 CTRL8K required-only；阶段 D `4M` 才以 CTRL8K + SLOW1K + stress 为目标。所有阶段均执行理论 preflight，任何波特率都不能绕过 70% 门限。

这些波特率是测试阶段，不是“必然支持”声明。真实支持必须同时满足理论 preflight 与真板 hardware smoke；当前仓库尚未进行真实 Hybrid30K DSP、串口电气链路、长时间稳定性或高波特率验收。
