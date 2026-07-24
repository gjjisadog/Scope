# SCP1 V2 DSP hardware Capture 状态机

Capture 的可见状态为 `Idle → Armed → Triggered → PostCapture → Complete → Uploading → Idle`。`ARM_CAPTURE` 只配置并布防 DSP 本地缓冲，`MANUAL_TRIGGER` 只能触发同一个已布防 id，`CANCEL_CAPTURE` 使其进入 `Cancelled`。

`Timeout`、`BufferOverrun`、`InvalidConfig` 和 `DeviceReset` 是失败终态，客户端不得把它们标为成功。冻结后 DSP 发送 `CAPTURE_BEGIN`，按从零开始的 `CAPTURE_DATA.block_index` 上传内嵌 V2 batch，最后发送 `CAPTURE_END` 的总块数、总样本数和完整性摘要。

客户端的 in-memory assembler 以 `capture_id` 和 `stream_id` 建立会话，检测丢块、重复、乱序和总量不一致。只有 `Complete` 且块/样本总数完全一致、无缺块的 Capture 才会标记为 complete。本阶段不修改 `.scope V1`。
