# Repid Interaction Extraction

Static extraction from `C:\Users\11981\Downloads\e378cb4e4937504d49eae1ba0826010d_1597339840547397181_m_unpacked\4-repidv6`.

This report records observable interaction logic only. The program was not executed. Evidence comes from `repid.exe` strings, bundled resources, `xIAOAO.wavx`, and DOCX template placeholders.

## 1. Menu And Workspace Structure

Confidence: medium. The exact visual menu hierarchy needs sandbox confirmation, but the executable keeps Qt/Qtitan Ribbon action names, resource paths, and class names.

Observed shell:

- Ribbon-style main window: `RibbonMainWindow`, `RibbonBar`, `RibbonPage`, `RibbonGroup`, `RibbonSystemMenu`, `addPage`, `addGroup`, `addAction`.
- MDI workspace: `QMdiArea`, `QMdiSubWindow`, `New Window`, `Switch Windows`, `Normal`, `fullscreen.png`, `newwindow.png`, `windowswitch.png`.
- Dock/sidebar controls: `left-sidebar.svg`, `right-sidebar.svg`, `ribbonMinimize.png`, `ribbonMaximize.png`.
- File/system actions: `new.png`, `open.png`, `save.png`, `folder.svg`, `Print Preview`, recent file support via `addPageRecentFile`.
- Tool actions: `build2.svg`, `run2.svg`, `running.svg`, `stop.svg`, `play.svg`, `clear.svg`, `del.svg`, `refresh.svg`, `search.svg`, `setting.svg`, `help.svg`.
- Editing/annotation-like tools: `sketch.png`, `textedit.png`.
- Validation status icons: `valid-ok.svg`, `valid-fail.svg`, `no-valid.svg`, `RedExclamation.svg`.

Likely top-level areas:

- Project/File: new/open/save/recent/print preview.
- Wave import/workspace: wave files, file groups, channel tree, curve tree.
- View/window: MDI new window, switch windows, fullscreen/normal, sidebars.
- Script/console: embedded Python console and scripts.
- Build/run/validate: BPA/RTDS style run, model validation workflows.
- Report/export: DOCX report generation and graph/image export.

## 2. File Import And Export Flow

Evidence strings:

- `WaveFileWidget::addFiles(QStringList)`
- `WaveFileWidget::addFileFinishedSlot()`
- `WaveFileImportWidget`
- `Batch wave files import finished in`
- `Current Batch Wave Files Import Not Finished.`
- `You can not import curves when wave files are importing`
- `file is already imported.`
- `already be imported into this project.`
- `You are not allowed to import files to virtual files.`
- `%1 file not exists`
- `%1 unkown wave file format`
- `file format is not supported.`
- `Unsupported wave file format`

Import filters and parsers:

- Project: `Repid Project(*.wavx);;`
- Wave filter A: `Wave Files(*.cfg *.out *.csv *.cur *.inf);;`
- Wave filter B: `Wave Files(*.cfg *.out *.csv *.cur *.mpb *.inf *.mat *wavx);;`
- Direct extension evidence: `.cprj`, `.wavx`, `.wavdat`, `.cfg`, `.out`, `.cur`, `.inf`, `.mpb`, `.mat`, `.csv`, `.tsv`, `.xlsx`, `.docx`.
- Parser classes: `Comtrade`, `CSVFile`, `TSVFile`, `MatFile`, `RTDSCSVFile`, `RTDSOutFile`, `PSCADInfFile`, `WaveFile`.

Inferred import flow:

1. User opens one or more files through the wave import dialog.
2. The app rejects unsupported formats before adding to the project.
3. The app prevents duplicate file import into the same project.
4. Batch import is asynchronous. While importing, curve import is blocked.
5. Imported files become `WaveFileItem` nodes under a file group/workspace.
6. For each file, parsed channels are inserted into the database/tree with `DbManager::insertChannel(WaveFile*, int)`.
7. User can attach custom scripts to a wave file through `WaveFileItem::applyCustomScript(QString, QStringList, QList<ChannelCustomInfo>)`.

Export/report flow:

- DOCX export is explicit: `*.docx`, `DDWindPidWidget::exportReport`, `PVPidWidget::exportReport`, `StatcomPidWidget::exportReport`.
- Java jar is a report helper. Its manifest main class is `cn.csg.sepri.App`; bundled libraries include `poi-tl`, Apache POI, Batik, XMLBeans, and Commons libraries.
- Image/SVG path is supported or embedded: `Batik`, `.svg`, `biasSVG.html`, `g.export("...\testsss.bmp")`.
- Print preview exists through `&Print Preview`.

## 3. Wave Interaction Rules

Confidence: medium-high for the data model, medium for mouse gestures. Static strings expose models and tools but not the complete event mapping.

Workspace behavior:

- Uses an MDI waveform workspace. Multiple wave windows/subwindows can be opened, switched, tiled or normalized.
- File grouping exists via `<FileGroup Name="文件组1">` in `xIAOAO.wavx`.
- One project can hold many wave files. Sample project has 1 file group, 19 files, and 122 custom curves.

Channel and curve tree behavior:

- Channel tree classes: `ChannelWidget`, `ChannelGroupWidget`, `ChannelItem`, `ChannelOperationDialog`.
- Curve tree classes: `CurveTreeWidgetItem`, `PVCurveTreeItem`, `DDCurveTreeItem`, `StatcomCurveTreeItem`, `WTMLCurveTreeItem`.
- Analog/digital channel icons: `analog-chn.svg`, `digital-chn0.svg`, `digital-chn1.svg`, `digital-chn-var.svg`.
- Same-name handling icon: `samename.svg`.
- Curve hover/inspection: `CurveToolTip`.

Analysis/derived curve behavior:

- FFT: `FFTWidget`.
- Prony analysis: `PronyWidget`, `CNRProny`, `pronywidget.cpp`.
- Phasor calculation: `Channel::phasorAt`.
- Channel operations: `channeloperation.cpp`, `ChannelOperationDialog`.
- Script engine: embedded Python with `from repid import *`, `from math import *`, `pandas as pd`, `applyCustomScript`.

Derived curve functions found in `xIAOAO.wavx`:

- `pseqp(Ch19,Ch20,Ch21,Ch25,Ch26,Ch27)/(240)`
- `pseqip(Ch19,Ch20,Ch21,Ch25,Ch26,Ch27)/(3.95897)`
- `pseqq(Ch19,Ch20,Ch21,Ch25,Ch26,Ch27)/(240)`
- `pseqiq(Ch19,Ch20,Ch21,Ch25,Ch26,Ch27)/(3.95897)`
- `pseqll(Ch19,Ch20,Ch21)/(35)`
- `pseqph(Ch25,Ch26,Ch27)/(3.95897)`

These imply built-in positive-sequence derived curves for active power, active current, reactive power, reactive current, line voltage RMS, and phase current RMS.

Visible cursor evidence:

- `X2 =` appears in embedded graph text. X1/X2 behavior needs sandbox confirmation.

## 4. Report Template Fields

Template syntax is `{{field}}`, `{{@imageField}}`, and `{{+sectionOrLoopField}}`.

Common report fields:

- Section/table metadata: `{{实测数据及参数辩识}}`, `{{SectionTitle}}`, `{{TableTitle}}`.
- Image placeholders: `{{@U}}`, `{{@P}}`, `{{@IP}}`, `{{@Q}}`, `{{@IQ}}`.
- Deviation table fields use region prefixes: `F1_`, `F2_`, `F3_`, `F4_`, `F5_`, `FG_`, `FU_`.
- Measured quantities: `U`, `P`, `IP`, `Q`, `IQ`.
- Region suffixes: `_A`, `_B1`, `_B2`, `_C1`, `_C2`.

Single-condition validation templates:

- Wind low-voltage ride-through: fields include `{{@U}}`, `{{@IQ}}`, `{{@P}}`, `{{@Q}}`, `{{F1_P_A}}`, `{{F2_P_B1}}`, `{{F3_Q_C2}}`, `{{FG_IQ}}`, `{{FU_A}}`.
- PV low-voltage ride-through: fields include `{{@U}}`, `{{@P}}`, `{{@IP}}`, `{{@Q}}`, `{{@IQ}}`, `{{F1_U_A}}`, `{{F2_U_B1}}`, `{{FG_U}}`.
- Statcom low-voltage ride-through: fields include `{{@U}}`, `{{@IQ}}`, `{{@Q}}`, `{{F1_Q_A}}`, `{{F2_Q_B1}}`, `{{FG_Q}}`.

Multi-condition validation templates:

- Fields are loop-like blocks beginning with `{{+...}}`.
- Power group prefixes:
  - `LP`: low power, shown in text as `0.1Pn <= P <= 0.3Pn`.
  - `MP`: middle power, present in PV templates.
  - `HP`: high power, shown in text as `P >= 0.9Pn`.
- Fault/voltage pattern:
  - `3ph020pu`, `3ph035pu`, `3ph050pu`, `3ph075pu`, `3ph090pu`, `3ph120pu`, `3ph125pu`, `3ph130pu`.
  - PV template also has denser points: `000pu`, `005pu`, `010pu`, `015pu`, `025pu`, `030pu`, `040pu`, `045pu`, `055pu`, `060pu`, `065pu`, `070pu`, `080pu`, `085pu`, `110pu`, `115pu`.
- Data fields append `Data`, e.g. `{{+LP3ph020puData}}`.
- Statcom has capacitive/inductive suffixes: `C` and `L`, e.g. `{{+LP3ph020puCData}}`, `{{+HP3ph130puLData}}`.
- BPA card block: `{{+BPACard}}`.

Card/parameter templates:

- Statcom card fields include `BASE`, `MVABASE`, `T1`-`T5`, `TP`, `TS`, `KP`, `KI`, `KPQ`, `KIQ`, `QMAX`, `QMIN`, `VMAX`, `VMIN`, `LV_*`, `HV_*`.
- PV card fields include `base`, `MVABASE`, `PPER`, `QPER`, `Uoc`, `Isc`, `Um`, `Im`, `Nshunt`, `Nser`, `Cf`, `VOL_LOW*`, `VOL_HIGH*`, `EU_*`, `EZ_*`, `L_LP_*`, `H_LP_*`, `L_LQ_*`, `H_LQ_*`.
- Direct-drive wind card fields include `base`, `MVABASE`, `PN`, `VC0`, `C`, `VDC_0`, `VDC_1`, `mr_R`, `my_ityp`, `ICHOPPER_FLG`, `EU_*`, `EZ_*`, `L_LP_*`, `H_LP_*`, `L_LQ_*`, `H_LQ_*`.

## 5. Channel And Curve Metadata Model

`xIAOAO.wavx` root:

- XML root: `Repid`
- Attributes: `BaseMVA="240"`, `BaseValue="220"`, `BaseVoltage="35"`, `src="硬件在环"`, `LegendLocation`.

Project hierarchy:

```text
Repid
  DatFiles
    FileGroup Name="文件组1"
      File AbsolutePath=... RelativePath=...
        CustomCurves
          Curve ...
```

File metadata:

- `AbsolutePath`
- `RelativePath`
- `FileName`
- `FileHash`
- `TimeOffset`
- `AutoColor`

Curve metadata:

- Identity: `ID`, `Name`, `RawName`, `FullPath`, `FileName`.
- Type/grouping: `Type`, `Group`, `Unit`.
- Value range: `Min`, `Max`.
- Display transform: `k`, `b`.
- Time transform: `TimeScale`, `TimeOffset`.
- Rendering: `Color`, `PenStyle`, `DrawPoint`, `AutoColor`.
- Electrical phase/sequence: `Phase`.
- Derivation: `Script`, `ScriptVars`.

Example curve:

```xml
<Curve
  ID="52"
  Name="S1) VG_基波正序rmsll"
  RawName="S1) VG_基波正序rmsll"
  Type="0"
  Min="0.23697046456540566"
  Max="1.0277297855201688"
  k="1"
  b="0"
  PenStyle="1"
  Color="#ff000000"
  AutoColor="1"
  TimeScale="1"
  TimeOffset="0"
  ScriptVars="0=S1) VGA&#xa;1=S1) VGB&#xa;2=S1) VGC&#xa;"
  Script="pseqll(Ch1,Ch2,Ch3)/(220)"
/>
```

## 6. Migration Notes For Scope Analyzer

Best-fit concepts to port:

- `WaveFile` -> dataset group/source.
- `FileGroup` -> dataset group folder.
- `Curve` -> channel display metadata plus optional derived expression.
- `Script`/`ScriptVars` -> derived-channel expression model.
- `BaseMVA`, `BaseValue`, `BaseVoltage` -> dataset-level engineering-base metadata.
- `TimeScale`, `TimeOffset` -> existing time alignment/display settings.
- `k`, `b` -> existing scale ratio plus possible offset support.
- `PenStyle`, `Color`, `AutoColor` -> line style/color display settings.
- DOCX `{{@...}}` fields -> exported waveform image slots.
- DOCX `F1/F2/F3/F4/F5/FG/FU` fields -> validation segment metrics.

Needs sandbox confirmation:

- Exact ribbon tab/group labels.
- Mouse gestures for zoom, pan, X1/X2 cursors, context menus.
- Whether `.xlsx` is an import format, an export format, or both.
- How report export maps selected wave windows/curves into `{{@...}}` placeholders.
- How validation widgets bind files to LP/MP/HP and voltage-condition placeholders.

## 7. Scope Analyzer Feature Comparison

Status legend:

- `已有`: Scope Analyzer already has the feature or a close equivalent.
- `可移植`: Repid has a useful behavior/model that can be implemented cleanly in Scope Analyzer.
- `不做`: Behavior is outside Scope Analyzer's current product boundary.
- `需要沙箱确认`: Static extraction is insufficient; observe the software before designing.

| Repid capability | Scope Analyzer current state | Status | Migration note |
| --- | --- | --- | --- |
| Ribbon/system menu with new/open/save/recent/print preview | Scope uses egui top menus for import/export/layout/config/help, recent files, and export actions. | 已有 | Keep Scope's simpler menu style; do not copy Ribbon UI. |
| MDI multi-window workspace (`QMdiArea`, new/switch windows) | Scope uses a fixed oscilloscope pane layout (`scope_layout_rows`, `scope_layout_cols`) inside one window. | 不做 | MDI would conflict with Scope's dense analyzer layout. |
| Left/right sidebars and collapsible panels | Scope already has channel and analysis panels plus toggles. | 已有 | Existing panel behavior is enough. |
| Import one or many wave files asynchronously | Scope has import worker/cancel state and multi-dataset import. | 已有 | Repid duplicate/blocked import messages can inspire better status text. |
| Duplicate-file rejection in project | Scope has recent files and imported datasets, but duplicate policy needs code-level confirmation. | 可移植 | Add if users hit accidental duplicate imports. |
| `.wavx` XML project containing file groups, file refs, custom curves | Scope has separate display/dataset config JSON files but no Repid `.wavx` import. | 可移植 | Add a Repid project importer that maps file groups, time offsets, display metadata, and derived curves where possible. |
| `.wavdat` binary sidecar | Scope has binary DAT support, but not Repid `.wavdat`. | 可移植 | Need format reverse engineering or sample decode before implementation. |
| COMTRADE `.cfg` + data import | Scope supports CSV, cloud Content CSV, local ADATA/DDATA CSV, and DAT. | 可移植 | High-value data source adapter; matches Scope's `DataSource` boundary. |
| RTDS `.out`/`.cur` import | Scope does not currently expose RTDS-specific parsers. | 可移植 | Add adapters after sample format inspection. |
| PSCAD `.inf`/`.infx` import | Not currently supported. | 可移植 | Useful if target users have PSCAD outputs. |
| MATLAB `.mat` import | Not currently supported. | 可移植 | Likely lower priority unless users provide real MAT samples. |
| `.xlsx` import/export | Scope exports CSV-like data and DOCX/images; no spreadsheet data source is evident. | 需要沙箱确认 | Static strings show `.xlsx`, but purpose is unclear. |
| File group tree | Scope has primary plus imported datasets and dataset group names/config. | 已有 | Scope's dataset model is close enough; `.wavx` importer can map groups into dataset display names. |
| Channel tree with analog/digital distinction | Scope supports analog/digital channels through combined/bitfield data sources and channel metadata. | 已有 | Preserve Scope's current channel panel. |
| Per-channel display name edit | Scope supports display names and name import/export. | 已有 | Direct match. |
| Per-channel color, line style, line width, scale | Scope stores `channel_colors`, `line_widths`, `line_patterns`, `channel_scales`. | 已有 | Repid's `k`, `PenStyle`, `Color`, `AutoColor` can map directly; Repid `b` offset is not present. |
| Per-curve display offset `b` | Scope currently has scale but no obvious per-channel y-offset. | 可移植 | Add only if `.wavx` imports need faithful display. |
| Time offset/time scale per file or curve | Scope has dataset `time_offset`, time sync, and channel/dataset config. | 已有 | Repid `TimeOffset` maps well; `TimeScale` may need an explicit field. |
| Base values (`BaseMVA`, `BaseValue`, `BaseVoltage`) | Scope has sample rate/harmonic base options, but no dataset-level engineering-base metadata. | 可移植 | Useful for per-unit derived curves and report context. |
| Script-derived curves (`pseqp`, `pseqiq`, etc.) | Scope has derived PLL/dq0 curves and sequence analysis, but no general user expression engine. | 可移植 | Implement a small whitelisted expression model before considering Python. |
| Embedded Python console and `from repid import *` scripting | Scope has no embedded scripting console. | 不做 | Too much security and product complexity for current analyzer. |
| Positive-sequence derived curves as selectable curves | Scope has sequence analysis results and PLL/dq0 derived curves. | 可移植 | Add first-class derived channels for U/P/IP/Q/IQ if needed by reports. |
| FFT and harmonic analysis | Scope has FFT, harmonics 0-10, phase, THD. | 已有 | Existing implementation is strong. |
| Prony analysis | Scope does not appear to have Prony. | 可移植 | Specialized; defer unless users need oscillation/modal analysis. |
| X1/X2 cursors and cursor range measurements | Scope has X1/X2 placement, hide/show, fit cursors, measurements, cursor-range export. | 已有 | Direct match. |
| Mouse zoom/pan/context menu behavior | Scope has wheel zoom, Ctrl-wheel time zoom, drag zoom box, right-drag pan per README. | 已有 | Exact Repid gestures still need sandbox if matching muscle memory matters. |
| Curve tooltip/highlight | Scope has `hovered_channel` and channel hover highlighting behavior. | 已有 | Good enough unless Repid tooltip has extra values. |
| Annotation tools for exported waveform images | Scope has export annotation model: variable labels, text, arrows, rectangles/ellipses, brush, eraser, undo/redo. | 已有 | Direct match, likely more complete than Repid static evidence. |
| PNG/SVG export | Scope has `png_export.rs`, `svg_export.rs`, and export image format selection. | 已有 | Direct match. |
| DOCX export with generated built-in template | Scope has `word_export.rs` and batch waveform/DOCX export. | 已有 | Current template is internal and simple. |
| External DOCX template placeholder filling | Scope explicitly does not import external templates in current docs/spec. | 可移植 | Implement as a report-template feature, separate from existing simple DOCX writer. |
| Report image placeholders `{{@U}}`, `{{@P}}`, `{{@IP}}`, `{{@Q}}`, `{{@IQ}}` | Scope can generate waveform images, but no placeholder-driven template mapping. | 可移植 | Map selected panes/derived curves to named figure slots. |
| Report metric fields `F1/F2/F3/F4/F5/FG/FU` | Scope measures cursor range but does not model validation segments. | 可移植 | Add validation-segment metrics only if low-voltage ride-through reports are in scope. |
| Multi-condition report blocks `{{+LP3ph020puData}}` etc. | Scope batch export has time windows/datasets/panes, not ride-through condition binding. | 可移植 | Needs a condition model: power group, fault type, voltage level, measured/simulated pair. |
| Wind/PV/Statcom model validation workflows | Scope is a general waveform analyzer, not a model validation suite. | 需要沙箱确认 | Decide whether to build a limited report assistant or a full validation workflow. |
| BPA/PSD model card templates | Scope does not manage BPA cards or model parameters. | 可移植 | Template filling is possible; model-card editing should be separate and optional. |
| BPA/RTDS build/run controls, GDB remote control, `bjdsrun.exe` | Scope does not run external simulators/debuggers. | 不做 | Outside offline waveform analyzer scope. |
| Online update/check URLs | Scope release process is local/offline. | 不做 | Avoid network update behavior. |
| Validation pass/fail icons and workflow state | Scope has analysis results but not validation pass/fail state. | 可移植 | Useful only with report/validation feature. |

Recommended implementation order:

1. Keep existing Scope features as the foundation: data sources, panes, cursors, FFT/sequence, export annotation, DOCX.
2. Add `Repid .wavx` import as a metadata-only bridge, mapping file groups, curve display settings, time offsets, and supported derived expressions.
3. Add data adapters by sample availability: COMTRADE first, then RTDS `.out/.cur`, then `.wavdat` or PSCAD/MAT/XLSX if real users need them.
4. Add external DOCX template filling for `{{@...}}` image slots and scalar table fields.
5. Add validation-segment/report workflows only after sandbox confirms how Repid binds wave files to LP/MP/HP and voltage conditions.
