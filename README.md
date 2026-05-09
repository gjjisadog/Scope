# Scope Analyzer

Windows 离线波形分析软件第一版原型。界面按软件示波器方式组织：通道勾选、全选/取消、自由缩放、双竖直光标测量、选区 FFT 与谐波表。

完整使用说明已集成到软件顶部 `Help` 菜单中。

打开文件使用统一的 `Open CSV` 入口，软件会自动判断云端 `Content` CSV 或本地数值 CSV。

## 当前支持的数据格式

第一版支持两类 CSV。

### 云端 Content CSV

匹配现有 MATLAB 脚本里的云端录波格式：

- 文件第一行是 `Content`。
- 每行 `Content` 是一条十六进制报文。
- 每条报文解析成 2 个采样点。
- 每个采样点包含 30 个模拟量通道和 30 个数字/状态通道。
- 前 30 个模拟量按 little-endian `int16` 解析。
- 第 31/32 个 raw word 按 MATLAB 脚本的 bit 规则拆成数字/状态通道。

### 本地/标准数值 CSV

```csv
time,CH1,CH2,CH3
0.000000,0.0,0.1,0.2
0.000010,0.1,0.2,0.3
```

- 第一列必须是时间，单位秒。
- 后续列为通道值，最多读取 128 个通道。
- 文件打开时只建立分块索引和 min/max 摘要；绘图和 FFT 按当前视窗/选区读取。

后续如果还有新的二进制、加密或报文格式，应新增 `DataSource` 适配器，不需要改 UI 与 FFT 模块。

## 本地运行

```bash
cargo run --release
```

## 波形交互

- 鼠标滚轮：以鼠标位置为中心缩放纵轴幅值范围。
- `Ctrl` + 鼠标滚轮：以鼠标位置为中心缩放横轴时间范围。
- `Options`：可调整鼠标滚轮缩放敏感度。
- 左侧变量栏：可勾选显示通道、编辑变量显示名，并作为图例使用；图内图例已关闭。
- 波形高亮：鼠标悬停左侧变量时，对应波形会加粗高亮。
- 左键单击：把最近的光标移动到点击位置。
- 左键拖拽：框选时间区域并放大。
- 右键单击：弹出菜单，选择 `Place Cursor A` 或 `Place Cursor B` 后出现虚线预览光标。
- 放置光标：虚线光标跟随鼠标，左键单击确认放置，`Esc` 取消。
- 隐藏光标：右键菜单可 `Hide Cursor A/B` 或 `Show Cursor A/B`；隐藏只影响显示，光标位置和 A/B 测量仍保留。
- 右键拖拽：平移当前波形视图。
- `Fit Cursors`：缩放到 A/B 两个光标之间。
- FFT 面板：自动分析光标 A/B 之间当前 FFT 通道的波形，并显示基波、谐波、相位和 THD。
- 序分量：当前 FFT 通道属于 `stVg_0.iA/iB/iC`、`stIg_0.iA/iB/iC` 或 `stVinv_0.iA/iB/iC` 时，自动显示零序、正序、负序的幅值和相位。
- 相位说明：单通道 FFT 相位会随光标起点变化；三相序分量按 A-B-C 正序约定计算，重点看三相相对相位和正/负/零序幅值比例。

## Windows 便携版打包

在 Windows 机器安装 Rust 稳定版后执行：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/package-windows.ps1
```

产物在 `dist/ScopeAnalyzer-0.1.0-win-x64.zip`。
