# SCP1 V2 DSP hardware Capture 状态机

Capture 的可见状态为 `Idle → Armed → Triggered → PostCapture → Complete → Uploading → Idle`。`ARM_CAPTURE` 只配置并布防 DSP 本地缓冲，`MANUAL_TRIGGER` 只能触发同一个已布防 id，`CANCEL_CAPTURE` 使其进入 `Cancelled`。

`Timeout`、`BufferOverrun`、`InvalidConfig` 和 `DeviceReset` 是失败终态，客户端不得把它们标为成功。冻结后 DSP 发送 `CAPTURE_BEGIN`，按从零开始的 `CAPTURE_DATA.block_index` 上传内嵌 V2 batch，最后发送 `CAPTURE_END` 的总块数、总样本数和完整性摘要。

客户端的 in-memory assembler 以 `capture_id` 和 `stream_id` 建立会话。每个块在缓存前都必须匹配 stream revision、domain、phase、group、顺序 channel ids 和固定 sample period；累计行数/块数/编码 payload 字节数受 `1,048,576` 行、`4,096` 块、每块 `4,096` 行和 `64 MiB` 上限约束。

块按 index 排序后必须没有缺失、重叠、倒退或行号不连续，trigger row 必须落在首末行内。成功结束还要求 `uploaded_rows == total_samples == 实收行数 == BEGIN.row_count`、`dropped_rows == 0` 和 `total_blocks == 实收块数`。`integrity_summary` 固定为 `CRC32C(le_u32(capture_id) + 按 block_index 排序的 CAPTURE_DATA 编码 payload)`，客户端重算并比较。只有这些条件和 `Complete` 同时成立才会标记 complete；本阶段不修改 `.scope V1`。
