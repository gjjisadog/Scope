# SCP1 V2 R2 DSP hardware Capture 状态机

可见流程为 `Idle → Armed → Triggered → PostCapture → Complete → Uploading → Idle`。`Cancelled`、`Timeout`、`BufferOverrun`、`InvalidConfig`、`DeviceReset` 是终态/异常，不能标记成功。

`CAPTURE_BEGIN` 绑定 id、Stream、预计行数和 trigger row。R2 `CAPTURE_DATA` 使用消息 `0x47`，内嵌压缩或 Explicit 的 R2 batch。插入块时先用纯长度函数检查 payload 和 64 MiB 上限，只在块索引相邻时比较 BTreeMap 前驱/后继的行邻接；实时会话把已验证的原始 wire payload 作为 `Arc<[u8]>` 交给 assembler，无需再次编码。

CRC32C 从 `le_u32(capture_id)` 开始。乱序块进入 map；当从 `next_crc_block` 开始形成连续前缀时，逐块增量更新 CRC 并立即释放对应 wire-payload Arc。不会拼接完整 Capture Vec。push 为 O(log n)，CRC 工作仅与新变为连续的块大小有关；finish 按序移动 batch，不复制全部 payload。

成功要求：状态 Complete、块号从零连续、行跨块连续、trigger row 在范围内、`uploaded_rows == total_samples == BEGIN.row_count`、`dropped_rows == 0`、块数一致且增量 CRC 匹配。上限为 1,048,576 行、4,096 块、每块 4,096 行、64 MiB。

finish（成功或失败）以及 terminal CaptureStatus 都释放 begin、blocks、rows、bytes 和 CRC 状态。CaptureFailure 不关闭 V2 协议连接；心跳继续，下一次 ARM 可成功。DeviceReset 进一步清空会话并要求新 session id。Capture 仍只在内存交付，不写 `.scope V2`，也不修改 `.scope V1`。
