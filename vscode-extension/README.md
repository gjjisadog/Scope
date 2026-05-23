# Scope Analyzer VS Code Extension / VS Code 扩展版

这个扩展版用于在 VS Code Webview 中打开现有桌面版支持的波形数据文件：

- 标准数值 CSV：第一列是秒级时间，后续列是通道数据。
- 元信息 CSV：前置 `file_path`、`dt`、`Number_of`、`t0` 等元信息，`END` 后一行为通道名，后续行为采样数据。
- 云端 `Content` CSV：包含名为 `Content` 的十六进制录波报文字段。
- 二进制 DAT：读取文件头中的采样率和通道名，按 little-endian `int16` 解析采样帧。

## 本地运行

1. 用 VS Code 打开 `vscode-extension` 目录。
2. 按 `F5` 启动 Extension Development Host。
3. 在命令面板运行 `Scope Analyzer: Open Waveform Data`。

当前 Webview 支持通道搜索/勾选、Canvas 波形绘制、X1/X2 游标、区间测量、FFT 谐波幅值/相位、THD、云端 CSV 采样率重载和谐波基频配置。

无需构建步骤；扩展直接运行 `src/extension.js` 和 `media` 中的静态文件。
