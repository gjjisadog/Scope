# Route A 验收矩阵

本文把 `docs/superpowers/specs/2026-07-14-route-a-design.md` 的交付条件映射到仓库证据。`通过` 表示当前工作区已有可重复的自动化证据；`待 Windows` 和 `待硬件` 表示入口和校验已实现，但必须在对应外部 runner/设备上执行后才能关闭。

当前结论：代码、协议、工程、CLI、规则和本地质量门已完成；路线 A 的最终发布状态仍待 Windows 原生验收、签名产物和实体 SCP1 证据。

| 范围 | 要求 | 当前证据 | 状态 |
| --- | --- | --- | --- |
| 阶段 0 | DOCX 游标真实值、P/Q/S 单位 | `src/word_export.rs`、`src/app.rs` 单元测试 | 通过 |
| 阶段 0 | VS Code 未信任工作区拒绝 workspace bridge | `vscode-extension/src/bridge-policy.test.js`（4/4） | 通过 |
| 阶段 0 | fmt/check/test/Clippy/性能/fuzz CI | `.github/workflows/ci.yml` | 通过 |
| 阶段 0 | Rust 1.96、WiX 3.14、Cargo.lock、offline 参数 | `rust-toolchain.toml`、`scripts/package-windows.ps1`、两个受控 Windows workflow 的预置工具链检查与 `CARGO_NET_OFFLINE=true` | 通过静态检查；待受控 runner 实跑 |
| 阶段 0 | provenance、SBOM、第三方许可证、运行时哈希 | `Write-BuildEvidence`/`Write-ReleaseEvidence`、`Assert-BuildProvenance`、签名模式的 clean-source/known-commit gate、`sourceDirty`、Cargo.lock/toolchain SHA256、`sbom.cdx.json`、`THIRD-PARTY-NOTICES.txt`、ANGLE/Mesa source hash manifest | 通过静态检查；待真实包产物 |
| 阶段 0 | SCP1 V1、`.scope` V1、`.scopeproj` V1 兼容 | `tests/fixtures/compatibility/`、协议/录波/工程测试 | 通过 |
| 阶段 0 | 外部输入 100 万样本 | protocol、project、bridge ignored fuzz gates | 通过 |
| 阶段 0 | Win10/11 安装、升级、卸载、WARP、Mesa、ANGLE、RDP | `scripts/windows-acceptance.ps1`（renderer startup log + 长驻 GUI smoke）、`.github/workflows/windows-release-acceptance.yml` | 待 Windows |
| 阶段 0 | 管理员生命周期与标准用户启动分离 | `-RequireStandardUser`、`-SkipMsiLifecycle`、标准用户 workflow checkout 管理员 run 的 `head_sha` 并校验 sourceCommit、双 evidence JSON | 待 Windows |
| 阶段 1 | Compare 五种对齐语义、置信度、重采样、gap | `src/compare/mod.rs`、`tests/fixtures/compare/` | 通过 |
| 阶段 1 | GUI/PNG/SVG/DOCX/JSON 共享证据 | `CompareEvidence`、导出测试、CLI report smoke、LibreOffice DOCX→PDF compatibility smoke、`render_docx.py` 页面检查 | 本地结构化/视觉/兼容转换通过；Windows Office 发布机复核待执行 |
| 阶段 1 | 工程 V1→V2 显式迁移 | `src/project.rs`、`docs/scopeproj-v2.md`、CLI migration smoke | 通过 |
| 阶段 2 | 确定性规则与 Invalid/evidence | `src/rules.rs` 测试 | 通过 |
| 阶段 2 | report provenance、哈希、数据质量、Compare、规则、SVG | `src/bin/scope_cli.rs` report smoke | 通过 |
| 阶段 2 | inspect/analyze/compare/test/report/validate-recording/project envelopes | `src/bin/scope_cli.rs`、CI CLI smoke、失败规则 fixture（退出码 5）、不完整录波 `valid:false`/退出码 5 语义 | 通过 |
| 阶段 2 | VS Code bridge capability negotiation | `src/vscode_bridge.rs`、扩展测试 | 通过 |
| 质量 | 核心库总体行覆盖率 ≥75% | `scripts/coverage-check.py` | 83.99%，通过 |
| 质量 | Compare/protocol/recording/project 各 ≥90% 行覆盖率 | `scripts/coverage-check.py` | 97.29/92.18/90.17/90.31%，通过 |
| 阶段 3 | 签名、MSI/ZIP SHA256、Windows acceptance evidence | `-RequireSignature` 覆盖主程序、CLI、Mesa helper；`release-evidence.json`、`Assert-BuildProvenance`、acceptance 脚本 | 待 Windows |
| 阶段 3 | 真实硬件采集 smoke | `scope-hardware-smoke`、`.github/workflows/windows-hardware-smoke.yml`、`scope-cli validate-recording` | 待硬件（需绑定设备 runner） |

## Windows 收口命令

在已预置 Rust 1.96、WiX 3.14、Mesa manifest、ANGLE DLL 和签名证书的 Windows 机器上执行：

```powershell
$env:SCOPE_PACKAGE_OFFLINE = "1"
$env:CARGO_NET_OFFLINE = "true"
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/release-check.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package-windows.ps1 -OfflinePackage -RequireSignature
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/windows-acceptance.ps1 `
  -MsiPath dist\ScopeAnalyzer-0.12.0-win-x64.msi `
  -ZipPath dist\ScopeAnalyzer-0.12.0-win-x64.zip `
  -ReleaseEvidencePath dist\release-evidence.json `
  -RequireSignature -RequireMesaRuntime -RequireAngleRuntime -RequireRdpSession `
  -OutputPath dist\windows-acceptance.json
```

另行部署但保留安装后，在标准用户会话中复核已部署程序；验收结束后由管理员执行卸载：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/windows-acceptance.ps1 `
  -MsiPath dist\ScopeAnalyzer-0.12.0-win-x64.msi `
  -ZipPath dist\ScopeAnalyzer-0.12.0-win-x64.zip `
  -ReleaseEvidencePath dist\release-evidence.json `
  -SkipMsiLifecycle -RequireStandardUser -RequireSignature -RequireMesaRuntime -RequireAngleRuntime -RequireRdpSession `
  -OutputPath dist\windows-acceptance-standard-user.json
```

```powershell
msiexec.exe /x dist\ScopeAnalyzer-0.12.0-win-x64.msi /qn /norestart
```

管理员 workflow 与标准用户 workflow 各自成功、并归档
`windows-acceptance.json` 和 `windows-acceptance-standard-user.json` 后，路线 A
才能从“代码完成”升级为“发布验收完成”。

发布验收命令必须保留 `-RequireSignature`、`-RequireMesaRuntime`、
`-RequireAngleRuntime` 和 `-RequireRdpSession`；不带这些开关的运行只属于
开发机 exploratory smoke，不能关闭路线 A 的发布门禁。

受控发布 runner 还必须在作业开始前预置 Rust/Cargo 1.96.0、Cargo registry/git
缓存、WiX 3.14、签名工具和 Mesa/ANGLE 输入；发布 workflow 会设置
`CARGO_NET_OFFLINE=true`，硬件 workflow 会用 `cargo build --locked --offline`。
这组前置条件是“断网可复现”的一部分，不能用临时在线下载替代。

硬件 smoke 必须在物理 SCP1 设备连接的 `scope-hardware` runner 执行；模拟器
测试只能证明协议/录波链路，不得替代实体设备证据。
