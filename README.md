# Scope Analyzer

## Configuration Files

Configuration import/export is intentionally split so one file type cannot overwrite another:

- `Names` imports/exports only variable display names (`scope-names.json`).
- `Config > Display Settings` imports/exports colors, line widths, line styles, scale ratios, scope layout, sample-rate related display settings, language, theme, and analysis display settings (`scope-display.json`).
- `Config > Shortcut Settings` imports/exports only keyboard shortcuts (`scope-shortcuts.json`).
- `Config > Dataset Settings` imports/exports dataset group names, checked states, group line styles, time sync enablement, and time offsets (`scope-datasets.json`).

Windows 离线波形分析工具。界面按软件示波器方式组织，支持多 CSV 数据组叠加、通道勾选与分栏显示、双光标测量、选区 FFT/THD、三相正负序分析、变量名导入导出、浅色/深色主题和中英文界面。

0.8.1 修复实时界面的跨平台中文字体、带回差触发、串口默认值、异常重连，以及录波完成后的直接回放流程。

0.8.0 新增 DSP 实时软件示波器：TCP/串口 SCP1 采集、软件触发、有界实时缓冲、`.scope` 录波与离线回放，以及可独立运行的确定性 DSP 模拟器。原有离线分析、FFT、导出、配置和打包流程保持不变。

完整交互说明也集成在软件顶部 `Help` 菜单中。

## 快速开始

1. 使用顶部 `Import Data` 菜单选择一个或多个 CSV 文件。
2. 第一个文件作为主数据，后续文件作为附加数据组按相同通道序号叠加显示。
3. 软件读取第一行后自动识别云端 `Content` CSV 或本地数值 CSV。
4. 导入成功的 CSV 会加入 `Recent Files`，列表保存为程序目录下的 `scope-recent-files.json`。

顶部 `Names` 菜单只导入/导出变量显示名。导出的默认文件名是 `scope-names.json`，文件内容只包含 `display_names`；通道显示状态、颜色、线宽、倍率、FFT 设置、快捷键、语言和主题不会随变量名文件导入导出。

## DSP 实时示波器

无硬件时可先启动内置 TCP 模拟器：

```bash
cargo run --release --bin scope_dsp_simulator -- --listen 127.0.0.1:19090
```

再启动客户端，顶部切换到 `Live/实时`，保留默认地址 `127.0.0.1:19090`，点击 `Connect/连接`，选择采集通道后直接点击 `Configure & Start/应用并开始`。客户端会先等待设备确认实际采样率、批量和通道 mask，再自动进入 Streaming。也可用单独的 `Configure/应用采集` 按钮只下发参数。实时工作区可设置采样率、每帧点数、显示历史、采集/显示通道、颜色、显示倍率，以及 Auto/Normal/Single、上升/下降/双边沿软件触发。

进入 Streaming 后 `Record/录波` 才会启用，避免生成没有采样数据的空文件；它将经过协议校验的采样帧、gap 和完整触发配置写入 `.scope`。采集线程先写独立有界录波队列，再尝试更新画面，因此显示队列背压不会造成录波缺帧。点击 `Finish recording/结束录波` 后，可直接点 `Replay latest/回放刚才录波`，也可用 `Open recording/打开录波` 选择其他文件，进入离线工作区继续使用光标、测量、FFT、序分量和导出功能。显示暂停只冻结画面，不停止采集或录波；使用 `Stop/停止` 才会停止设备数据流。

Normal/Single 触发命中后，中央波形显示完整 pre/post capture；`Arm/重新布防` 清除上一次冻结 capture。Auto 模式未命中边沿时按 `Auto timeout` 样点数生成明确标记的超时 capture。右侧同时显示 CRC、协议错误、主机显示丢批、设备丢样/发送溢出，以及录波已写/排队记录数。

串口模式使用 8-N-1、无流控，切换到串口时默认 115200 baud；波特率仍可在实时工具栏中修改。DSP 固件需要实现 [SCP1 V1 协议](docs/protocols/scp1-live-scope-v1.md)。`LAUNCHXL-F28P65X` + XDS110 Application/User UART 已在 115200 baud、四通道、500 Hz、每批 10 点条件下完成连接、触发、3010 连续样本录波和离线回放实测。

### 实时连接排查

- `Connect` 失败：确认模拟器显示 `listening on 127.0.0.1:19090`，或串口未被其他程序占用。
- 一直停在 Handshaking：检查 DSP 是否按顺序返回非零 session_id 的 HELLO_ACK 与 CHANNEL_TABLE，并使用 SCP1 CRC32C。
- Configure 被拒绝：采样率不得超过设备 `tick_hz`，批量不得超过 `max_batch_samples`，至少选择一个且只能选择通道表内的通道。
- 3 秒后断开：客户端与设备都应每秒主动发送 PING 并回送同 nonce 的 PONG；CRC 错误帧不计作有效心跳。
- 波形有断点：查看 Host drops、Sequence gaps、Device drops 和 CRC 统计；客户端不会跨 gap 画连线。
- 录波意外中断：重新打开 `.scope` 会扫描并恢复最后一个完整记录；中间 CRC 损坏仍会拒绝，避免静默展示错误数据。
- 中文显示为方框或乱码：0.8.1 会自动查找 Windows、macOS 和常见 Linux CJK 字体；精简系统可用 `SCOPE_CJK_FONT` 指向本机 `.ttf/.otf/.ttc` 字体文件。

## 当前支持的数据格式

### 云端 Content CSV

匹配现有 MATLAB 脚本里的云端录波格式：

- 文件第一行是 `Content`。
- 每行 `Content` 是一条十六进制报文。
- 每条报文解析成 2 个采样点。
- 每个采样点包含 30 个模拟量通道和 30 个数字/状态通道。
- 前 30 个模拟量按 little-endian `int16` 解析。
- 第 31/32 个 raw word 按 MATLAB 脚本的 bit 规则拆成数字/状态通道。
- 云端文件没有直接时间列，软件使用 `Options` 中的 `FFT Fs` 生成秒级时间轴，默认 `1000 Hz`。

### 本地/标准数值 CSV

```csv
time,CH1,CH2,CH3
0.000000,0.0,0.1,0.2
0.000010,0.1,0.2,0.3
```

- 第一列必须是时间，单位秒。
- 后续列为通道值，最多读取 128 个通道。
- 文件打开时只建立分块索引和 min/max 摘要；绘图、测量和 FFT 按当前视窗或光标选区读取。
- 大文件不会一次性全部载入内存；缩小视图使用 min/max 包络，放大后读取原始采样点。
- 绘图使用总点数预算，通道越多每通道绘图点越少；缩放和平移只替换当前窗口缓存，不累积历史窗口数据。

### 本地 ADATA/DDATA CSV

适配本地导出的 `ADATA`/`DDATA` 成对 CSV：

- 文件名包含 `ADATA` 和 `DDATA` 时会自动识别，可只选择单个文件并自动查找配对文件，也可批量选择多个文件自动配对。
- 配对优先使用文件名中的时间戳，其次使用名称关系和文件修改时间。
- `ADATA` 作为模拟量通道，`DDATA` 作为数字量通道。
- 两类文件的第一列序号会自动剔除。
- `DDATA` 去掉序号列后的前三个 bit 通道会合并成一个数字量通道：`bit0 + 2*bit1 + 4*bit2`。
- 该读取规则只作用于这种本地 `ADATA`/`DDATA` 格式，不影响云端 `Content` CSV、标准数值 CSV 或 DAT 文件。
- 本地数据文件的变量命名尽量与云端 CSV 保持一致。

后续如果还有新的二进制、加密或报文格式，应新增 `DataSource` 适配器，不需要改 UI 与 FFT 模块。

## 主要功能

### 数据组与布局

- `Import Data` 可一次选择多个波形文件，第一组为主数据，附加数据组以虚线叠加。
- 菜单中可勾选一个或多个数据组后删除。
- 左侧变量栏按数据组、模拟量/数字量和变量名组织。
- 右键数据组可全选/全不选该数据组变量，也可配置该组线型。
- 顶部 `Layout` 可设置示波器行数和列数；点击某个子窗口后再勾选变量，会把变量放入当前子窗口。
- 所有子窗口共享时间轴和光标；多窗口下可分别缩放各窗口纵轴。

### 变量与显示

- 双击变量名可编辑显示名。
- 变量颜色、线型、线宽和倍率系数可在变量行或颜色设置中调整。
- 搜索支持多关键词，并匹配显示名和原始名。
- 鼠标悬停左侧变量时，对应波形会加粗高亮。
- 附加数据组按相同通道序号叠加，使用同一通道颜色、线宽和倍率，并可用数据组线型区分。
- 左右侧栏都可拖动调整宽度。窗口变窄时，变量名会自动缩短或隐藏，右侧分析面板会按可用空间自动横向或纵向排列。
- 数字量变量名在空间不足时优先显示 `.` 后半截；空间充足时显示全名。

### 变量名文件

- `Names > Export Names` 只保存当前变量显示名。
- `Names > Import Names` 只恢复变量显示名。
- 导入或导出成功的文件会显示在 `Names > Recent Names`，可清空列表。
- 变量名文件不会覆盖通道可见性、颜色、线宽、倍率、FFT 设置、快捷键、语言或主题。
- `Config` 中的变量名、显示、快捷键、数据组配置各自导入导出、各自维护最近文件。
- 配置文件带有类型标识；选错配置类型时会拒绝导入，避免不同配置互相覆盖。

### 波形图片与 DOCX 导出

- `Export > Export Waveform PNG` 会先打开导出标注台，不会立即保存。
- 标注台提供选择、文字、箭头、矩形/椭圆、画笔、橡皮、撤销和恢复工具。
- 变量名标注会自动生成，箭头默认吸附对应曲线；拖动变量名时箭头会自动变长、缩短和旋转，拖动锚点可改变箭头指向同一曲线的位置。
- 手动箭头和文字用于标注故障点、现象和说明，不绑定变量曲线。
- 可保存 PNG、SVG，也可导出 DOCX。
- 批量导出可按多个时间窗口、数据组和子窗口生成多张 PNG，或把多张已标注波形图写入同一个 DOCX。
- DOCX 使用内置简洁模板，光标数据表可选择包含或隐藏；第一版不导入外部模板。

### 光标与缩放

- 鼠标滚轮：以鼠标位置为中心缩放纵轴幅值范围。
- `Ctrl` + 鼠标滚轮/触控板滚动：以鼠标位置为中心缩放横轴时间范围。
- 左键单击波形：把最近的光标移动到点击位置。
- 左键拖拽波形：框选时间区域并放大。
- 右键单击波形：打开光标菜单。
- `Place Cursor X1/X2`：显示虚线预览光标，左键确认，`Esc` 取消。
- `Hide/Show Cursor X1/X2`：只切换显示状态，不改变光标位置和测量结果。
- 右键拖拽波形：平移当前波形视图。
- `Fit Cursors`：缩放到 X1/X2 两个光标之间。

### 测量、FFT 与序分量

- 右侧 `Analysis` 统一选择数据组、分析通道和三相 A/B/C 通道；测量、FFT 和正负序共用该入口。
- `Measurements` 对 X1/X2 区间内已选通道显示 `Y1`、`Y2`、`dY`、最大值和最小值，结果使用通道倍率后的值。
- `FFT` 可选择数据组和模拟量通道，分析 X1/X2 之间的选区。
- FFT/谐波计算会去除直流均值，使用 Hann 窗，并按目标谐波频率做相量投影和窗增益补偿。
- 谐波表显示 0 次直流量以及 1-10 次谐波的幅值、相位、相对基波比例和 THD。
- `THD = 2 次及以上谐波平方和开根号 / 1 次谐波幅值`。
- `Sequence` 正负序分析需要三个模拟量通道，按 A-B-C 正序约定显示零序、正序、负序的幅值、相位和相对正序比例。
- `PLL / dq0` 使用 `Options` 中的锁相环同步源，可在电压 `stVg_0.iA/iB/iC` 和电流 `stIg_0.iA/iB/iC` 间切换；右侧 `PLL / dq0` 下方可勾选 `PLL theta (deg)`、`dq0.d`、`dq0.q`、`dq0.0` 四条派生曲线。
- 单通道相位会随光标起点变化；三相序分量更适合看相对相位和正/负/零序幅值比例。

### Options

- `FFT Fs`：云端 `Content` CSV 的时间轴采样率，也是 FFT 分析使用的采样率，默认 `1000 Hz`。
- `Harmonic Base`：谐波基准频率，默认 `50 Hz`。
- 可设置滚轮缩放敏感度。
- 可切换 `中文` / `English` 和浅色 / 深色主题。
- 可配置快捷键。
- `Align dataset time axes` 可按谐波基准频率估计相位差，并以主数据为基准平移附加数据组时间轴。

### 默认快捷键

- `R`：复位视图。
- `F`：适配光标。
- `H`：隐藏/显示 X1/X2 光标。
- `Ctrl+A`：全选通道。
- `Ctrl+D`：取消全选。
- `Ctrl+B`：隐藏/显示左侧变量栏。
- `Ctrl+Alt+B`：隐藏/显示右侧分析栏。

侧栏快捷键采用类似 VS Code 的默认习惯，并且在输入框获得焦点时仍可生效。

## 本地运行

```bash
cargo run --release
```

## Windows 打包

在 Windows 机器安装 Rust 稳定版和 WiX Toolset v3 后执行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/release-check.ps1
$env:SCOPE_PACKAGE_OFFLINE=1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package-windows.ps1 -OfflinePackage
```

Release machines should preload Mesa in `target/mesa-runtime/x64` with
`mesa-runtime-manifest.json` or provide `MESA_RUNTIME_DIR`/`third_party/mesa`.
Bundle ANGLE from `ANGLE_RUNTIME_DIR`, `third_party/angle`, or
`target/angle-runtime`; `SCOPE_ALLOW_SYSTEM_ANGLE=1` is only for local packaging
experiments.

产物在：

- `dist/ScopeAnalyzer-0.8.1-win-x64.zip`
- `dist/ScopeAnalyzer-0.8.1-win-x64.msi`

## 启动渲染器和云桌面兜底

默认启动器会依次尝试多个渲染器，并把详细过程写入程序目录下的
`ScopeAnalyzer-startup.log`；如果安装目录不可写，则写入系统临时目录：

1. `glow-software` / OpenGL via ANGLE, hardware acceleration off, intended for
   cloud desktops and virtual display adapters.
2. `glow` / OpenGL via ANGLE, hardware preferred.
3. `wgpu-software` / DX12 fallback adapter, intended for WARP/software rendering.
4. `wgpu` / DX12.
5. `mesa` / Mesa llvmpipe software OpenGL, isolated in the packaged `mesa`
   directory as the last cloud-desktop fallback.

Packaged builds bundle ANGLE only from explicit reproducible sources:
`ANGLE_RUNTIME_DIR`, `third_party/angle`, or `target/angle-runtime`. When ANGLE
is bundled, packaging writes `angle-runtime-manifest.json` with the copied DLL
hashes. Set `SCOPE_ALLOW_SYSTEM_ANGLE=1` only for local experiments that should
probe the build machine's Edge/WebView runtime. If every renderer exits during
startup, the launcher tries the isolated Mesa helper before stopping. Mesa
runtime files are resolved
from `MESA_RUNTIME_DIR`, `third_party/mesa`, or the verified
`target/mesa-runtime/x64` cache. If no local runtime is available, packaging
downloads the pinned `pal1000/mesa-dist-win` `26.0.8`
`mesa3d-26.0.8-release-msvc.7z` asset and verifies SHA256 before extraction.
The automatic cache writes `mesa-runtime-manifest.json`; CI/release machines can
preload this cache and set `SCOPE_PACKAGE_OFFLINE=1` or pass `-OfflinePackage`
to fail instead of downloading. Set `SCOPE_SKIP_MESA_DOWNLOAD=1` to package
without Mesa.

Packaged builds include two startup entries:

- `Start-ScopeAnalyzer.bat`: automatic fallback.
- `Start-ScopeAnalyzer-Mesa.bat`: force the isolated Mesa llvmpipe helper.

云桌面建议优先使用默认启动器。若虚拟显卡仍无法启动，再使用
`Start-ScopeAnalyzer-Mesa.bat` 强制 Mesa/llvmpipe 兜底。Mesa 显示效果通常一致，
但 CPU 占用和拖拽缩放流畅度可能下降。

软件顶部 `Help` 菜单提供：

- `复制诊断信息`：复制版本、渲染器环境、日志路径、当前数据和最近错误。
- `打开日志目录`：打开启动日志和崩溃日志所在目录。
