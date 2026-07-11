use std::{
    collections::HashSet,
    env,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    ops::RangeInclusive,
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{Arc, Once},
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eframe::egui::{self, Color32, PointerButton, RichText, Stroke};
use egui_plot::{
    Envelope, HLine, Legend, Line, LineStyle, Plot, PlotBounds, PlotPoint, PlotUi, Text, VLine,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{
    data::{
        csv_reader_from_path_with_headers, BitfieldDigitalDataSource, ChannelMeta,
        CloudCsvDataSource, CombinedDataSource, CsvDataSource, DatDataSource, DataCancelToken,
        DataResult, DataSource, DatasetMeta, MergedLeadingBitsDataSource, RangeSummary,
        RenamedDataSource, SampleBlock, CHANNEL_UNIT_ANALOG, CHANNEL_UNIT_DIGITAL, VARIABLE_NAMES,
    },
    export_annotation::{
        self, AnnotationColor, AnnotationLineStyle, ArrowAnnotation as ExportArrowAnnotation,
        CanvasPoint, CanvasRect, InkStroke as ExportInkStroke,
        RectangleAnnotation as ExportRectangleAnnotation, ShapeKind,
        TextAnnotation as ExportTextAnnotation,
    },
    fft::{self, FftResult, SequenceResult},
    png_export::{Canvas, ClipRect, Rgba, StrokeStyle, TextStyle, WaveformCanvas},
    svg_export::SvgCanvas,
    transforms,
};

mod export;
mod jobs;
mod live_ui;
mod plot;
mod state;

use export::{
    ensure_export_extension, ExportDpi, ExportImageFormat, ExportLabelFontStyle,
    ExportPreviewEditState, ExportPreviewPrimaryAction, ExportResolution, RenderedWaveformImage,
};
use jobs::JobCancelToken;
use plot::{
    CompareDatasetJobResult, ComparePlotJobInput, ComparePlotJobResult, PanePlotSelections,
    PlotCacheKey, PlotJobData, PlotJobResult, PlotSelections, PreparedPlotSeries,
};
use state::ImportedDataset;
pub use state::ScopeApp;

const MAX_DRAW_POINTS_PER_CHANNEL: usize = 8_000;
const MAX_TOTAL_DRAW_POINTS: usize = 60_000;
const MIN_DRAW_POINTS_PER_CHANNEL: usize = 192;
const SOFTWARE_MAX_DRAW_POINTS_PER_CHANNEL: usize = 3_000;
const SOFTWARE_MAX_TOTAL_DRAW_POINTS: usize = 24_000;
const SOFTWARE_MIN_DRAW_POINTS_PER_CHANNEL: usize = 128;
const MAX_RAW_PLOT_SOURCE_SAMPLES: usize = 250_000;
const LAYOUT_RESIZE_DRAW_POINTS_PER_CHANNEL: usize = 384;
const LAYOUT_RESIZE_ACTIVE_GRACE: Duration = Duration::from_millis(180);
const MAX_SCOPE_LEGEND_SERIES: usize = 32;
const SOFTWARE_MAX_SCOPE_LEGEND_SERIES: usize = 16;
const SCOPE_GRID_TARGET_X_LINES: usize = 10;
const SCOPE_GRID_TARGET_Y_LINES: usize = 8;
const SCOPE_GRID_MAX_LINES_PER_AXIS: usize = 80;
const DEFAULT_PLOT_PIXEL_WIDTH: f32 = 1024.0;
const PLOT_CACHE_WIDTH_BUCKET: f32 = 64.0;
const MAX_FFT_POINTS: usize = 262_144;
const MAX_AUTO_MEASURE_POINTS: usize = 131_072;
const EXPORT_CHUNK_SAMPLES: usize = 100_000;
const ZOOM_BOX_MIN_PIXELS: f32 = 8.0;
const DEFAULT_WHEEL_ZOOM_SENSITIVITY: f64 = 0.0625;
const MIN_WHEEL_ZOOM_SENSITIVITY: f64 = 0.005;
const MAX_WHEEL_ZOOM_SENSITIVITY: f64 = 0.40;
const DEFAULT_CHANNEL_LINE_WIDTH: f32 = 1.4;
const MIN_CHANNEL_LINE_WIDTH: f32 = 0.5;
const MAX_CHANNEL_LINE_WIDTH: f32 = 8.0;
const DEFAULT_CHANNEL_SCALE: f32 = 1.0;
const MIN_CHANNEL_SCALE: f32 = -1_000_000.0;
const MAX_CHANNEL_SCALE: f32 = 1_000_000.0;
const MAX_RECENT_FILES: usize = 12;
const MAX_RECENT_CONFIGS: usize = 12;
const APP_HOME_ENV: &str = "SCOPE_APP_HOME";
const EXPORT_TOOL_ICON_SIZE: [usize; 2] = [32, 32];
const EXPORT_TOOL_ICON_DISPLAY_SIZE: f32 = 22.0;
const EXPORT_TOOL_ICON_SELECT: &[u8] = include_bytes!("../resources/export-icons/select.rgba");
const EXPORT_TOOL_ICON_TEXT: &[u8] = include_bytes!("../resources/export-icons/text.rgba");
const EXPORT_TOOL_ICON_ARROW: &[u8] = include_bytes!("../resources/export-icons/arrow.rgba");
const EXPORT_TOOL_ICON_RECTANGLE: &[u8] =
    include_bytes!("../resources/export-icons/rectangle.rgba");
const EXPORT_TOOL_ICON_BRUSH: &[u8] = include_bytes!("../resources/export-icons/brush.rgba");
const EXPORT_TOOL_ICON_ERASER: &[u8] = include_bytes!("../resources/export-icons/eraser.rgba");
const EXPORT_TOOL_ICON_UNDO: &[u8] = include_bytes!("../resources/export-icons/undo.rgba");
const EXPORT_TOOL_ICON_REDO: &[u8] = include_bytes!("../resources/export-icons/redo.rgba");
const EXPORT_PREVIEW_ZOOM_PRESETS: [f32; 7] = [0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 4.0];
const LOCAL_CSV_PAIR_MTIME_WINDOW_MS: i128 = 10_000;
const CHANNEL_NAME_AVERAGE_CHAR_WIDTH: f32 = 7.0;
const CHANNEL_PANEL_DEFAULT_WIDTH: f32 = 220.0;
const CHANNEL_PANEL_MIN_WIDTH: f32 = 56.0;
const CHANNEL_PANEL_MAX_WIDTH: f32 = 320.0;
const ANALYSIS_PANEL_DEFAULT_WIDTH: f32 = 360.0;
const ANALYSIS_PANEL_MIN_WIDTH: f32 = 260.0;
const ANALYSIS_PANEL_MAX_WIDTH: f32 = 380.0;
const MIN_CENTRAL_PANEL_WIDTH: f32 = 360.0;
const MAX_CHANNEL_PANEL_FRACTION: f32 = 0.45;
const MAX_ANALYSIS_PANEL_FRACTION: f32 = 0.50;
const CHANNEL_NAME_COLUMN_MIN_WIDTH: f32 = 56.0;
const CHANNEL_NAME_HIDE_WIDTH: f32 = 42.0;
const CHANNEL_NAME_COLUMN_MAX_WIDTH: f32 = 520.0;
const ANALYSIS_PANEL_CONTENT_MIN_WIDTH: f32 = 520.0;
const THREE_PHASE_SELECTOR_VERTICAL_WIDTH: f32 = 760.0;
const MEASUREMENT_CHANNEL_COLUMN_WIDTH: f32 = 132.0;
const MEASUREMENT_VALUE_COLUMN_WIDTH: f32 = 78.0;
const ANALYSIS_INPUT_LABEL_WIDTH: f32 = 84.0;
const ANALYSIS_LABEL_COLUMN_WIDTH: f32 = 56.0;
const ANALYSIS_VALUE_COLUMN_WIDTH: f32 = 86.0;
const ANALYSIS_CHANNEL_LABEL_CHARS: usize = 14;
const MAX_SCOPE_LAYOUT_ROWS: usize = 4;
const MAX_SCOPE_LAYOUT_COLS: usize = 4;
const MAX_TIME_SYNC_POINTS: usize = 20_000;
const PLOT_RELOAD_DEBOUNCE: Duration = Duration::from_millis(90);
const DERIVED_CHANNEL_COUNT: usize = 4;
const DERIVED_CHANNEL_NAMES: [&str; DERIVED_CHANNEL_COUNT] =
    ["PLL theta (deg)", "dq0.d", "dq0.q", "dq0.0"];
const DEFAULT_EXPORT_RESOLUTION: ExportResolution = ExportResolution::Ultra;
const DEFAULT_EXPORT_ARROW_SIZE: f32 = 11.0;
const MIN_EXPORT_ARROW_SIZE: f32 = 4.0;
const MAX_EXPORT_ARROW_SIZE: f32 = 28.0;
const DEFAULT_EXPORT_LABEL_SCALE: i32 = 3;
const MIN_EXPORT_LABEL_SCALE: i32 = 1;
const MAX_EXPORT_LABEL_SCALE: i32 = 8;

struct SummaryEnvelopeDraw<'a> {
    high_points: &'a Arc<[PlotPoint]>,
    high_lightweight_points: Option<&'a Arc<[PlotPoint]>>,
    low_points: Option<&'a Arc<[PlotPoint]>>,
    low_lightweight_points: Option<&'a Arc<[PlotPoint]>>,
    lightweight: bool,
    legend_name: Option<String>,
    line_color: Color32,
    line_style: LineStyle,
    line_width: f32,
}

struct PlotLoadOptions<'a> {
    budget_series_count: usize,
    plot_pixel_width: f32,
    software_performance_mode: bool,
    cancel: Option<&'a DataCancelToken>,
}

#[derive(Clone, Debug)]
struct SidebarWidthRanges {
    channel: RangeInclusive<f32>,
    analysis: RangeInclusive<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreePhaseSelectorLayout {
    Horizontal,
    Vertical,
}

fn default_sample_rate_hz() -> f64 {
    1000.0
}

fn default_harmonic_base_hz() -> f64 {
    50.0
}

fn default_scope_layout_rows() -> usize {
    1
}

fn default_scope_layout_cols() -> usize {
    1
}

fn default_language() -> Language {
    Language::Zh
}

fn default_theme_mode() -> ThemeMode {
    ThemeMode::Light
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum Language {
    Zh,
    En,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UiText {
    Error,
    Dismiss,
    ScopeLayout,
    Rows,
    Columns,
    ActivePane,
    PaneSelectHint,
    QuickSelect,
    Single,
    DeleteDataset,
    DeleteSelectedDatasets,
    DatasetSettings,
    DatasetName,
    MarkForDeletion,
    SelectAllChannels,
    LineStyle,
    Source,
    Analog,
    Digital,
    NoMatchingChannels,
    ImportData,
    SelectDataFiles,
    ImportingData,
    WaveformCsv,
    Export,
    ExportData,
    ExportAllRange,
    ExportCursorRangeData,
    ExportCheckedChannels,
    ExportAllChannels,
    ExportAllFormats,
    ExportWaveformImage,
    BatchExport,
    ImportAction,
    ExportAction,
    Config,
    NameConfig,
    DisplayConfig,
    ShortcutConfig,
    DatasetConfig,
    BatchExportWaveformDocx,
    ExportQuality,
    ImageFormat,
    ShowAnnotations,
    CursorDataTable,
    AutoVariableLabels,
    Pane,
    TimeRange,
    ManualRange,
    ArrowLineStyle,
    RecentFiles,
    NoRecentFiles,
    MissingFile,
    ClearRecentFiles,
    Layout,
    View,
    ResetView,
    FitCursors,
    AutoY,
    ImportNames,
    ExportNames,
    RecentNames,
    NoRecentNames,
    ClearRecentNames,
    RecentConfigs,
    NoRecentConfigs,
    ClearRecentConfigs,
    Options,
    Help,
    Diagnostics,
    CopyDiagnostics,
    OpenLogDirectory,
    Interaction,
    UiLanguage,
    Theme,
    ImageExportLabels,
    ArrowSize,
    ArrowLabelColor,
    CustomColor,
    VariableNameSize,
    VariableNameFont,
    ResetExportLabelStyle,
    HarmonicBasePrefix,
    PllSyncSource,
    TimeAxisSync,
    AlignDatasetTimeAxes,
    SyncByPhase,
    ClearSync,
    TimeSyncSource,
    WheelZoomSensitivity,
    ResetSensitivity,
    Shortcuts,
    ToggleCursors,
    DeselectAllChannels,
    ResetShortcuts,
    DoubleClickRename,
    ColorSettings,
    Color,
    LineWidth,
    ScaleRatio,
    Scale,
    Zoom2x,
    ShrinkHalf,
    Reset,
    Derived,
    Channels,
    Datasets,
    Clear,
    FilterChannelsHint,
    NoDataLoaded,
    AnalysisDataset,
    AnalysisChannel,
    AnalysisInput,
    ThreePhaseChannels,
    Hidden,
    Measurements,
    NoChannelsSelected,
    CalculatingMeasurements,
    Channel,
    Min,
    Max,
    SamplesPrefix,
    Sequence,
    SequenceNeedsThreeAnalogChannels,
    CalculatingSequence,
    Component,
    Amplitude,
    Phase,
    PositiveRatio,
    ZeroSequence,
    PositiveSequence,
    NegativeSequence,
    PllDq0Title,
    Dq0Input,
    PllNeedsThreeAnalogChannels,
    PllDistinctChannels,
    SelectDerivedCurves,
    PllDq0Enabled,
    PllUsesAnalysisDataset,
    CalculatingPllDq0,
    CalculatingFft,
    NoAnalogFftChannels,
    FftNeedsCursorSamples,
    Order,
    FundamentalRatio,
    PlaceCursorX1,
    PlaceCursorX2,
    CancelPlacement,
    HideCursorX1,
    ShowCursorX1,
    HideCursorX2,
    ShowCursorX2,
    FftSampleRatePrefix,
    FftSampleRateHint,
    HarmonicBaseHint,
    TimeSyncNeedsMainThreeAnalogChannels,
    TimeSyncPhaseHint,
    ToggleLeftSidebar,
    ToggleRightSidebar,
    MouseWheelZoomY,
    CtrlWheelZoomX,
    AllDatasets,
    CursorExportHint,
}

impl UiText {
    fn get(self, language: Language) -> &'static str {
        use UiText::*;
        match (self, language) {
            (Error, Language::Zh) => "错误",
            (Error, Language::En) => "Error",
            (Dismiss, Language::Zh) => "关闭",
            (Dismiss, Language::En) => "Dismiss",
            (ScopeLayout, Language::Zh) => "示波器布局",
            (ScopeLayout, Language::En) => "Scope Layout",
            (Rows, Language::Zh) => "纵向",
            (Rows, Language::En) => "Rows",
            (Columns, Language::Zh) => "横向",
            (Columns, Language::En) => "Columns",
            (ActivePane, Language::Zh) => "当前示波器",
            (ActivePane, Language::En) => "Active Pane",
            (PaneSelectHint, Language::Zh) => {
                "请先点击一个示波器子窗口，再勾选变量，变量会放入该子窗口。"
            }
            (PaneSelectHint, Language::En) => {
                "Click a scope pane first, then check variables to place them there."
            }
            (QuickSelect, Language::Zh) => "快速选择",
            (QuickSelect, Language::En) => "Quick Select",
            (Single, Language::Zh) => "单栏",
            (Single, Language::En) => "Single",
            (DeleteDataset, Language::Zh) => "删除数据组",
            (DeleteDataset, Language::En) => "Delete Dataset",
            (DeleteSelectedDatasets, Language::Zh) => "删除已选数据组",
            (DeleteSelectedDatasets, Language::En) => "Delete Selected Datasets",
            (DatasetSettings, Language::Zh) => "数据组设置",
            (DatasetSettings, Language::En) => "Dataset Settings",
            (DatasetName, Language::Zh) => "数据组名",
            (DatasetName, Language::En) => "Dataset Name",
            (MarkForDeletion, Language::Zh) => "选中待删",
            (MarkForDeletion, Language::En) => "Mark for deletion",
            (SelectAllChannels, Language::Zh) => "全选通道",
            (SelectAllChannels, Language::En) => "Select All Channels",
            (LineStyle, Language::Zh) => "线型",
            (LineStyle, Language::En) => "Line style",
            (Source, Language::Zh) => "原始",
            (Source, Language::En) => "src",
            (Analog, Language::Zh) => "模拟量",
            (Analog, Language::En) => "Analog",
            (Digital, Language::Zh) => "数字量",
            (Digital, Language::En) => "Digital",
            (NoMatchingChannels, Language::Zh) => "没有匹配的通道。",
            (NoMatchingChannels, Language::En) => "No matching channels.",
            (ImportData, Language::Zh) => "导入数据",
            (ImportData, Language::En) => "Import Data",
            (SelectDataFiles, Language::Zh) => "选择数据文件...",
            (SelectDataFiles, Language::En) => "Select Data Files...",
            (ImportingData, Language::Zh) => "正在导入数据...",
            (ImportingData, Language::En) => "Importing data...",
            (WaveformCsv, Language::Zh) => "波形 CSV",
            (WaveformCsv, Language::En) => "Waveform CSV",
            (Export, Language::Zh) => "导出",
            (Export, Language::En) => "Export",
            (ExportData, Language::Zh) => "导出数据",
            (ExportData, Language::En) => "Export Data",
            (ExportAllRange, Language::Zh) => "全部数据",
            (ExportAllRange, Language::En) => "All Data",
            (ExportCursorRangeData, Language::Zh) => "光标 X1-X2",
            (ExportCursorRangeData, Language::En) => "Cursor X1-X2",
            (ExportCheckedChannels, Language::Zh) => "已勾选通道",
            (ExportCheckedChannels, Language::En) => "Checked Channels",
            (ExportAllChannels, Language::Zh) => "全部通道",
            (ExportAllChannels, Language::En) => "All Channels",
            (ExportAllFormats, Language::Zh) => "全部格式...",
            (ExportAllFormats, Language::En) => "All Formats...",
            (ExportWaveformImage, Language::Zh) => "导出波形图片",
            (ExportWaveformImage, Language::En) => "Export Waveform Image",
            (BatchExport, Language::Zh) => "批量导出",
            (BatchExport, Language::En) => "Batch Export",
            (ImportAction, Language::Zh) => "导入",
            (ImportAction, Language::En) => "Import",
            (ExportAction, Language::Zh) => "导出",
            (ExportAction, Language::En) => "Export",
            (Config, Language::Zh) => "配置",
            (Config, Language::En) => "Config",
            (NameConfig, Language::Zh) => "变量名配置",
            (NameConfig, Language::En) => "Name Settings",
            (DisplayConfig, Language::Zh) => "显示配置",
            (DisplayConfig, Language::En) => "Display Settings",
            (ShortcutConfig, Language::Zh) => "快捷键配置",
            (ShortcutConfig, Language::En) => "Shortcut Settings",
            (DatasetConfig, Language::Zh) => "数据组配置",
            (DatasetConfig, Language::En) => "Dataset Settings",
            (BatchExportWaveformDocx, Language::Zh) => "批量导出波形 / DOCX",
            (BatchExportWaveformDocx, Language::En) => "Batch Export Waveform / DOCX",
            (ExportQuality, Language::Zh) => "导出质量",
            (ExportQuality, Language::En) => "Export quality",
            (ImageFormat, Language::Zh) => "图片格式",
            (ImageFormat, Language::En) => "Image format",
            (ShowAnnotations, Language::Zh) => "显示标注",
            (ShowAnnotations, Language::En) => "Show annotations",
            (CursorDataTable, Language::Zh) => "显示光标数据表",
            (CursorDataTable, Language::En) => "Show cursor data table",
            (AutoVariableLabels, Language::Zh) => "自动标注变量",
            (AutoVariableLabels, Language::En) => "Auto variable labels",
            (Pane, Language::Zh) => "子窗口",
            (Pane, Language::En) => "Pane",
            (TimeRange, Language::Zh) => "时间范围",
            (TimeRange, Language::En) => "Time range",
            (ManualRange, Language::Zh) => "手动范围",
            (ManualRange, Language::En) => "Manual range",
            (ArrowLineStyle, Language::Zh) => "箭头线型",
            (ArrowLineStyle, Language::En) => "Arrow line style",
            (RecentFiles, Language::Zh) => "最近文件",
            (RecentFiles, Language::En) => "Recent Files",
            (NoRecentFiles, Language::Zh) => "暂无最近文件",
            (NoRecentFiles, Language::En) => "No recent files",
            (MissingFile, Language::Zh) => "(文件不存在)",
            (MissingFile, Language::En) => "(missing)",
            (ClearRecentFiles, Language::Zh) => "清空最近文件",
            (ClearRecentFiles, Language::En) => "Clear Recent Files",
            (Layout, Language::Zh) => "布局",
            (Layout, Language::En) => "Layout",
            (View, Language::Zh) => "视图",
            (View, Language::En) => "View",
            (ResetView, Language::Zh) => "重置视图",
            (ResetView, Language::En) => "Reset View",
            (FitCursors, Language::Zh) => "适配光标",
            (FitCursors, Language::En) => "Fit Cursors",
            (AutoY, Language::Zh) => "Y轴自适应",
            (AutoY, Language::En) => "Auto Y",
            (ImportNames, Language::Zh) => "导入变量名",
            (ImportNames, Language::En) => "Import Names",
            (ExportNames, Language::Zh) => "导出变量名",
            (ExportNames, Language::En) => "Export Names",
            (RecentNames, Language::Zh) => "最近变量名",
            (RecentNames, Language::En) => "Recent Names",
            (NoRecentNames, Language::Zh) => "暂无最近变量名",
            (NoRecentNames, Language::En) => "No recent names",
            (ClearRecentNames, Language::Zh) => "清空最近变量名",
            (ClearRecentNames, Language::En) => "Clear Recent Names",
            (RecentConfigs, Language::Zh) => "最近配置",
            (RecentConfigs, Language::En) => "Recent Configs",
            (NoRecentConfigs, Language::Zh) => "暂无最近配置",
            (NoRecentConfigs, Language::En) => "No recent configs",
            (ClearRecentConfigs, Language::Zh) => "清空最近配置",
            (ClearRecentConfigs, Language::En) => "Clear Recent Configs",
            (Options, Language::Zh) => "选项",
            (Options, Language::En) => "Options",
            (Help, Language::Zh) => "帮助",
            (Help, Language::En) => "Help",
            (Diagnostics, Language::Zh) => "诊断",
            (Diagnostics, Language::En) => "Diagnostics",
            (CopyDiagnostics, Language::Zh) => "复制诊断信息",
            (CopyDiagnostics, Language::En) => "Copy Diagnostics",
            (OpenLogDirectory, Language::Zh) => "打开日志目录",
            (OpenLogDirectory, Language::En) => "Open Log Directory",
            (Interaction, Language::Zh) => "交互",
            (Interaction, Language::En) => "Interaction",
            (UiLanguage, Language::Zh) => "界面语言",
            (UiLanguage, Language::En) => "Language",
            (Theme, Language::Zh) => "主题",
            (Theme, Language::En) => "Theme",
            (ImageExportLabels, Language::Zh) => "导出图片标注",
            (ImageExportLabels, Language::En) => "Image Export Labels",
            (ArrowSize, Language::Zh) => "箭头大小",
            (ArrowSize, Language::En) => "Arrow size",
            (ArrowLabelColor, Language::Zh) => "箭头/标注颜色",
            (ArrowLabelColor, Language::En) => "Arrow/label color",
            (CustomColor, Language::Zh) => "自定义颜色",
            (CustomColor, Language::En) => "Custom color",
            (VariableNameSize, Language::Zh) => "变量名字号",
            (VariableNameSize, Language::En) => "Variable name size",
            (VariableNameFont, Language::Zh) => "变量名字体",
            (VariableNameFont, Language::En) => "Variable name font",
            (ResetExportLabelStyle, Language::Zh) => "重置导出标注样式",
            (ResetExportLabelStyle, Language::En) => "Reset Export Label Style",
            (HarmonicBasePrefix, Language::Zh) => "谐波基准: ",
            (HarmonicBasePrefix, Language::En) => "Harmonic base: ",
            (PllSyncSource, Language::Zh) => "锁相环源",
            (PllSyncSource, Language::En) => "PLL source",
            (TimeAxisSync, Language::Zh) => "时间轴同步",
            (TimeAxisSync, Language::En) => "Time Axis Sync",
            (AlignDatasetTimeAxes, Language::Zh) => "统一数据组时间轴",
            (AlignDatasetTimeAxes, Language::En) => "Align dataset time axes",
            (SyncByPhase, Language::Zh) => "按所选变量相位同步",
            (SyncByPhase, Language::En) => "Sync by selected variable phase",
            (ClearSync, Language::Zh) => "清除同步",
            (ClearSync, Language::En) => "Clear Sync",
            (TimeSyncSource, Language::Zh) => "同步源",
            (TimeSyncSource, Language::En) => "Sync Source",
            (WheelZoomSensitivity, Language::Zh) => "滚轮缩放灵敏度",
            (WheelZoomSensitivity, Language::En) => "Wheel zoom sensitivity",
            (ResetSensitivity, Language::Zh) => "重置灵敏度",
            (ResetSensitivity, Language::En) => "Reset Sensitivity",
            (Shortcuts, Language::Zh) => "快捷键",
            (Shortcuts, Language::En) => "Shortcuts",
            (ToggleCursors, Language::Zh) => "隐藏/显示光标",
            (ToggleCursors, Language::En) => "Hide/Show Cursors",
            (DeselectAllChannels, Language::Zh) => "取消全选通道",
            (DeselectAllChannels, Language::En) => "Deselect All Channels",
            (ResetShortcuts, Language::Zh) => "重置快捷键",
            (ResetShortcuts, Language::En) => "Reset Shortcuts",
            (DoubleClickRename, Language::Zh) => "双击修改变量名",
            (DoubleClickRename, Language::En) => "Double-click to rename",
            (ColorSettings, Language::Zh) => "颜色设置",
            (ColorSettings, Language::En) => "Color Settings",
            (Color, Language::Zh) => "颜色",
            (Color, Language::En) => "Color",
            (LineWidth, Language::Zh) => "线宽",
            (LineWidth, Language::En) => "Line width",
            (ScaleRatio, Language::Zh) => "变比",
            (ScaleRatio, Language::En) => "Scale Ratio",
            (Scale, Language::Zh) => "倍率",
            (Scale, Language::En) => "Scale",
            (Zoom2x, Language::Zh) => "放大 2x",
            (Zoom2x, Language::En) => "Zoom 2x",
            (ShrinkHalf, Language::Zh) => "缩小 1/2",
            (ShrinkHalf, Language::En) => "Shrink 1/2",
            (Reset, Language::Zh) => "重置",
            (Reset, Language::En) => "Reset",
            (Derived, Language::Zh) => "派生量",
            (Derived, Language::En) => "Derived",
            (Channels, Language::Zh) => "变量",
            (Channels, Language::En) => "Channels",
            (Datasets, Language::Zh) => "数据组",
            (Datasets, Language::En) => "Datasets",
            (Clear, Language::Zh) => "清除",
            (Clear, Language::En) => "Clear",
            (FilterChannelsHint, Language::Zh) => "筛选变量，支持多关键词",
            (FilterChannelsHint, Language::En) => "Filter channels, multiple keywords",
            (NoDataLoaded, Language::Zh) => "未加载数据。",
            (NoDataLoaded, Language::En) => "No data loaded.",
            (AnalysisDataset, Language::Zh) => "分析数据组",
            (AnalysisDataset, Language::En) => "Analysis Dataset",
            (AnalysisChannel, Language::Zh) => "分析通道",
            (AnalysisChannel, Language::En) => "Analysis Channel",
            (AnalysisInput, Language::Zh) => "分析入口",
            (AnalysisInput, Language::En) => "Analysis",
            (ThreePhaseChannels, Language::Zh) => "三相通道",
            (ThreePhaseChannels, Language::En) => "3-phase Channels",
            (Hidden, Language::Zh) => "（隐藏）",
            (Hidden, Language::En) => " (hidden)",
            (Measurements, Language::Zh) => "测量",
            (Measurements, Language::En) => "Measurements",
            (NoChannelsSelected, Language::Zh) => "当前数据组没有选中通道。",
            (NoChannelsSelected, Language::En) => "No channels selected in this dataset.",
            (CalculatingMeasurements, Language::Zh) => "计算测量中...",
            (CalculatingMeasurements, Language::En) => "Calculating measurements...",
            (Channel, Language::Zh) => "通道",
            (Channel, Language::En) => "Channel",
            (Min, Language::Zh) => "最小",
            (Min, Language::En) => "Min",
            (Max, Language::Zh) => "最大",
            (Max, Language::En) => "Max",
            (SamplesPrefix, Language::Zh) => "样本数:",
            (SamplesPrefix, Language::En) => "Samples:",
            (Sequence, Language::Zh) => "正负序",
            (Sequence, Language::En) => "Sequence",
            (SequenceNeedsThreeAnalogChannels, Language::Zh) => {
                "正负序分析至少需要三个模拟量通道。"
            }
            (SequenceNeedsThreeAnalogChannels, Language::En) => {
                "Sequence analysis needs at least three analog channels."
            }
            (CalculatingSequence, Language::Zh) => "计算正负序中...",
            (CalculatingSequence, Language::En) => "Calculating sequence...",
            (Component, Language::Zh) => "分量",
            (Component, Language::En) => "Component",
            (Amplitude, Language::Zh) => "幅值",
            (Amplitude, Language::En) => "Amplitude",
            (Phase, Language::Zh) => "相位",
            (Phase, Language::En) => "Phase",
            (PositiveRatio, Language::Zh) => "相对正序比例",
            (PositiveRatio, Language::En) => "% Positive",
            (ZeroSequence, Language::Zh) => "零序",
            (ZeroSequence, Language::En) => "Zero",
            (PositiveSequence, Language::Zh) => "正序",
            (PositiveSequence, Language::En) => "Positive",
            (NegativeSequence, Language::Zh) => "负序",
            (NegativeSequence, Language::En) => "Negative",
            (PllDq0Title, Language::Zh) => "PLL/dq0",
            (PllDq0Title, Language::En) => "PLL/dq0",
            (Dq0Input, Language::Zh) => "dq0 输入",
            (Dq0Input, Language::En) => "dq0 input",
            (PllNeedsThreeAnalogChannels, Language::Zh) => {
                "PLL/dq0 至少需要三个模拟量通道。"
            }
            (PllNeedsThreeAnalogChannels, Language::En) => {
                "PLL/dq0 needs at least three analog channels."
            }
            (PllDistinctChannels, Language::Zh) => "A/B/C 通道不能重复。",
            (PllDistinctChannels, Language::En) => "A/B/C channels must be distinct.",
            (SelectDerivedCurves, Language::Zh) => "在左侧派生量分组勾选派生曲线。",
            (SelectDerivedCurves, Language::En) => "Select derived curves below.",
            (PllDq0Enabled, Language::Zh) => "PLL/dq0 派生曲线已启用。",
            (PllDq0Enabled, Language::En) => "PLL/dq0 uses the Analysis Dataset selected above.",
            (PllUsesAnalysisDataset, Language::Zh) => {
                "PLL/dq0 使用所选分析数据组中的变量作为锁相环输入。"
            }
            (PllUsesAnalysisDataset, Language::En) => {
                "PLL/dq0 uses the selected variables in the Analysis Dataset as PLL inputs."
            }
            (CalculatingPllDq0, Language::Zh) => "PLL/dq0 计算中...",
            (CalculatingPllDq0, Language::En) => "Calculating PLL/dq0...",
            (CalculatingFft, Language::Zh) => "FFT 计算中...",
            (CalculatingFft, Language::En) => "Calculating FFT...",
            (NoAnalogFftChannels, Language::Zh) => "没有可用于 FFT 的模拟量通道。",
            (NoAnalogFftChannels, Language::En) => "No analog channels are available for FFT.",
            (FftNeedsCursorSamples, Language::Zh) => "FFT 需要光标区间内至少 16 个采样点。",
            (FftNeedsCursorSamples, Language::En) => {
                "FFT needs at least 16 samples in the cursor range."
            }
            (Order, Language::Zh) => "次数",
            (Order, Language::En) => "Order",
            (FundamentalRatio, Language::Zh) => "相对基波比例",
            (FundamentalRatio, Language::En) => "% Fundamental",
            (PlaceCursorX1, Language::Zh) => "放置光标 X1",
            (PlaceCursorX1, Language::En) => "Place Cursor X1",
            (PlaceCursorX2, Language::Zh) => "放置光标 X2",
            (PlaceCursorX2, Language::En) => "Place Cursor X2",
            (CancelPlacement, Language::Zh) => "取消放置",
            (CancelPlacement, Language::En) => "Cancel Placement",
            (HideCursorX1, Language::Zh) => "隐藏光标 X1",
            (HideCursorX1, Language::En) => "Hide Cursor X1",
            (ShowCursorX1, Language::Zh) => "显示光标 X1",
            (ShowCursorX1, Language::En) => "Show Cursor X1",
            (HideCursorX2, Language::Zh) => "隐藏光标 X2",
            (HideCursorX2, Language::En) => "Hide Cursor X2",
            (ShowCursorX2, Language::Zh) => "显示光标 X2",
            (ShowCursorX2, Language::En) => "Show Cursor X2",
            (FftSampleRatePrefix, Language::Zh) => "FFT Fs: ",
            (FftSampleRatePrefix, Language::En) => "FFT Fs: ",
            (FftSampleRateHint, Language::Zh) => {
                "默认 FFT Fs 为 1000 Hz。云端 Content CSV 使用该值生成时间轴，FFT 频率轴也使用该值。"
            }
            (FftSampleRateHint, Language::En) => {
                "Default FFT Fs is 1000 Hz. Cloud Content CSV uses this value for the time axis; the FFT frequency axis explicitly uses it too."
            }
            (HarmonicBaseHint, Language::Zh) => {
                "谐波使用该基准频率显示 0 次直流量和 1-10 次谐波，默认 50 Hz。"
            }
            (HarmonicBaseHint, Language::En) => {
                "Harmonics show the 0th DC component and the 1st-10th orders using this base frequency. Default is 50 Hz."
            }
            (TimeSyncNeedsMainThreeAnalogChannels, Language::Zh) => {
                "时间轴同步至少需要主数据组三个模拟量通道。"
            }
            (TimeSyncNeedsMainThreeAnalogChannels, Language::En) => {
                "Time-axis sync needs at least three analog channels in the primary dataset."
            }
            (TimeSyncPhaseHint, Language::Zh) => {
                "以主数据为参考，按所选变量和谐波基准频率计算相位差，并平移附加数据组的时间轴。"
            }
            (TimeSyncPhaseHint, Language::En) => {
                "Uses the primary dataset as reference, calculates phase difference from the selected variables at the harmonic base frequency, then shifts extra dataset time axes."
            }
            (ToggleLeftSidebar, Language::Zh) => "切换左侧栏",
            (ToggleLeftSidebar, Language::En) => "Toggle left sidebar",
            (ToggleRightSidebar, Language::Zh) => "切换右侧栏",
            (ToggleRightSidebar, Language::En) => "Toggle right sidebar",
            (MouseWheelZoomY, Language::Zh) => "鼠标滚轮缩放纵轴。",
            (MouseWheelZoomY, Language::En) => "Mouse wheel zooms the vertical axis.",
            (CtrlWheelZoomX, Language::Zh) => {
                "Ctrl + 鼠标滚轮/触摸板滚动缩放横轴；不按 Ctrl 时缩放纵轴。"
            }
            (CtrlWheelZoomX, Language::En) => {
                "Ctrl + mouse wheel / touchpad scroll zooms the horizontal axis; without Ctrl it zooms the vertical axis."
            }
            (AllDatasets, Language::Zh) => "所有数据组",
            (AllDatasets, Language::En) => "All Datasets",
            (CursorExportHint, Language::Zh) => "导出 X1-X2 光标区间数据。",
            (CursorExportHint, Language::En) => "Export data between X1 and X2.",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DatasetExportFormat {
    StandardCsv,
    DataCsv,
    Tsv,
    Json,
}

impl DatasetExportFormat {
    const ALL: [Self; 4] = [Self::StandardCsv, Self::DataCsv, Self::Tsv, Self::Json];

    fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::StandardCsv, Language::Zh) => "标准 CSV",
            (Self::DataCsv, Language::Zh) => "DATA CSV",
            (Self::Tsv, Language::Zh) => "TSV",
            (Self::Json, Language::Zh) => "JSON",
            (Self::StandardCsv, Language::En) => "Standard CSV",
            (Self::DataCsv, Language::En) => "DATA CSV",
            (Self::Tsv, Language::En) => "TSV",
            (Self::Json, Language::En) => "JSON",
        }
    }

    fn filter_name(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::StandardCsv, Language::Zh) => "CSV 文件",
            (Self::DataCsv, Language::Zh) => "DATA CSV 文件",
            (Self::Tsv, Language::Zh) => "TSV 文件",
            (Self::Json, Language::Zh) => "JSON 文件",
            (Self::StandardCsv, Language::En) => "CSV file",
            (Self::DataCsv, Language::En) => "DATA CSV file",
            (Self::Tsv, Language::En) => "TSV file",
            (Self::Json, Language::En) => "JSON file",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::StandardCsv | Self::DataCsv => "csv",
            Self::Tsv => "tsv",
            Self::Json => "json",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::StandardCsv => "",
            Self::DataCsv => "_data",
            Self::Tsv => "",
            Self::Json => "",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum ThemeMode {
    Light,
    Dark,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum ShortcutKey {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
}

impl ShortcutKey {
    const ALL: [ShortcutKey; 26] = [
        ShortcutKey::A,
        ShortcutKey::B,
        ShortcutKey::C,
        ShortcutKey::D,
        ShortcutKey::E,
        ShortcutKey::F,
        ShortcutKey::G,
        ShortcutKey::H,
        ShortcutKey::I,
        ShortcutKey::J,
        ShortcutKey::K,
        ShortcutKey::L,
        ShortcutKey::M,
        ShortcutKey::N,
        ShortcutKey::O,
        ShortcutKey::P,
        ShortcutKey::Q,
        ShortcutKey::R,
        ShortcutKey::S,
        ShortcutKey::T,
        ShortcutKey::U,
        ShortcutKey::V,
        ShortcutKey::W,
        ShortcutKey::X,
        ShortcutKey::Y,
        ShortcutKey::Z,
    ];

    fn label(self) -> &'static str {
        match self {
            ShortcutKey::A => "A",
            ShortcutKey::B => "B",
            ShortcutKey::C => "C",
            ShortcutKey::D => "D",
            ShortcutKey::E => "E",
            ShortcutKey::F => "F",
            ShortcutKey::G => "G",
            ShortcutKey::H => "H",
            ShortcutKey::I => "I",
            ShortcutKey::J => "J",
            ShortcutKey::K => "K",
            ShortcutKey::L => "L",
            ShortcutKey::M => "M",
            ShortcutKey::N => "N",
            ShortcutKey::O => "O",
            ShortcutKey::P => "P",
            ShortcutKey::Q => "Q",
            ShortcutKey::R => "R",
            ShortcutKey::S => "S",
            ShortcutKey::T => "T",
            ShortcutKey::U => "U",
            ShortcutKey::V => "V",
            ShortcutKey::W => "W",
            ShortcutKey::X => "X",
            ShortcutKey::Y => "Y",
            ShortcutKey::Z => "Z",
        }
    }

    fn egui_key(self) -> egui::Key {
        match self {
            ShortcutKey::A => egui::Key::A,
            ShortcutKey::B => egui::Key::B,
            ShortcutKey::C => egui::Key::C,
            ShortcutKey::D => egui::Key::D,
            ShortcutKey::E => egui::Key::E,
            ShortcutKey::F => egui::Key::F,
            ShortcutKey::G => egui::Key::G,
            ShortcutKey::H => egui::Key::H,
            ShortcutKey::I => egui::Key::I,
            ShortcutKey::J => egui::Key::J,
            ShortcutKey::K => egui::Key::K,
            ShortcutKey::L => egui::Key::L,
            ShortcutKey::M => egui::Key::M,
            ShortcutKey::N => egui::Key::N,
            ShortcutKey::O => egui::Key::O,
            ShortcutKey::P => egui::Key::P,
            ShortcutKey::Q => egui::Key::Q,
            ShortcutKey::R => egui::Key::R,
            ShortcutKey::S => egui::Key::S,
            ShortcutKey::T => egui::Key::T,
            ShortcutKey::U => egui::Key::U,
            ShortcutKey::V => egui::Key::V,
            ShortcutKey::W => egui::Key::W,
            ShortcutKey::X => egui::Key::X,
            ShortcutKey::Y => egui::Key::Y,
            ShortcutKey::Z => egui::Key::Z,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct ShortcutBinding {
    ctrl: bool,
    #[serde(default)]
    alt: bool,
    key: ShortcutKey,
}

impl ShortcutBinding {
    fn new(ctrl: bool, key: ShortcutKey) -> Self {
        Self {
            ctrl,
            alt: false,
            key,
        }
    }

    fn with_alt(ctrl: bool, alt: bool, key: ShortcutKey) -> Self {
        Self { ctrl, alt, key }
    }

    fn label(self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        parts.push(self.key.label());
        parts.join("+")
    }

    fn pressed(self, input: &egui::InputState) -> bool {
        input.modifiers.ctrl == self.ctrl
            && input.modifiers.alt == self.alt
            && input.key_pressed(self.key.egui_key())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct ShortcutConfig {
    #[serde(default = "default_reset_view_shortcut")]
    reset_view: ShortcutBinding,
    #[serde(default = "default_fit_cursors_shortcut")]
    fit_cursors: ShortcutBinding,
    #[serde(default = "default_toggle_cursors_shortcut")]
    toggle_cursors: ShortcutBinding,
    #[serde(default = "default_select_all_shortcut")]
    select_all: ShortcutBinding,
    #[serde(default = "default_select_none_shortcut")]
    select_none: ShortcutBinding,
    #[serde(default = "default_toggle_channel_panel_shortcut")]
    toggle_channel_panel: ShortcutBinding,
    #[serde(default = "default_toggle_analysis_panel_shortcut")]
    toggle_analysis_panel: ShortcutBinding,
}

impl Default for ShortcutConfig {
    fn default() -> Self {
        Self {
            reset_view: default_reset_view_shortcut(),
            fit_cursors: default_fit_cursors_shortcut(),
            toggle_cursors: default_toggle_cursors_shortcut(),
            select_all: default_select_all_shortcut(),
            select_none: default_select_none_shortcut(),
            toggle_channel_panel: default_toggle_channel_panel_shortcut(),
            toggle_analysis_panel: default_toggle_analysis_panel_shortcut(),
        }
    }
}

fn default_reset_view_shortcut() -> ShortcutBinding {
    ShortcutBinding::new(false, ShortcutKey::R)
}

fn default_fit_cursors_shortcut() -> ShortcutBinding {
    ShortcutBinding::new(false, ShortcutKey::F)
}

fn default_toggle_cursors_shortcut() -> ShortcutBinding {
    ShortcutBinding::new(false, ShortcutKey::H)
}

fn default_select_all_shortcut() -> ShortcutBinding {
    ShortcutBinding::new(true, ShortcutKey::A)
}

fn default_select_none_shortcut() -> ShortcutBinding {
    ShortcutBinding::new(true, ShortcutKey::D)
}

fn default_toggle_channel_panel_shortcut() -> ShortcutBinding {
    ShortcutBinding::new(true, ShortcutKey::B)
}

fn default_toggle_analysis_panel_shortcut() -> ShortcutBinding {
    ShortcutBinding::with_alt(true, true, ShortcutKey::B)
}

fn default_shortcuts() -> ShortcutConfig {
    ShortcutConfig::default()
}

fn default_wheel_zoom_sensitivity() -> f64 {
    DEFAULT_WHEEL_ZOOM_SENSITIVITY
}

fn default_pll_sync_source() -> PllSyncSource {
    PllSyncSource::Voltage
}

fn default_three_phase_channels() -> [usize; 3] {
    [0, 1, 2]
}

fn default_export_arrow_size() -> f32 {
    DEFAULT_EXPORT_ARROW_SIZE
}

fn default_export_arrow_color_style() -> ExportArrowColorStyle {
    ExportArrowColorStyle::Curve
}

fn default_export_style_preset() -> ExportStylePreset {
    ExportStylePreset::Screenshot
}

fn default_export_pane_scope() -> ExportPaneScope {
    ExportPaneScope::All
}

fn default_export_time_range_mode() -> ExportTimeRangeMode {
    ExportTimeRangeMode::View
}

fn default_export_arrow_line_style() -> ExportArrowLineStyle {
    ExportArrowLineStyle::Solid
}

fn default_export_manual_arrow_head_enabled() -> bool {
    true
}

fn default_export_arrow_custom_color() -> [u8; 4] {
    Color32::BLACK.to_array()
}

fn default_export_label_scale() -> i32 {
    DEFAULT_EXPORT_LABEL_SCALE
}

fn default_export_label_font_style() -> ExportLabelFontStyle {
    ExportLabelFontStyle::Regular
}

fn default_export_resolution() -> ExportResolution {
    DEFAULT_EXPORT_RESOLUTION
}

fn default_export_dpi() -> ExportDpi {
    ExportDpi::Dpi300
}

fn default_export_dpi_value() -> u32 {
    300
}

fn default_export_image_format() -> ExportImageFormat {
    ExportImageFormat::Png
}

fn default_export_annotations_visible() -> bool {
    true
}

fn default_export_auto_variable_labels_enabled() -> bool {
    false
}

fn default_export_cursor_table_enabled() -> bool {
    true
}

fn default_primary_line_pattern() -> ChannelLinePattern {
    ChannelLinePattern::Solid
}

fn default_imported_line_pattern() -> ChannelLinePattern {
    ChannelLinePattern::Dashed
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct NamesConfig {
    #[serde(default)]
    display_names: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ConfigSection {
    Names,
    Display,
    Shortcut,
    Dataset,
}

impl ConfigSection {
    fn type_name(self) -> &'static str {
        match self {
            Self::Names => "names",
            Self::Display => "display",
            Self::Shortcut => "shortcuts",
            Self::Dataset => "datasets",
        }
    }

    fn mismatch_message(self) -> String {
        format!("配置类型不匹配，请选择{}配置文件。", self.zh_label())
    }

    fn zh_label(self) -> &'static str {
        match self {
            Self::Names => "变量名",
            Self::Display => "显示",
            Self::Shortcut => "快捷键",
            Self::Dataset => "数据组",
        }
    }

    fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::Names, Language::Zh) => "变量名配置",
            (Self::Names, Language::En) => "Name Settings",
            (Self::Display, Language::Zh) => "显示配置",
            (Self::Display, Language::En) => "Display Settings",
            (Self::Shortcut, Language::Zh) => "快捷键配置",
            (Self::Shortcut, Language::En) => "Shortcut Settings",
            (Self::Dataset, Language::Zh) => "数据组配置",
            (Self::Dataset, Language::En) => "Dataset Settings",
        }
    }

    fn sentence_label(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::Names, Language::Zh) => "变量名",
            (Self::Names, Language::En) => "Name settings",
            (Self::Display, Language::Zh) => "显示",
            (Self::Display, Language::En) => "Display settings",
            (Self::Shortcut, Language::Zh) => "快捷键",
            (Self::Shortcut, Language::En) => "Shortcut settings",
            (Self::Dataset, Language::Zh) => "数据组",
            (Self::Dataset, Language::En) => "Dataset settings",
        }
    }

    fn default_file_name(self) -> &'static str {
        match self {
            Self::Names => "scope-names.json",
            Self::Display => "scope-display.json",
            Self::Shortcut => "scope-shortcuts.json",
            Self::Dataset => "scope-datasets.json",
        }
    }

    fn recent_file_name(self) -> &'static str {
        match self {
            Self::Names => "scope-recent-names.json",
            Self::Display => "scope-recent-display-configs.json",
            Self::Shortcut => "scope-recent-shortcut-configs.json",
            Self::Dataset => "scope-recent-dataset-configs.json",
        }
    }

    fn exported_message(self, language: Language) -> String {
        match language {
            Language::Zh => format!("已导出{}配置。", self.sentence_label(language)),
            Language::En => format!("{} exported.", self.sentence_label(language)),
        }
    }

    fn imported_message(self, language: Language) -> String {
        match language {
            Language::Zh => format!("已导入{}配置。", self.sentence_label(language)),
            Language::En => format!("{} imported.", self.sentence_label(language)),
        }
    }
}

#[derive(Serialize)]
struct TaggedConfigRef<'a, T> {
    scope_config_type: &'static str,
    scope_config_version: u32,
    config: &'a T,
}

#[derive(Deserialize)]
struct TaggedConfig<T> {
    scope_config_type: String,
    #[allow(dead_code)]
    scope_config_version: u32,
    config: T,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DisplayConfig {
    #[serde(default)]
    channel_colors: Vec<[u8; 4]>,
    #[serde(default)]
    line_widths: Vec<f32>,
    #[serde(default)]
    line_patterns: Vec<ChannelLinePattern>,
    #[serde(default)]
    channel_scales: Vec<f32>,
    #[serde(default)]
    channel_panes: Vec<usize>,
    #[serde(default)]
    derived_visible: Vec<bool>,
    #[serde(default)]
    derived_colors: Vec<[u8; 4]>,
    #[serde(default)]
    derived_line_patterns: Vec<ChannelLinePattern>,
    #[serde(default)]
    derived_panes: Vec<usize>,
    #[serde(default = "default_pll_sync_source")]
    pll_sync_source: PllSyncSource,
    #[serde(default = "default_three_phase_channels")]
    pll_source_channels: [usize; 3],
    #[serde(default = "default_three_phase_channels")]
    dq_source_channels: [usize; 3],
    #[serde(default = "default_three_phase_channels")]
    time_sync_source_channels: [usize; 3],
    #[serde(default)]
    fft_channel: usize,
    #[serde(default = "default_wheel_zoom_sensitivity")]
    wheel_zoom_sensitivity: f64,
    #[serde(default = "default_sample_rate_hz")]
    sample_rate_hz: f64,
    #[serde(default = "default_harmonic_base_hz")]
    harmonic_base_hz: f64,
    #[serde(default = "default_scope_layout_rows")]
    scope_layout_rows: usize,
    #[serde(default = "default_scope_layout_cols")]
    scope_layout_cols: usize,
    #[serde(default = "default_language")]
    language: Language,
    #[serde(default = "default_theme_mode")]
    theme_mode: ThemeMode,
    #[serde(default = "default_export_arrow_size")]
    export_arrow_size: f32,
    #[serde(default = "default_export_arrow_color_style")]
    export_arrow_color_style: ExportArrowColorStyle,
    #[serde(default = "default_export_style_preset")]
    export_style_preset: ExportStylePreset,
    #[serde(default = "default_export_pane_scope")]
    export_pane_scope: ExportPaneScope,
    #[serde(default = "default_export_time_range_mode")]
    export_time_range_mode: ExportTimeRangeMode,
    #[serde(default)]
    export_manual_start: f64,
    #[serde(default)]
    export_manual_end: f64,
    #[serde(default = "default_export_arrow_line_style")]
    export_arrow_line_style: ExportArrowLineStyle,
    #[serde(default = "default_export_manual_arrow_head_enabled")]
    export_manual_arrow_head_enabled: bool,
    #[serde(default = "default_export_arrow_custom_color")]
    export_arrow_custom_color: [u8; 4],
    #[serde(default = "default_export_label_scale")]
    export_label_scale: i32,
    #[serde(default = "default_export_label_font_style")]
    export_label_font_style: ExportLabelFontStyle,
    #[serde(default = "default_export_resolution")]
    export_resolution: ExportResolution,
    #[serde(default = "default_export_dpi")]
    export_dpi: ExportDpi,
    #[serde(default = "default_export_dpi_value")]
    export_dpi_value: u32,
    #[serde(default = "default_export_image_format")]
    export_image_format: ExportImageFormat,
    #[serde(default = "default_export_annotations_visible")]
    export_annotations_visible: bool,
    #[serde(default = "default_export_auto_variable_labels_enabled")]
    export_auto_variable_labels_enabled: bool,
    #[serde(default = "default_export_cursor_table_enabled")]
    export_cursor_table_enabled: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            channel_colors: Vec::new(),
            line_widths: Vec::new(),
            line_patterns: Vec::new(),
            channel_scales: Vec::new(),
            channel_panes: Vec::new(),
            derived_visible: Vec::new(),
            derived_colors: Vec::new(),
            derived_line_patterns: Vec::new(),
            derived_panes: Vec::new(),
            pll_sync_source: default_pll_sync_source(),
            pll_source_channels: default_three_phase_channels(),
            dq_source_channels: default_three_phase_channels(),
            time_sync_source_channels: default_three_phase_channels(),
            fft_channel: 0,
            wheel_zoom_sensitivity: default_wheel_zoom_sensitivity(),
            sample_rate_hz: default_sample_rate_hz(),
            harmonic_base_hz: default_harmonic_base_hz(),
            scope_layout_rows: default_scope_layout_rows(),
            scope_layout_cols: default_scope_layout_cols(),
            language: default_language(),
            theme_mode: default_theme_mode(),
            export_arrow_size: default_export_arrow_size(),
            export_arrow_color_style: default_export_arrow_color_style(),
            export_style_preset: default_export_style_preset(),
            export_pane_scope: default_export_pane_scope(),
            export_time_range_mode: default_export_time_range_mode(),
            export_manual_start: 0.0,
            export_manual_end: 0.0,
            export_arrow_line_style: default_export_arrow_line_style(),
            export_manual_arrow_head_enabled: default_export_manual_arrow_head_enabled(),
            export_arrow_custom_color: default_export_arrow_custom_color(),
            export_label_scale: default_export_label_scale(),
            export_label_font_style: default_export_label_font_style(),
            export_resolution: default_export_resolution(),
            export_dpi: default_export_dpi(),
            export_dpi_value: default_export_dpi_value(),
            export_image_format: default_export_image_format(),
            export_annotations_visible: default_export_annotations_visible(),
            export_auto_variable_labels_enabled: default_export_auto_variable_labels_enabled(),
            export_cursor_table_enabled: default_export_cursor_table_enabled(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DatasetConfig {
    #[serde(default)]
    primary_dataset_name: String,
    #[serde(default)]
    primary_visible: Vec<bool>,
    #[serde(default = "default_primary_line_pattern")]
    primary_line_pattern: ChannelLinePattern,
    #[serde(default)]
    sync_time_axes: bool,
    #[serde(default = "default_three_phase_channels")]
    time_sync_source_channels: [usize; 3],
    #[serde(default)]
    imported: Vec<DatasetGroupConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DatasetGroupConfig {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    visible: Vec<bool>,
    #[serde(default = "default_imported_line_pattern")]
    line_pattern: ChannelLinePattern,
    #[serde(default)]
    time_offset: f64,
}

#[derive(Clone, Debug)]
struct RuntimeConfig {
    display_names: Vec<String>,
    visible: Vec<bool>,
    channel_colors: Vec<[u8; 4]>,
    line_widths: Vec<f32>,
    line_patterns: Vec<ChannelLinePattern>,
    channel_scales: Vec<f32>,
    channel_panes: Vec<usize>,
    derived_visible: Vec<bool>,
    derived_colors: Vec<[u8; 4]>,
    derived_line_patterns: Vec<ChannelLinePattern>,
    derived_panes: Vec<usize>,
    pll_sync_source: PllSyncSource,
    pll_source_channels: [usize; 3],
    dq_source_channels: [usize; 3],
    time_sync_source_channels: [usize; 3],
    fft_channel: usize,
    wheel_zoom_sensitivity: f64,
    sample_rate_hz: f64,
    harmonic_base_hz: f64,
    scope_layout_rows: usize,
    scope_layout_cols: usize,
    language: Language,
    theme_mode: ThemeMode,
    shortcuts: ShortcutConfig,
    export_arrow_size: f32,
    export_arrow_color_style: ExportArrowColorStyle,
    export_style_preset: ExportStylePreset,
    export_pane_scope: ExportPaneScope,
    export_time_range_mode: ExportTimeRangeMode,
    export_manual_start: f64,
    export_manual_end: f64,
    export_arrow_line_style: ExportArrowLineStyle,
    export_manual_arrow_head_enabled: bool,
    export_arrow_custom_color: [u8; 4],
    export_label_scale: i32,
    export_label_font_style: ExportLabelFontStyle,
    export_resolution: ExportResolution,
    export_dpi: ExportDpi,
    export_dpi_value: u32,
    export_image_format: ExportImageFormat,
    export_annotations_visible: bool,
    export_auto_variable_labels_enabled: bool,
    export_cursor_table_enabled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct RecentFiles {
    paths: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
struct AutoMeasurement {
    first: f32,
    last: f32,
    min: f32,
    max: f32,
}

#[derive(Clone, Debug)]
struct MeasurementCache {
    dataset_index: usize,
    start: f64,
    end: f64,
    channels: Vec<usize>,
    rows: Vec<(usize, AutoMeasurement)>,
}

#[derive(Clone, Debug)]
struct SequenceCache {
    dataset_index: usize,
    start: f64,
    end: f64,
    channels: [usize; 3],
    result: Result<SequenceResult, String>,
}

#[derive(Clone, Debug)]
struct DerivedCurveCache {
    dataset_index: usize,
    start: f64,
    end: f64,
    pll_channels: [usize; 3],
    dq_channels: [usize; 3],
    channels: Vec<usize>,
    repid_signatures: Vec<crate::repid_derived::RepidCurveSignature>,
}

#[derive(Clone, Debug)]
struct DerivedMeasurementCache {
    dataset_index: usize,
    start: f64,
    end: f64,
    pll_channels: [usize; 3],
    dq_channels: [usize; 3],
    repid_signatures: Vec<crate::repid_derived::RepidCurveSignature>,
    channels: Vec<usize>,
    rows: Vec<(usize, AutoMeasurement)>,
}

#[derive(Clone, Debug, PartialEq)]
struct PaneYBoundsCacheKey {
    pane_count: usize,
    selections: Vec<PanePlotSelections>,
    primary_plot_key: Option<PlotCacheKey>,
    imported_plot_keys: Vec<Option<PlotCacheKey>>,
    derived_key: Option<DerivedJobKey>,
    global_y_override: Option<(u64, u64)>,
    pane_y_overrides: Vec<Option<(u64, u64)>>,
}

#[derive(Clone, Debug, PartialEq)]
struct PanePreparedPlotCacheKey {
    pane_count: usize,
    selections: Vec<PanePlotSelections>,
    primary_plot_key: Option<PlotCacheKey>,
    imported_plot_keys: Vec<Option<PlotCacheKey>>,
    derived_key: Option<DerivedJobKey>,
}

#[derive(Clone, Default)]
struct PanePreparedImportedDataset {
    samples: PreparedPlotSeries,
    summary: Option<PreparedPlotSeries>,
}

#[derive(Clone, Default)]
struct PanePreparedPlotPane {
    primary_samples: PreparedPlotSeries,
    primary_summary: Option<PreparedPlotSeries>,
    imported: Vec<PanePreparedImportedDataset>,
    derived: PreparedPlotSeries,
}

#[derive(Clone, Default)]
struct PanePreparedPlotCache {
    panes: Vec<PanePreparedPlotPane>,
}

#[derive(Clone, Debug, PartialEq)]
struct MeasurementJobKey {
    generation: u64,
    dataset_index: usize,
    start: f64,
    end: f64,
    channels: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
struct SequenceJobKey {
    generation: u64,
    dataset_index: usize,
    start: f64,
    end: f64,
    channels: [usize; 3],
}

#[derive(Clone, Debug, PartialEq)]
struct DerivedJobKey {
    generation: u64,
    dataset_index: usize,
    start: f64,
    end: f64,
    pll_channels: [usize; 3],
    dq_channels: [usize; 3],
    channels: Vec<usize>,
    repid_signatures: Vec<crate::repid_derived::RepidCurveSignature>,
}

struct ExportCurve<'a> {
    label_index: usize,
    label: String,
    color: Color32,
    width: i32,
    points: &'a [PlotPoint],
    low_points: Option<&'a [PlotPoint]>,
}

struct PendingExportPaneLabels<'a> {
    plot: ClipRect,
    canvas_bounds: ClipRect,
    curves: Vec<ExportCurve<'a>>,
    occupied_rects: Vec<[i32; 4]>,
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
}

struct ExportWaveformRenderScene {
    x_min: f64,
    x_max: f64,
    source_pane_count: usize,
    export_pane_count: usize,
    cols: usize,
    pane_indices: Vec<usize>,
    y_bounds: Vec<(f64, f64)>,
    resolution_scale: i32,
    width: usize,
    height: usize,
    margin: i32,
    gap: i32,
    pane_w: i32,
    pane_h: i32,
    bottom_reserved: i32,
    canvas_bounds: ClipRect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportRenderLayer {
    BaseWaveform,
    VariableLabels,
    ManualAnnotations,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportPreviewActionIcon {
    Save,
    Copy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportPreviewZoomMode {
    Fit,
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportAnnotationSelection {
    VariableLabel(usize),
    Text(usize),
    Arrow(usize),
    Rectangle(usize),
    Ink(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportAnnotationKeyboardAction {
    Delete,
    Nudge([i32; 2]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportCurveLabelSource {
    Primary(usize),
    Imported {
        dataset_index: usize,
        channel_index: usize,
    },
    Derived(usize),
}

#[derive(Clone)]
struct ExportCurveLabel {
    name: String,
    color: Color32,
    source: ExportCurveLabelSource,
}

#[derive(Clone, Debug)]
struct ExportLabelPlacement {
    label_index: usize,
    label_rect: [i32; 4],
    anchor_rect: [i32; 4],
    anchor_point: [i32; 2],
    plot_rect: ClipRect,
    drag_bounds: ClipRect,
}

#[derive(Clone, Debug)]
struct ExportPreviewDrag {
    label_index: usize,
    start_pos: [i32; 2],
    before_state: ExportPreviewEditState,
    undo_recorded: bool,
}

#[derive(Clone, Debug)]
struct ExportPreviewAnchorDrag {
    label_index: usize,
    before_state: ExportPreviewEditState,
    undo_recorded: bool,
}

#[derive(Clone, Debug)]
struct ExportPreviewTextDrag {
    text_index: usize,
    start_pos: [i32; 2],
    before_state: ExportPreviewEditState,
    undo_recorded: bool,
}

#[derive(Clone, Debug)]
struct ExportInkDrag {
    stroke_index: Option<usize>,
    before_state: ExportPreviewEditState,
    undo_recorded: bool,
}

#[derive(Clone, Debug)]
struct ExportArrowDrag {
    arrow_index: Option<usize>,
    before_state: ExportPreviewEditState,
    undo_recorded: bool,
}

#[derive(Clone, Debug)]
struct ExportRectangleDrag {
    rectangle_index: Option<usize>,
    start: CanvasPoint,
    before_state: ExportPreviewEditState,
    undo_recorded: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExportPreviewTool {
    Select,
    Text,
    Arrow,
    Rectangle,
    Brush,
    Eraser,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExportHistoryAction {
    Undo,
    Redo,
}

impl ShapeKind {
    const ALL: [Self; 2] = [Self::Rectangle, Self::Ellipse];

    fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::Rectangle, Language::Zh) => "矩形",
            (Self::Ellipse, Language::Zh) => "椭圆",
            (Self::Rectangle, Language::En) => "Rectangle",
            (Self::Ellipse, Language::En) => "Ellipse",
        }
    }
}

#[derive(Clone)]
struct ExportToolIconTextures {
    select: egui::TextureHandle,
    text: egui::TextureHandle,
    arrow: egui::TextureHandle,
    rectangle: egui::TextureHandle,
    brush: egui::TextureHandle,
    eraser: egui::TextureHandle,
    undo: egui::TextureHandle,
    redo: egui::TextureHandle,
}

#[derive(Clone)]
struct BatchExportTimeWindow {
    enabled: bool,
    start: f64,
    end: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BatchExportDatasetMode {
    Combined,
    EachDataset,
}

impl BatchExportDatasetMode {
    const ALL: [Self; 2] = [Self::Combined, Self::EachDataset];

    fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::Combined, Language::Zh) => "合并当前选择",
            (Self::Combined, Language::En) => "Current selection together",
            (Self::EachDataset, Language::Zh) => "按数据组拆分",
            (Self::EachDataset, Language::En) => "One image per dataset",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BatchExportPaneMode {
    Current,
    AllPanes,
    EachPane,
}

impl BatchExportPaneMode {
    const ALL: [Self; 3] = [Self::Current, Self::AllPanes, Self::EachPane];

    fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::Current, Language::Zh) => "使用当前导出子窗口设置",
            (Self::Current, Language::En) => "Use current pane setting",
            (Self::AllPanes, Language::Zh) => "所有子窗口合成一张",
            (Self::AllPanes, Language::En) => "All panes in one image",
            (Self::EachPane, Language::Zh) => "每个子窗口单独导出",
            (Self::EachPane, Language::En) => "One image per pane",
        }
    }
}

struct FftJobResult {
    generation: u64,
    result: Result<Vec<(usize, FftResult)>, String>,
}

struct MeasurementJobResult {
    generation: u64,
    dataset_index: usize,
    start: f64,
    end: f64,
    channels: Vec<usize>,
    result: Result<Vec<(usize, AutoMeasurement)>, String>,
}

struct SequenceJobResult {
    generation: u64,
    dataset_index: usize,
    start: f64,
    end: f64,
    channels: [usize; 3],
    result: Result<SequenceResult, String>,
}

struct DerivedCurveJobResult {
    generation: u64,
    dataset_index: usize,
    start: f64,
    end: f64,
    pll_channels: [usize; 3],
    dq_channels: [usize; 3],
    channels: Vec<usize>,
    repid_signatures: Vec<crate::repid_derived::RepidCurveSignature>,
    result: Result<SampleBlock, String>,
}

struct DerivedMeasurementJobResult {
    generation: u64,
    dataset_index: usize,
    start: f64,
    end: f64,
    pll_channels: [usize; 3],
    dq_channels: [usize; 3],
    repid_signatures: Vec<crate::repid_derived::RepidCurveSignature>,
    channels: Vec<usize>,
    result: Result<Vec<(usize, AutoMeasurement)>, String>,
}

struct OpenedDataset {
    source: Arc<dyn DataSource>,
    path: PathBuf,
    kind: SourceKind,
}

struct ImportJobResult {
    generation: u64,
    replace_primary: bool,
    opened: Vec<OpenedDataset>,
    errors: Vec<String>,
}

struct SummaryEnvelopeSeries {
    high: Arc<[PlotPoint]>,
    low: Arc<[PlotPoint]>,
    bounds: Option<(f64, f64)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CursorId {
    A,
    B,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SourceKind {
    Cloud,
    Dat,
    Local,
    Scope,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LocalCsvRole {
    Analog,
    Digital,
}

#[derive(Clone, Debug)]
struct LocalCsvPairInfo {
    role: LocalCsvRole,
    key: String,
    filename_timestamp: Option<String>,
    modified_millis: Option<i128>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum PllSyncSource {
    Voltage,
    Current,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum ExportArrowColorStyle {
    Curve,
    Dark,
    Red,
    Blue,
    Custom,
}

impl ExportArrowColorStyle {
    const ALL: [Self; 5] = [Self::Curve, Self::Dark, Self::Red, Self::Blue, Self::Custom];

    fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::Curve, Language::Zh) => "跟随曲线",
            (Self::Dark, Language::Zh) => "深色统一",
            (Self::Red, Language::Zh) => "红色统一",
            (Self::Blue, Language::Zh) => "蓝色统一",
            (Self::Custom, Language::Zh) => "自定义",
            (Self::Curve, Language::En) => "Match curve",
            (Self::Dark, Language::En) => "Dark",
            (Self::Red, Language::En) => "Red",
            (Self::Blue, Language::En) => "Blue",
            (Self::Custom, Language::En) => "Custom",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum ExportStylePreset {
    ReportWhite,
    PaperMono,
    Screenshot,
    HighContrastPrint,
}

#[derive(Clone, Copy)]
struct ExportStylePalette {
    canvas_bg: Rgba,
    plot_bg: Rgba,
    grid: Rgba,
    border: Rgba,
    axis_text: Rgba,
    label_bg: Rgba,
    cursor_label_bg: Rgba,
    line_width_scale: f32,
}

impl ExportStylePreset {
    fn palette(self) -> ExportStylePalette {
        match self {
            Self::ReportWhite => ExportStylePalette {
                canvas_bg: Rgba::rgb(255, 255, 255),
                plot_bg: Rgba::rgb(255, 255, 255),
                grid: Rgba::rgb(211, 224, 240),
                border: Rgba::rgb(28, 43, 64),
                axis_text: Rgba::rgb(18, 31, 50),
                label_bg: Rgba::rgba(255, 255, 255, 230),
                cursor_label_bg: Rgba::rgba(255, 255, 255, 238),
                line_width_scale: 1.0,
            },
            Self::PaperMono => ExportStylePalette {
                canvas_bg: Rgba::rgb(255, 255, 255),
                plot_bg: Rgba::rgb(255, 255, 255),
                grid: Rgba::rgb(222, 222, 222),
                border: Rgba::rgb(0, 0, 0),
                axis_text: Rgba::rgb(0, 0, 0),
                label_bg: Rgba::rgba(255, 255, 255, 245),
                cursor_label_bg: Rgba::rgba(255, 255, 255, 245),
                line_width_scale: 0.9,
            },
            Self::Screenshot => ExportStylePalette {
                canvas_bg: Rgba::rgb(248, 251, 255),
                plot_bg: Rgba::rgb(252, 254, 255),
                grid: Rgba::rgb(206, 220, 238),
                border: Rgba::rgb(42, 58, 80),
                axis_text: Rgba::rgb(24, 36, 56),
                label_bg: Rgba::rgba(255, 255, 255, 224),
                cursor_label_bg: Rgba::rgba(255, 255, 255, 232),
                line_width_scale: 1.0,
            },
            Self::HighContrastPrint => ExportStylePalette {
                canvas_bg: Rgba::rgb(255, 255, 255),
                plot_bg: Rgba::rgb(255, 255, 255),
                grid: Rgba::rgb(180, 180, 180),
                border: Rgba::rgb(0, 0, 0),
                axis_text: Rgba::rgb(0, 0, 0),
                label_bg: Rgba::rgba(255, 255, 255, 248),
                cursor_label_bg: Rgba::rgba(255, 255, 255, 248),
                line_width_scale: 1.25,
            },
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum ExportPaneScope {
    All,
    Active,
}

impl ExportPaneScope {
    const ALL: [Self; 2] = [Self::All, Self::Active];

    fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::All, Language::Zh) => "全部子窗口",
            (Self::Active, Language::Zh) => "当前子窗口",
            (Self::All, Language::En) => "All panes",
            (Self::Active, Language::En) => "Active pane",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum ExportTimeRangeMode {
    View,
    Cursor,
    Manual,
}

impl ExportTimeRangeMode {
    const ALL: [Self; 3] = [Self::View, Self::Cursor, Self::Manual];

    fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::View, Language::Zh) => "当前视图",
            (Self::Cursor, Language::Zh) => "光标 X1-X2",
            (Self::Manual, Language::Zh) => "手动范围",
            (Self::View, Language::En) => "Current view",
            (Self::Cursor, Language::En) => "Cursor X1-X2",
            (Self::Manual, Language::En) => "Manual range",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum ExportArrowLineStyle {
    Solid,
    Dashed,
    Dotted,
    Thick,
    Double,
}

impl ExportArrowLineStyle {
    const BASE: [Self; 3] = [Self::Solid, Self::Dashed, Self::Dotted];
    const TOOLBAR: [Self; 5] = [
        Self::Solid,
        Self::Dashed,
        Self::Dotted,
        Self::Thick,
        Self::Double,
    ];

    fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::Solid, Language::Zh) => "箭头实线",
            (Self::Dashed, Language::Zh) => "箭头虚线",
            (Self::Dotted, Language::Zh) => "箭头点线",
            (Self::Thick, Language::Zh) => "➜ 粗箭头",
            (Self::Double, Language::Zh) => "⇒ 双线箭头",
            (Self::Solid, Language::En) => "→ Solid arrow",
            (Self::Dashed, Language::En) => "⇢ Dashed arrow",
            (Self::Dotted, Language::En) => "⋯→ Dotted arrow",
            (Self::Thick, Language::En) => "➜ Thick arrow",
            (Self::Double, Language::En) => "⇒ Double arrow",
        }
    }

    fn base_style(self) -> Self {
        match self {
            Self::Dashed => Self::Dashed,
            Self::Dotted => Self::Dotted,
            Self::Solid | Self::Thick | Self::Double => Self::Solid,
        }
    }

    fn base_label(self, language: Language) -> &'static str {
        match (self.base_style(), language) {
            (Self::Solid, Language::Zh) => "箭头实线",
            (Self::Dashed, Language::Zh) => "箭头虚线",
            (Self::Dotted, Language::Zh) => "箭头点线",
            (Self::Solid, Language::En) => "Solid arrow",
            (Self::Dashed, Language::En) => "Dashed arrow",
            (Self::Dotted, Language::En) => "Dotted arrow",
            _ => self.label(language),
        }
    }

    fn stroke_style(self) -> StrokeStyle {
        match self {
            Self::Solid | Self::Thick | Self::Double => StrokeStyle::Solid,
            Self::Dashed => StrokeStyle::Dashed,
            Self::Dotted => StrokeStyle::Dotted,
        }
    }

    fn width_extra(self) -> i32 {
        match self {
            Self::Thick => 2,
            Self::Double => 1,
            Self::Solid | Self::Dashed | Self::Dotted => 0,
        }
    }

    fn head_scale(self) -> f32 {
        match self {
            Self::Thick => 1.18,
            Self::Double => 1.06,
            Self::Solid | Self::Dashed | Self::Dotted => 1.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum ChannelLinePattern {
    Solid,
    Dashed,
    DashedShort,
    DashedLong,
    Dotted,
    DottedDense,
    DottedLoose,
}

impl ChannelLinePattern {
    const ALL: [ChannelLinePattern; 7] = [
        ChannelLinePattern::Solid,
        ChannelLinePattern::Dashed,
        ChannelLinePattern::DashedShort,
        ChannelLinePattern::DashedLong,
        ChannelLinePattern::Dotted,
        ChannelLinePattern::DottedDense,
        ChannelLinePattern::DottedLoose,
    ];

    fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (ChannelLinePattern::Solid, Language::Zh) => "实线",
            (ChannelLinePattern::Dashed, Language::Zh) => "虚线",
            (ChannelLinePattern::DashedShort, Language::Zh) => "短虚线",
            (ChannelLinePattern::DashedLong, Language::Zh) => "长虚线",
            (ChannelLinePattern::Dotted, Language::Zh) => "点线",
            (ChannelLinePattern::DottedDense, Language::Zh) => "密点线",
            (ChannelLinePattern::DottedLoose, Language::Zh) => "疏点线",
            (ChannelLinePattern::Solid, Language::En) => "Solid",
            (ChannelLinePattern::Dashed, Language::En) => "Dashed",
            (ChannelLinePattern::DashedShort, Language::En) => "Short dashed",
            (ChannelLinePattern::DashedLong, Language::En) => "Long dashed",
            (ChannelLinePattern::Dotted, Language::En) => "Dotted",
            (ChannelLinePattern::DottedDense, Language::En) => "Dense dotted",
            (ChannelLinePattern::DottedLoose, Language::En) => "Loose dotted",
        }
    }

    fn plot_style(self) -> LineStyle {
        match self {
            ChannelLinePattern::Solid => LineStyle::Solid,
            ChannelLinePattern::Dashed => LineStyle::Dashed { length: 8.0 },
            ChannelLinePattern::DashedShort => LineStyle::Dashed { length: 4.0 },
            ChannelLinePattern::DashedLong => LineStyle::Dashed { length: 14.0 },
            ChannelLinePattern::Dotted => LineStyle::Dotted { spacing: 5.0 },
            ChannelLinePattern::DottedDense => LineStyle::Dotted { spacing: 3.0 },
            ChannelLinePattern::DottedLoose => LineStyle::Dotted { spacing: 10.0 },
        }
    }
}

impl Language {
    fn label(self) -> &'static str {
        match self {
            Language::Zh => "中文",
            Language::En => "English",
        }
    }
}

impl ThemeMode {
    fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (ThemeMode::Light, Language::Zh) => "浅色",
            (ThemeMode::Dark, Language::Zh) => "深色",
            (ThemeMode::Light, Language::En) => "Light",
            (ThemeMode::Dark, Language::En) => "Dark",
        }
    }

    fn visuals(self) -> egui::Visuals {
        match self {
            ThemeMode::Light => egui::Visuals::light(),
            ThemeMode::Dark => egui::Visuals::dark(),
        }
    }
}

impl ScopeApp {
    fn default_live_state() -> scope_analyzer::live::state::LiveScopeState {
        scope_analyzer::live::state::LiveScopeState::default()
    }

    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::install_panic_hook();
        Self::install_cjk_fonts(&cc.egui_ctx);

        let recent_files = Self::load_recent_files();
        let recent_name_configs = Self::load_recent_configs(ConfigSection::Names);
        let recent_display_configs = Self::load_recent_configs(ConfigSection::Display);
        let recent_shortcut_configs = Self::load_recent_configs(ConfigSection::Shortcut);
        let recent_dataset_configs = Self::load_recent_configs(ConfigSection::Dataset);
        Self {
            live: Self::default_live_state(),
            source: None,
            source_kind: None,
            imported_datasets: Vec::new(),
            primary_selected_for_delete: false,
            primary_dataset_name: String::new(),
            visible: Vec::new(),
            display_names: Vec::new(),
            editing_display_name: None,
            pending_display_name_focus: None,
            channel_colors: Vec::new(),
            line_widths: Vec::new(),
            line_patterns: Vec::new(),
            channel_scales: Vec::new(),
            channel_panes: Vec::new(),
            derived_visible: vec![false; DERIVED_CHANNEL_COUNT],
            derived_colors: (0..DERIVED_CHANNEL_COUNT)
                .map(Self::default_derived_color)
                .collect(),
            derived_line_patterns: vec![ChannelLinePattern::Solid; DERIVED_CHANNEL_COUNT],
            derived_panes: vec![0; DERIVED_CHANNEL_COUNT],
            repid_derived_curves: Vec::new(),
            pll_sync_source: PllSyncSource::Voltage,
            active_scope_pane: 0,
            hovered_channel: None,
            view_start: 0.0,
            view_end: 1.0,
            y_min: None,
            y_max: None,
            pane_y_bounds: Vec::new(),
            pane_y_bounds_cache_key: None,
            pane_y_bounds_cache: Vec::new(),
            pane_prepared_plot_cache_key: None,
            pane_prepared_plot_cache: PanePreparedPlotCache::default(),
            cursor_a: 0.25,
            cursor_b: 0.75,
            show_cursor_a: true,
            show_cursor_b: true,
            active_cursor: CursorId::A,
            channel_filter: String::new(),
            show_help: false,
            show_options: false,
            show_channel_panel: true,
            show_analysis_panel: true,
            show_export_preview: false,
            show_batch_export: false,
            export_preview_dirty: false,
            export_preview_texture: None,
            export_tool_icons: None,
            export_preview_size: [0, 0],
            export_preview_zoom_mode: ExportPreviewZoomMode::Fit,
            export_preview_zoom_scale: 1.0,
            export_preview_display_scale: 1.0,
            export_preview_error: None,
            export_label_overrides: Vec::new(),
            export_label_positions: Vec::new(),
            export_label_anchor_x: Vec::new(),
            export_preview_selections: None,
            export_preview_label_layout: Vec::new(),
            export_preview_drag: None,
            export_preview_anchor_drag: None,
            export_preview_text_drag: None,
            export_preview_edit_label_index: None,
            export_preview_edit_label_focus_pending: false,
            export_preview_edit_text_index: None,
            export_preview_edit_text_focus_pending: false,
            export_annotation_selection: None,
            export_preview_undo_stack: Vec::new(),
            export_preview_redo_stack: Vec::new(),
            export_text_annotations: Vec::new(),
            export_arrow_annotations: Vec::new(),
            export_rectangle_annotations: Vec::new(),
            batch_export_windows: Vec::new(),
            batch_export_dataset_mode: BatchExportDatasetMode::Combined,
            batch_export_pane_mode: BatchExportPaneMode::Current,
            batch_export_last_summary: None,
            export_preview_tool: ExportPreviewTool::Select,
            export_ink_strokes: Vec::new(),
            export_ink_drag: None,
            export_arrow_drag: None,
            export_rectangle_drag: None,
            export_brush_color: Color32::from_rgb(220, 20, 38),
            export_brush_width: 4,
            wheel_zoom_sensitivity: DEFAULT_WHEEL_ZOOM_SENSITIVITY,
            sample_rate_hz: default_sample_rate_hz(),
            harmonic_base_hz: default_harmonic_base_hz(),
            sync_time_axes: false,
            time_sync_status: String::new(),
            time_sync_source_channels: [0, 1, 2],
            time_sync_source_channels_user_selected: false,
            scope_layout_rows: default_scope_layout_rows(),
            scope_layout_cols: default_scope_layout_cols(),
            language: default_language(),
            theme_mode: default_theme_mode(),
            shortcuts: default_shortcuts(),
            export_arrow_size: DEFAULT_EXPORT_ARROW_SIZE,
            export_arrow_color_style: ExportArrowColorStyle::Curve,
            export_style_preset: ExportStylePreset::Screenshot,
            export_pane_scope: ExportPaneScope::All,
            export_time_range_mode: ExportTimeRangeMode::View,
            export_manual_start: 0.0,
            export_manual_end: 1.0,
            export_arrow_line_style: ExportArrowLineStyle::Solid,
            export_manual_arrow_head_enabled: true,
            export_arrow_custom_color: Color32::from_rgb(20, 96, 180),
            export_shape_kind: ShapeKind::Rectangle,
            export_label_scale: DEFAULT_EXPORT_LABEL_SCALE,
            export_label_font_style: ExportLabelFontStyle::Regular,
            export_resolution: DEFAULT_EXPORT_RESOLUTION,
            export_dpi: ExportDpi::Dpi300,
            export_dpi_value: 300,
            export_image_format: ExportImageFormat::Png,
            export_annotations_visible: true,
            export_auto_variable_labels_enabled: false,
            export_cursor_table_enabled: true,
            last_error: None,
            import_status: None,
            loaded_path: None,
            recent_files,
            recent_name_configs,
            recent_display_configs,
            recent_shortcut_configs,
            recent_dataset_configs,
            plot_cache_key: None,
            plot_cache: SampleBlock::default(),
            plot_summary: None,
            prepared_plot_cache: PreparedPlotSeries::default(),
            prepared_plot_summary: None,
            fft_results: Vec::new(),
            measurement_cache: None,
            sequence_cache: None,
            derived_curve_cache: None,
            prepared_derived_curve_cache: PreparedPlotSeries::default(),
            derived_measurement_cache: None,
            plot_worker: None,
            plot_cancel: None,
            compare_plot_worker: None,
            compare_plot_cancel: None,
            fft_worker: None,
            fft_cancel: None,
            measurement_worker: None,
            measurement_worker_key: None,
            measurement_cancel: None,
            sequence_worker: None,
            sequence_worker_key: None,
            sequence_cancel: None,
            derived_curve_worker: None,
            derived_curve_worker_key: None,
            derived_curve_cancel: None,
            derived_measurement_worker: None,
            derived_measurement_worker_key: None,
            derived_measurement_cancel: None,
            import_worker: None,
            import_cancel: None,
            data_generation: 0,
            fft_dataset_index: 0,
            fft_channel: 0,
            fft_channel_user_selected: false,
            sequence_channels: [0, 1, 2],
            sequence_channels_user_selected: false,
            pll_source_channels: [0, 1, 2],
            dq_source_channels: [0, 1, 2],
            dq_source_channels_user_selected: false,
            needs_fft_reload: false,
            needs_plot_reload: false,
            needs_compare_plot_reload: false,
            plot_reload_deferred_until: None,
            last_channel_panel_width: None,
            last_analysis_panel_width: None,
            layout_resize_active_until: None,
            needs_derived_reload: false,
            cursor_place_mode: None,
            zoom_box_start: None,
            zoom_box_current: None,
        }
    }

    fn bump_data_generation(&mut self) {
        self.cancel_plot_jobs();
        self.cancel_analysis_jobs();
        self.cancel_import_job();
        self.data_generation = self.data_generation.wrapping_add(1);
        self.plot_worker = None;
        self.plot_cancel = None;
        self.compare_plot_worker = None;
        self.compare_plot_cancel = None;
        self.plot_reload_deferred_until = None;
        self.fft_worker = None;
        self.fft_cancel = None;
        self.measurement_worker = None;
        self.measurement_worker_key = None;
        self.measurement_cancel = None;
        self.sequence_worker = None;
        self.sequence_worker_key = None;
        self.sequence_cancel = None;
        self.derived_curve_worker = None;
        self.derived_curve_worker_key = None;
        self.derived_curve_cancel = None;
        self.derived_measurement_worker = None;
        self.derived_measurement_worker_key = None;
        self.derived_measurement_cancel = None;
        self.import_worker = None;
        self.import_cancel = None;
        self.measurement_cache = None;
        self.sequence_cache = None;
        self.derived_curve_cache = None;
        self.prepared_derived_curve_cache = PreparedPlotSeries::default();
        self.derived_measurement_cache = None;
    }

    fn install_panic_hook() {
        static PANIC_HOOK: Once = Once::new();
        PANIC_HOOK.call_once(|| {
            panic::set_hook(Box::new(|info| {
                let message = Self::panic_payload_message(info.payload());
                let location = info
                    .location()
                    .map(|location| {
                        format!(
                            "{}:{}:{}",
                            location.file(),
                            location.line(),
                            location.column()
                        )
                    })
                    .unwrap_or_else(|| "unknown location".to_owned());
                Self::append_crash_log(&format!("panic at {location}: {message}"));
            }));
        });
    }

    fn crash_log_path() -> PathBuf {
        Self::app_home_dir().join("scope-crash.log")
    }

    fn startup_log_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        paths.push(Self::app_home_dir().join("ScopeAnalyzer-startup.log"));
        paths.push(env::temp_dir().join("ScopeAnalyzer-startup.log"));
        paths
    }

    fn log_directory() -> PathBuf {
        Self::app_home_dir()
    }

    fn app_home_dir() -> PathBuf {
        if let Some(path) = env::var_os(APP_HOME_ENV)
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
        {
            return path;
        }

        env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .or_else(|| env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn open_log_directory(&mut self) {
        let path = Self::log_directory();
        let result = if cfg!(target_os = "windows") {
            std::process::Command::new("explorer").arg(&path).spawn()
        } else if cfg!(target_os = "macos") {
            std::process::Command::new("open").arg(&path).spawn()
        } else {
            std::process::Command::new("xdg-open").arg(&path).spawn()
        };
        if let Err(error) = result {
            self.last_error = Some(match self.language {
                Language::Zh => format!("打开日志目录失败: {error}"),
                Language::En => format!("Failed to open log directory: {error}"),
            });
        }
    }

    fn copy_diagnostics_to_clipboard(&mut self, ctx: &egui::Context) {
        let diagnostics = self.diagnostic_info();
        ctx.output_mut(|output| {
            output.copied_text = diagnostics;
        });
        self.last_error = Some(match self.language {
            Language::Zh => "诊断信息已复制到剪贴板。".to_owned(),
            Language::En => "Diagnostics copied to clipboard.".to_owned(),
        });
    }

    fn log_file_description(path: &Path) -> String {
        match std::fs::metadata(path) {
            Ok(metadata) => format!("{} (exists, {} bytes)", path.display(), metadata.len()),
            Err(_) => format!("{} (missing)", path.display()),
        }
    }

    fn diagnostic_info(&self) -> String {
        let mut text = String::new();
        text.push_str("Scope Analyzer Diagnostics\n");
        text.push_str(&format!("version: {}\n", env!("CARGO_PKG_VERSION")));
        text.push_str(&format!(
            "exe: {}\n",
            env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|error| format!("unknown ({error})"))
        ));
        text.push_str(&format!("log_dir: {}\n", Self::log_directory().display()));
        text.push_str(&format!("app_home: {}\n", Self::app_home_dir().display()));
        text.push_str(&format!(
            "renderer_env: {}\n",
            env::var("SCOPE_RENDERER").unwrap_or_else(|_| "(unset)".to_owned())
        ));
        text.push_str(&format!(
            "renderer_child: {}\n",
            env::var("SCOPE_RENDERER_CHILD").unwrap_or_else(|_| "(unset)".to_owned())
        ));
        text.push_str(&format!("language: {:?}\n", self.language));
        text.push_str(&format!("theme: {:?}\n", self.theme_mode));
        text.push_str(&format!(
            "sample_rate_hz: {:.3}, harmonic_base_hz: {:.3}\n",
            self.sample_rate_hz, self.harmonic_base_hz
        ));
        text.push_str(&format!(
            "view: {:.9}..{:.9}, cursors: X1={:.9}, X2={:.9}, visible=({}, {})\n",
            self.view_start,
            self.view_end,
            self.cursor_a,
            self.cursor_b,
            self.show_cursor_a,
            self.show_cursor_b
        ));
        text.push_str(&format!(
            "layout: {}x{}, active_pane: {}\n",
            self.scope_layout_rows,
            self.scope_layout_cols,
            self.current_scope_pane() + 1
        ));
        text.push_str(&format!(
            "workers: plot={}, compare={}, fft={}, measurement={}, sequence={}, derived={}, import={}\n",
            self.plot_worker.is_some(),
            self.compare_plot_worker.is_some(),
            self.fft_worker.is_some(),
            self.measurement_worker.is_some(),
            self.sequence_worker.is_some(),
            self.derived_curve_worker.is_some() || self.derived_measurement_worker.is_some(),
            self.import_worker.is_some()
        ));
        if let Some(error) = &self.last_error {
            text.push_str(&format!("last_error: {}\n", error));
        }
        if let Some(status) = &self.import_status {
            text.push_str(&format!("import_status: {}\n", status));
        }
        if let Some(meta) = self.meta() {
            text.push_str(&format!(
                "primary: name={}, samples={}, duration={:.6}, data_hz={:.3}, channels={}, selected={}\n",
                meta.source_name,
                meta.sample_count,
                meta.duration(),
                meta.nominal_sample_rate_hz,
                meta.channels.len(),
                self.selected_channels().len()
            ));
        } else {
            text.push_str("primary: none\n");
        }
        text.push_str(&format!(
            "imported_dataset_count: {}\n",
            self.imported_datasets.len()
        ));
        for (index, dataset) in self.imported_datasets.iter().enumerate() {
            let meta = dataset.source.metadata();
            text.push_str(&format!(
                "  data{}: name={}, samples={}, duration={:.6}, data_hz={:.3}, channels={}, selected={}, offset={:.9}, kind={:?}\n",
                Self::dataset_letter(index + 1),
                dataset.display_name,
                meta.sample_count,
                meta.duration(),
                meta.nominal_sample_rate_hz,
                meta.channels.len(),
                self.selected_imported_channels(index).len(),
                dataset.time_offset,
                dataset.kind
            ));
        }
        text.push_str("logs:\n");
        text.push_str(&format!(
            "  crash: {}\n",
            Self::log_file_description(&Self::crash_log_path())
        ));
        for path in Self::startup_log_paths() {
            text.push_str(&format!(
                "  startup: {}\n",
                Self::log_file_description(&path)
            ));
        }
        text
    }

    fn append_crash_log(message: &str) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(Self::crash_log_path())
        {
            let _ = writeln!(file, "[{timestamp}] {message}");
        }
    }

    fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
        if let Some(message) = payload.downcast_ref::<&str>() {
            (*message).to_owned()
        } else if let Some(message) = payload.downcast_ref::<String>() {
            message.clone()
        } else {
            "non-string panic payload".to_owned()
        }
    }

    fn recover_worker_panic(
        worker_message: &'static str,
        payload: Box<dyn std::any::Any + Send>,
    ) -> String {
        let detail = Self::panic_payload_message(payload.as_ref());
        Self::append_crash_log(&format!("{worker_message}: {detail}"));
        worker_message.to_owned()
    }

    fn install_cjk_fonts(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();

        if let Some((font_name, font_data)) = Self::load_cjk_font() {
            fonts
                .font_data
                .insert(font_name.clone(), egui::FontData::from_owned(font_data));

            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push(font_name.clone());
            }
        }

        ctx.set_fonts(fonts);
    }

    fn load_cjk_font() -> Option<(String, Vec<u8>)> {
        Self::cjk_font_candidates().into_iter().find_map(|path| {
            fs::read(&path).ok().map(|font_data| {
                let file_name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "custom".to_owned());
                (format!("scope-cjk-{file_name}"), font_data)
            })
        })
    }

    fn cjk_font_candidates() -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(path) = env::var_os("SCOPE_CJK_FONT") {
            candidates.push(PathBuf::from(path));
        }

        let windows_dir = env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let fonts_dir = windows_dir.join("Fonts");
        candidates.extend(
            [
                "Deng.ttf",
                "simhei.ttf",
                "simsunb.ttf",
                "NotoSansSC-VF.ttf",
                "msyh.ttc",
                "simsun.ttc",
            ]
            .into_iter()
            .map(|file_name| fonts_dir.join(file_name)),
        );

        candidates.extend([
            PathBuf::from("/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
            PathBuf::from("/Library/Fonts/Arial Unicode.ttf"),
            PathBuf::from("/System/Library/Fonts/STHeiti Medium.ttc"),
            PathBuf::from("/System/Library/Fonts/STHeiti Light.ttc"),
            PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
            PathBuf::from("/usr/share/fonts/truetype/noto/NotoSansSC-Regular.otf"),
            PathBuf::from("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc"),
        ]);
        candidates
    }

    fn meta(&self) -> Option<&DatasetMeta> {
        self.source.as_ref().map(|source| source.metadata())
    }

    fn imported_meta(&self, index: usize) -> Option<&DatasetMeta> {
        self.imported_datasets
            .get(index)
            .map(|dataset| dataset.source.metadata())
    }

    fn default_dataset_name(path: &Path) -> String {
        path.file_stem()
            .or_else(|| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "dataset".to_owned())
    }

    fn dataset_letter(index: usize) -> String {
        const LETTERS: &[u8; 26] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        if index < LETTERS.len() {
            (LETTERS[index] as char).to_string()
        } else {
            format!("{}", index + 1)
        }
    }

    fn dataset_short_label(&self, index: usize) -> String {
        if self.language == Language::Zh {
            format!("数据{}", Self::dataset_letter(index))
        } else {
            format!("Data {}", Self::dataset_letter(index))
        }
    }

    fn dataset_label(&self, index: usize) -> String {
        let prefix = if self.language == Language::Zh {
            "数据"
        } else {
            "Data "
        };
        let name = if index == 0 {
            if self.primary_dataset_name.trim().is_empty() {
                self.meta()
                    .map(|meta| meta.source_name.clone())
                    .unwrap_or_else(|| "A".to_owned())
            } else {
                self.primary_dataset_name.clone()
            }
        } else {
            self.imported_datasets
                .get(index - 1)
                .map(|dataset| dataset.display_name.clone())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| {
                    if self.language == Language::Zh {
                        format!("数据组 {}", index + 1)
                    } else {
                        format!("Dataset {}", index + 1)
                    }
                })
        };
        format!("{prefix}{}  {name}", Self::dataset_letter(index))
    }

    fn dataset_count(&self) -> usize {
        usize::from(self.source.is_some()) + self.imported_datasets.len()
    }

    fn selected_fft_dataset_index(&self) -> usize {
        self.fft_dataset_index
            .min(self.dataset_count().saturating_sub(1))
    }

    fn sync_channel_state_lengths(&mut self) {
        let Some((state_len, default_visible, default_names)) = self.meta().map(|meta| {
            let state_len = meta
                .channels
                .iter()
                .map(|channel| channel.index)
                .max()
                .map(|index| index + 1)
                .unwrap_or(0);
            let mut default_visible = vec![false; state_len];
            let mut default_names = vec![String::new(); state_len];
            for channel in &meta.channels {
                if channel.index < state_len {
                    default_visible[channel.index] = channel.default_visible;
                    default_names[channel.index] = channel.name.clone();
                }
            }
            (state_len, default_visible, default_names)
        }) else {
            return;
        };

        let old_visible_len = self.visible.len();
        self.visible.truncate(state_len);
        if self.visible.len() < state_len {
            self.visible
                .extend(default_visible.iter().skip(old_visible_len).copied());
        }

        let old_display_len = self.display_names.len();
        self.display_names.truncate(state_len);
        if self.display_names.len() < state_len {
            self.display_names
                .extend(default_names.iter().skip(old_display_len).cloned());
        }

        let old_color_len = self.channel_colors.len();
        self.channel_colors.truncate(state_len);
        if self.channel_colors.len() < state_len {
            self.channel_colors
                .extend((old_color_len..state_len).map(Self::default_channel_color));
        }

        self.line_widths.truncate(state_len);
        if self.line_widths.len() < state_len {
            self.line_widths
                .resize(state_len, DEFAULT_CHANNEL_LINE_WIDTH);
        }

        self.line_patterns.truncate(state_len);
        if self.line_patterns.len() < state_len {
            self.line_patterns
                .resize(state_len, ChannelLinePattern::Solid);
        }

        self.channel_scales.truncate(state_len);
        if self.channel_scales.len() < state_len {
            self.channel_scales.resize(state_len, DEFAULT_CHANNEL_SCALE);
        }

        let pane_count = self.scope_pane_count();
        self.channel_panes.truncate(state_len);
        if self.channel_panes.len() < state_len {
            self.channel_panes.resize(state_len, 0);
        }
        for pane in &mut self.channel_panes {
            *pane = (*pane).min(pane_count.saturating_sub(1));
        }
        self.active_scope_pane = self.active_scope_pane.min(pane_count.saturating_sub(1));

        if self
            .editing_display_name
            .is_some_and(|channel_index| channel_index >= state_len)
        {
            self.editing_display_name = None;
            self.pending_display_name_focus = None;
        }
        if self
            .pending_display_name_focus
            .is_some_and(|channel_index| channel_index >= state_len)
        {
            self.pending_display_name_focus = None;
        }
        if self
            .hovered_channel
            .is_some_and(|channel_index| channel_index >= state_len)
        {
            self.hovered_channel = None;
        }
        self.fft_results
            .retain(|(channel_index, _)| *channel_index < state_len);
        if state_len > 0 && self.fft_channel >= state_len {
            self.fft_channel = 0;
            self.fft_channel_user_selected = false;
            self.fft_results.clear();
            self.needs_fft_reload = true;
        }
        let channel_options = self.fft_channel_options();
        let valid_triplet = |channels: &[usize; 3]| {
            channels
                .iter()
                .all(|channel| channel_options.contains(channel))
                && channels[0] != channels[1]
                && channels[0] != channels[2]
                && channels[1] != channels[2]
        };
        if !valid_triplet(&self.pll_source_channels) {
            self.pll_source_channels = self
                .preferred_pll_source_channels(&channel_options)
                .or_else(|| Self::default_sequence_channels_from_options(&channel_options))
                .unwrap_or([0, 1, 2]);
            self.needs_derived_reload = true;
        }
        if !valid_triplet(&self.dq_source_channels) {
            self.dq_source_channels = self
                .preferred_three_phase_channels(&channel_options, true)
                .unwrap_or(self.pll_source_channels);
            self.dq_source_channels_user_selected = false;
            self.needs_derived_reload = true;
        }
        let time_sync_options = self.primary_time_sync_channel_options();
        let valid_time_sync_triplet = |channels: &[usize; 3]| {
            channels
                .iter()
                .all(|channel| time_sync_options.contains(channel))
                && channels[0] != channels[1]
                && channels[0] != channels[2]
                && channels[1] != channels[2]
        };
        if !valid_time_sync_triplet(&self.time_sync_source_channels) {
            self.time_sync_source_channels = self
                .preferred_time_sync_source_channels(&time_sync_options)
                .or_else(|| Self::default_sequence_channels_from_options(&time_sync_options))
                .unwrap_or([0, 1, 2]);
            self.time_sync_source_channels_user_selected = false;
            self.time_sync_status.clear();
        }

        for dataset in &mut self.imported_datasets {
            let imported_len = dataset
                .source
                .metadata()
                .channels
                .iter()
                .map(|channel| channel.index)
                .max()
                .map(|index| index + 1)
                .unwrap_or(0);
            dataset.visible.truncate(imported_len);
            if dataset.visible.len() < imported_len {
                dataset.visible.resize(imported_len, false);
            }
        }

        self.sync_derived_state_lengths();
    }

    fn sync_derived_state_lengths(&mut self) {
        let state_len = self.derived_output_count();
        let old_color_len = self.derived_colors.len();
        self.derived_visible.resize(state_len, false);
        self.derived_colors.truncate(state_len);
        if self.derived_colors.len() < state_len {
            self.derived_colors
                .extend((old_color_len..state_len).map(Self::default_derived_color));
        }
        self.derived_line_patterns
            .resize(state_len, ChannelLinePattern::Solid);
        self.derived_panes.resize(state_len, 0);
    }

    #[allow(dead_code)]
    fn set_repid_derived_curves(&mut self, curves: Vec<crate::repid_derived::RepidDerivedCurve>) {
        self.repid_derived_curves = curves;
        self.sync_derived_state_lengths();
        self.derived_curve_cache = None;
        self.prepared_derived_curve_cache = PreparedPlotSeries::default();
        self.derived_measurement_cache = None;
        self.needs_derived_reload = true;
    }

    fn clear_derived_runtime_state(&mut self) {
        self.clear_y_overrides();
        self.derived_curve_cache = None;
        self.prepared_derived_curve_cache = PreparedPlotSeries::default();
        self.derived_measurement_cache = None;
        self.needs_derived_reload = true;
    }

    fn remove_repid_derived_curve(&mut self, repid_index: usize) {
        if repid_index >= self.repid_derived_curves.len() {
            return;
        }
        self.repid_derived_curves.remove(repid_index);
        let derived_index = DERIVED_CHANNEL_COUNT + repid_index;
        if derived_index < self.derived_visible.len() {
            self.derived_visible.remove(derived_index);
        }
        if derived_index < self.derived_colors.len() {
            self.derived_colors.remove(derived_index);
        }
        if derived_index < self.derived_line_patterns.len() {
            self.derived_line_patterns.remove(derived_index);
        }
        if derived_index < self.derived_panes.len() {
            self.derived_panes.remove(derived_index);
        }
        self.sync_derived_state_lengths();
        self.clear_derived_runtime_state();
    }

    fn add_repid_sequence_presets(&mut self) {
        let start_index = self.repid_derived_curves.len();
        let mut presets = crate::repid_derived::positive_sequence_presets(
            self.pll_source_channels,
            self.dq_source_channels,
            crate::repid_derived::RepidPresetBases::default(),
        );
        for curve in &mut presets {
            curve.name = self.unique_repid_curve_name(&curve.name);
            curve.raw_name = curve.name.clone();
        }
        self.repid_derived_curves.extend(presets);
        self.sync_derived_state_lengths();
        for repid_index in start_index..self.repid_derived_curves.len() {
            let derived_index = DERIVED_CHANNEL_COUNT + repid_index;
            if let Some(visible) = self.derived_visible.get_mut(derived_index) {
                *visible = true;
            }
            let active_pane = self.current_scope_pane();
            if let Some(pane) = self.derived_panes.get_mut(derived_index) {
                *pane = active_pane;
            }
        }
        self.clear_derived_runtime_state();
    }

    fn unique_repid_curve_name(&self, base_name: &str) -> String {
        if !self
            .repid_derived_curves
            .iter()
            .any(|curve| curve.name == base_name)
        {
            return base_name.to_owned();
        }
        for suffix in 2.. {
            let candidate = format!("{base_name} {suffix}");
            if !self
                .repid_derived_curves
                .iter()
                .any(|curve| curve.name == candidate)
            {
                return candidate;
            }
        }
        unreachable!("suffix loop always returns")
    }

    fn set_source(&mut self, source: Arc<dyn DataSource>, path: PathBuf, kind: SourceKind) {
        self.bump_data_generation();
        let meta = source.metadata().clone();
        self.primary_dataset_name = Self::default_dataset_name(&path);
        self.visible = meta
            .channels
            .iter()
            .map(|channel| channel.default_visible)
            .collect();
        self.display_names = meta
            .channels
            .iter()
            .map(|channel| channel.name.clone())
            .collect();
        self.editing_display_name = None;
        self.pending_display_name_focus = None;
        self.channel_colors = meta
            .channels
            .iter()
            .map(|channel| Self::default_channel_color(channel.index))
            .collect();
        self.line_widths = vec![DEFAULT_CHANNEL_LINE_WIDTH; meta.channels.len()];
        self.line_patterns = vec![ChannelLinePattern::Solid; meta.channels.len()];
        self.channel_scales = vec![DEFAULT_CHANNEL_SCALE; meta.channels.len()];
        self.channel_panes = vec![0; meta.channels.len()];
        let derived_count = self.derived_output_count();
        self.derived_visible = vec![false; derived_count];
        self.derived_colors = (0..derived_count)
            .map(Self::default_derived_color)
            .collect();
        self.derived_line_patterns = vec![ChannelLinePattern::Solid; derived_count];
        self.derived_panes = vec![0; derived_count];
        self.active_scope_pane = 0;
        self.hovered_channel = None;
        self.view_start = meta.start_time;
        self.view_end = meta.end_time;
        self.y_min = None;
        self.y_max = None;
        let span = meta.duration();
        self.cursor_a = meta.start_time + span * 0.33;
        self.cursor_b = meta.start_time + span * 0.66;
        self.show_cursor_a = true;
        self.show_cursor_b = true;
        self.fft_results.clear();
        self.measurement_cache = None;
        self.needs_fft_reload = true;
        self.plot_cache = SampleBlock::default();
        self.plot_summary = None;
        self.prepared_plot_cache = PreparedPlotSeries::default();
        self.prepared_plot_summary = None;
        self.loaded_path = Some(path);
        self.source = Some(source);
        self.source_kind = Some(kind);
        self.primary_selected_for_delete = false;
        self.fft_dataset_index = 0;
        self.fft_channel_user_selected = false;
        self.fft_channel = self
            .preferred_fft_channel(&self.fft_channel_options())
            .unwrap_or(0);
        self.sequence_channels = self
            .preferred_sequence_channels(&self.fft_channel_options())
            .unwrap_or([0, 1, 2]);
        self.sequence_channels_user_selected = false;
        self.pll_source_channels = self
            .preferred_pll_source_channels(&self.fft_channel_options())
            .unwrap_or([0, 1, 2]);
        self.dq_source_channels = self.pll_source_channels;
        self.dq_source_channels_user_selected = false;
        let time_sync_options = self.primary_time_sync_channel_options();
        self.time_sync_source_channels = self
            .preferred_time_sync_source_channels(&time_sync_options)
            .or_else(|| Self::default_sequence_channels_from_options(&time_sync_options))
            .unwrap_or([0, 1, 2]);
        self.time_sync_source_channels_user_selected = false;
        self.time_sync_status.clear();
        self.derived_curve_cache = None;
        self.prepared_derived_curve_cache = PreparedPlotSeries::default();
        self.derived_measurement_cache = None;
        self.last_error = None;
        self.import_status = None;
        self.needs_plot_reload = true;
        self.needs_compare_plot_reload = true;
        self.needs_derived_reload = true;
        self.cursor_place_mode = None;
    }

    fn add_imported_dataset(
        &mut self,
        source: Arc<dyn DataSource>,
        path: PathBuf,
        kind: SourceKind,
    ) {
        self.bump_data_generation();
        let visible_len = source
            .metadata()
            .channels
            .iter()
            .map(|channel| channel.index)
            .max()
            .map(|index| index + 1)
            .unwrap_or(0);
        let display_name = Self::default_dataset_name(&path);
        self.imported_datasets.push(ImportedDataset {
            source,
            kind,
            path,
            display_name,
            visible: vec![false; visible_len],
            line_pattern: ChannelLinePattern::Dashed,
            time_offset: 0.0,
            plot_cache_key: None,
            plot_cache: SampleBlock::default(),
            plot_summary: None,
            prepared_plot_cache: PreparedPlotSeries::default(),
            prepared_plot_summary: None,
            selected_for_delete: false,
        });
        self.last_error = None;
        self.import_status = None;
        self.needs_compare_plot_reload = true;
        self.y_min = None;
        self.y_max = None;
    }

    fn clear_imported_datasets(&mut self) {
        self.bump_data_generation();
        self.imported_datasets.clear();
        self.needs_compare_plot_reload = false;
        self.y_min = None;
        self.y_max = None;
    }

    fn clear_all_datasets(&mut self) {
        self.bump_data_generation();
        self.source = None;
        self.source_kind = None;
        self.loaded_path = None;
        self.primary_dataset_name.clear();
        self.clear_imported_datasets();
        self.visible.clear();
        self.display_names.clear();
        self.editing_display_name = None;
        self.pending_display_name_focus = None;
        self.channel_colors.clear();
        self.line_widths.clear();
        self.line_patterns.clear();
        self.channel_scales.clear();
        self.channel_panes.clear();
        let derived_count = self.derived_output_count();
        self.derived_visible = vec![false; derived_count];
        self.derived_panes = vec![0; derived_count];
        self.active_scope_pane = 0;
        self.hovered_channel = None;
        self.import_status = None;
        self.primary_selected_for_delete = false;
        self.plot_cache = SampleBlock::default();
        self.plot_summary = None;
        self.prepared_plot_cache = PreparedPlotSeries::default();
        self.prepared_plot_summary = None;
        self.fft_results.clear();
        self.fft_dataset_index = 0;
        self.measurement_cache = None;
        self.derived_curve_cache = None;
        self.prepared_derived_curve_cache = PreparedPlotSeries::default();
        self.derived_measurement_cache = None;
        self.needs_plot_reload = false;
        self.needs_compare_plot_reload = false;
        self.needs_derived_reload = false;
        self.needs_fft_reload = false;
        self.y_min = None;
        self.y_max = None;
        self.time_sync_status.clear();
    }

    fn delete_selected_datasets(&mut self) {
        if self.source.is_none() {
            self.clear_imported_datasets();
            return;
        }

        if self.primary_selected_for_delete {
            let mut remaining = self
                .imported_datasets
                .drain(..)
                .filter(|dataset| !dataset.selected_for_delete)
                .collect::<Vec<_>>();
            if remaining.is_empty() {
                self.clear_all_datasets();
            } else {
                let promoted = remaining.remove(0);
                let promoted_name = promoted.display_name.clone();
                let promoted_visible = promoted.visible.clone();
                let promoted_line_pattern = promoted.line_pattern;
                self.set_source(promoted.source, promoted.path, promoted.kind);
                self.primary_dataset_name = promoted_name;
                for (index, visible) in promoted_visible.into_iter().enumerate() {
                    if let Some(current) = self.visible.get_mut(index) {
                        *current = visible;
                    }
                }
                self.line_patterns.fill(promoted_line_pattern);
                for dataset in &mut remaining {
                    dataset.time_offset = 0.0;
                }
                self.imported_datasets = remaining;
                self.primary_selected_for_delete = false;
                self.time_sync_status.clear();
                self.fft_dataset_index = 0;
                self.needs_compare_plot_reload = true;
            }
        } else {
            let old_len = self.imported_datasets.len();
            self.imported_datasets
                .retain(|dataset| !dataset.selected_for_delete);
            if self.imported_datasets.len() != old_len {
                for dataset in &mut self.imported_datasets {
                    dataset.selected_for_delete = false;
                    dataset.plot_cache = SampleBlock::default();
                    dataset.plot_summary = None;
                    dataset.prepared_plot_cache = PreparedPlotSeries::default();
                    dataset.prepared_plot_summary = None;
                }
                self.needs_compare_plot_reload = true;
                self.fft_dataset_index = self.selected_fft_dataset_index();
                self.fft_results.clear();
                self.needs_fft_reload = true;
                self.y_min = None;
                self.y_max = None;
            }
        }
    }

    fn delete_dataset_group(&mut self, dataset_index: usize) {
        if self.source.is_none() {
            return;
        }

        self.primary_selected_for_delete = dataset_index == 0;
        for (index, dataset) in self.imported_datasets.iter_mut().enumerate() {
            dataset.selected_for_delete = dataset_index == index + 1;
        }
        self.delete_selected_datasets();
    }

    fn dataset_selected_for_delete(&self, dataset_index: usize) -> bool {
        if dataset_index == 0 {
            self.primary_selected_for_delete
        } else {
            self.imported_datasets
                .get(dataset_index - 1)
                .map(|dataset| dataset.selected_for_delete)
                .unwrap_or(false)
        }
    }

    fn set_dataset_selected_for_delete(&mut self, dataset_index: usize, selected: bool) {
        if dataset_index == 0 {
            self.primary_selected_for_delete = selected;
        } else if let Some(dataset) = self.imported_datasets.get_mut(dataset_index - 1) {
            dataset.selected_for_delete = selected;
        }
    }

    fn any_dataset_selected_for_delete(&self) -> bool {
        self.primary_selected_for_delete
            || self
                .imported_datasets
                .iter()
                .any(|dataset| dataset.selected_for_delete)
    }

    fn recent_files_path() -> PathBuf {
        Self::app_home_dir().join("scope-recent-files.json")
    }

    fn legacy_recent_configs_path() -> PathBuf {
        Self::app_home_dir().join("scope-recent-configs.json")
    }

    fn recent_configs_path(section: ConfigSection) -> PathBuf {
        Self::app_home_dir().join(section.recent_file_name())
    }

    fn load_recent_files() -> Vec<PathBuf> {
        let Ok(text) = std::fs::read_to_string(Self::recent_files_path()) else {
            return Vec::new();
        };
        let Ok(recent) = serde_json::from_str::<RecentFiles>(&text) else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        for path in recent.paths {
            if !paths.iter().any(|existing| existing == &path) {
                paths.push(path);
            }
            if paths.len() >= MAX_RECENT_FILES {
                break;
            }
        }
        paths
    }

    fn load_recent_configs(section: ConfigSection) -> Vec<PathBuf> {
        let path = Self::recent_configs_path(section);
        let text = std::fs::read_to_string(&path).or_else(|error| {
            if section == ConfigSection::Names {
                std::fs::read_to_string(Self::legacy_recent_configs_path())
            } else {
                Err(error)
            }
        });
        let Ok(text) = text else {
            return Vec::new();
        };
        let Ok(recent) = serde_json::from_str::<RecentFiles>(&text) else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        for path in recent.paths {
            if !paths.iter().any(|existing| existing == &path) {
                paths.push(path);
            }
            if paths.len() >= MAX_RECENT_CONFIGS {
                break;
            }
        }
        paths
    }

    fn save_recent_files(&self) {
        let recent = RecentFiles {
            paths: self.recent_files.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&recent) {
            let _ = std::fs::write(Self::recent_files_path(), json);
        }
    }

    fn save_recent_configs(&self, section: ConfigSection) {
        let recent = RecentFiles {
            paths: self.recent_configs(section).to_vec(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&recent) {
            let _ = std::fs::write(Self::recent_configs_path(section), json);
        }
    }

    fn remember_recent_file(&mut self, path: &Path) {
        let normalized = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.recent_files.retain(|existing| existing != &normalized);
        self.recent_files.insert(0, normalized);
        self.recent_files.truncate(MAX_RECENT_FILES);
        self.save_recent_files();
    }

    fn recent_configs(&self, section: ConfigSection) -> &[PathBuf] {
        match section {
            ConfigSection::Names => &self.recent_name_configs,
            ConfigSection::Display => &self.recent_display_configs,
            ConfigSection::Shortcut => &self.recent_shortcut_configs,
            ConfigSection::Dataset => &self.recent_dataset_configs,
        }
    }

    fn recent_configs_mut(&mut self, section: ConfigSection) -> &mut Vec<PathBuf> {
        match section {
            ConfigSection::Names => &mut self.recent_name_configs,
            ConfigSection::Display => &mut self.recent_display_configs,
            ConfigSection::Shortcut => &mut self.recent_shortcut_configs,
            ConfigSection::Dataset => &mut self.recent_dataset_configs,
        }
    }

    fn remember_recent_config(&mut self, section: ConfigSection, path: &Path) {
        let normalized = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let recent = self.recent_configs_mut(section);
        recent.retain(|existing| existing != &normalized);
        recent.insert(0, normalized);
        recent.truncate(MAX_RECENT_CONFIGS);
        self.save_recent_configs(section);
    }

    fn clear_recent_files(&mut self) {
        self.recent_files.clear();
        self.save_recent_files();
    }

    fn clear_recent_configs(&mut self, section: ConfigSection) {
        self.recent_configs_mut(section).clear();
        self.save_recent_configs(section);
    }

    fn recent_file_label(path: &Path) -> String {
        let display = path.to_string_lossy();
        display
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or(&display)
            .to_owned()
    }

    #[allow(dead_code)]
    fn open_standard_csv(&mut self, path: PathBuf) -> bool {
        match CsvDataSource::open_with_sample_rate(&path, self.sample_rate_hz) {
            Ok(source) => {
                self.set_source(Arc::new(source), path, SourceKind::Local);
                true
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    #[allow(dead_code)]
    fn open_cloud_csv(&mut self, path: PathBuf) -> bool {
        match CloudCsvDataSource::open_with_sample_rate(&path, self.sample_rate_hz) {
            Ok(source) => {
                self.set_source(Arc::new(source), path, SourceKind::Cloud);
                true
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    #[allow(dead_code)]
    fn open_auto_csv(&mut self, path: PathBuf) -> bool {
        let opened = match Self::looks_like_cloud_csv(&path) {
            Ok(true) => self.open_cloud_csv(path),
            Ok(false) => self.open_standard_csv(path),
            Err(error) => {
                self.last_error = Some(error);
                false
            }
        };
        if opened {
            if let Some(path) = self.loaded_path.clone() {
                self.remember_recent_file(&path);
            }
        }
        opened
    }

    #[allow(dead_code)]
    fn open_standard_compare_csv(&mut self, path: PathBuf) -> bool {
        match CsvDataSource::open_with_sample_rate(&path, self.sample_rate_hz) {
            Ok(source) => {
                self.add_imported_dataset(Arc::new(source), path, SourceKind::Local);
                true
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    #[allow(dead_code)]
    fn open_cloud_compare_csv(&mut self, path: PathBuf) -> bool {
        match CloudCsvDataSource::open_with_sample_rate(&path, self.sample_rate_hz) {
            Ok(source) => {
                self.add_imported_dataset(Arc::new(source), path, SourceKind::Cloud);
                true
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    #[allow(dead_code)]
    fn open_auto_compare_csv(&mut self, path: PathBuf) -> bool {
        if self.source.is_none() {
            self.last_error = Some(
                self.tr(
                    "Import a primary dataset first, or select multiple CSV files at once.",
                    "Import a primary dataset first, or select multiple CSV files at once.",
                )
                .to_owned(),
            );
            return false;
        }
        let recent_path = path.clone();
        let opened = match Self::looks_like_cloud_csv(&path) {
            Ok(true) => self.open_cloud_compare_csv(path),
            Ok(false) => self.open_standard_compare_csv(path),
            Err(error) => {
                self.last_error = Some(error);
                false
            }
        };
        if opened {
            self.remember_recent_file(&recent_path);
        }
        opened
    }

    fn import_data_files(&mut self, paths: Vec<PathBuf>) -> bool {
        if paths.is_empty() {
            self.last_error = Some("Select at least one waveform data file.".to_owned());
            return false;
        }
        if self.import_worker.is_some() {
            self.last_error = Some("Data import is already running.".to_owned());
            return false;
        }

        self.import_status = Some(self.t(UiText::ImportingData).to_owned());
        self.last_error = None;
        let generation = self.data_generation;
        let replace_primary = self.source.is_none();
        let sample_rate_hz = self.sample_rate_hz;
        let cancel_token = JobCancelToken::new();
        let worker_cancel = cancel_token.clone();
        let data_cancel = worker_cancel.data_token();
        self.import_cancel = Some(cancel_token);
        Self::spawn_job(&mut self.import_worker, move || {
            let generation_for_panic = generation;
            let replace_primary_for_panic = replace_primary;
            match panic::catch_unwind(AssertUnwindSafe(|| {
                let (opened, errors) =
                    Self::open_waveform_files_cancellable(paths, sample_rate_hz, &data_cancel);
                ImportJobResult {
                    generation,
                    replace_primary,
                    opened,
                    errors,
                }
            })) {
                Ok(result) => result,
                Err(payload) => ImportJobResult {
                    generation: generation_for_panic,
                    replace_primary: replace_primary_for_panic,
                    opened: Vec::new(),
                    errors: vec![Self::recover_worker_panic(
                        "Import worker panicked.",
                        payload,
                    )],
                },
            }
        });
        true
    }

    fn import_finished_message(imported: usize, failed: usize, language: Language) -> String {
        match language {
            Language::Zh => {
                if failed == 0 {
                    format!("已导入 {imported} 个数据组。")
                } else {
                    format!("已导入 {imported} 个数据组，{failed} 个文件失败。")
                }
            }
            Language::En => {
                if failed == 0 {
                    format!("Import finished: {imported} datasets imported.")
                } else {
                    format!("Import finished: {imported} datasets imported, {failed} files failed.")
                }
            }
        }
    }

    fn export_finished_message(rows: u64, language: Language) -> String {
        match language {
            Language::Zh => format!("已导出 {rows} 行。"),
            Language::En => format!("Exported {rows} rows."),
        }
    }

    fn export_cursor_finished_message(rows: u64, language: Language) -> String {
        match language {
            Language::Zh => format!("已导出光标区间数据 {rows} 行。"),
            Language::En => format!("Exported {rows} cursor-range rows."),
        }
    }

    fn export_failed_message(error: &str, language: Language) -> String {
        match language {
            Language::Zh => format!("导出失败: {error}"),
            Language::En => format!("Failed to export data: {error}"),
        }
    }

    fn export_cursor_failed_message(error: &str, language: Language) -> String {
        match language {
            Language::Zh => format!("导出光标区间失败: {error}"),
            Language::En => format!("Failed to export cursor range: {error}"),
        }
    }

    fn poll_import_worker(&mut self) {
        let Some(joined) =
            Self::take_finished_job(&mut self.import_worker, "Import worker panicked.")
        else {
            return;
        };
        self.import_cancel = None;
        let Ok(result) = joined else {
            self.last_error = Some("Import worker panicked.".to_owned());
            self.import_status = None;
            return;
        };
        if result.generation != self.data_generation {
            self.import_status = None;
            return;
        }
        let mut opened = result.opened.into_iter();
        let mut imported_count = 0_usize;
        if result.replace_primary {
            if let Some(primary) = opened.next() {
                let recent_path = primary.path.clone();
                self.set_source(primary.source, primary.path, primary.kind);
                self.remember_recent_file(&recent_path);
                imported_count += 1;
            }
        }
        for dataset in opened {
            let recent_path = dataset.path.clone();
            self.add_imported_dataset(dataset.source, dataset.path, dataset.kind);
            self.remember_recent_file(&recent_path);
            imported_count += 1;
        }
        self.import_status = Some(Self::import_finished_message(
            imported_count,
            result.errors.len(),
            self.language,
        ));
        if !result.errors.is_empty() {
            self.last_error = Some(result.errors.join("\n"));
        }
    }

    #[allow(dead_code)]
    fn open_waveform_file(path: &Path, sample_rate_hz: f64) -> Result<OpenedDataset, String> {
        Self::open_waveform_file_with_cancel(path, sample_rate_hz, None)
    }

    fn open_waveform_file_with_cancel(
        path: &Path,
        sample_rate_hz: f64,
        cancel: Option<&DataCancelToken>,
    ) -> Result<OpenedDataset, String> {
        if let Some(cancel) = cancel {
            cancel.check().map_err(|error| error.to_string())?;
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        match extension.as_str() {
            "dat" => {
                let source = if let Some(cancel) = cancel {
                    DatDataSource::open_cancellable(path, cancel)
                } else {
                    DatDataSource::open(path)
                };
                source
                    .map(|source| OpenedDataset {
                        source: Arc::new(source),
                        path: path.to_owned(),
                        kind: SourceKind::Dat,
                    })
                    .map_err(|error| error.to_string())
            }
            "csv" => match Self::looks_like_cloud_csv(path) {
                Ok(true) => CloudCsvDataSource::open_with_sample_rate(path, sample_rate_hz)
                    .map(|source| OpenedDataset {
                        source: Arc::new(source),
                        path: path.to_owned(),
                        kind: SourceKind::Cloud,
                    })
                    .map_err(|error| error.to_string()),
                Ok(false) => {
                    let source = if let Some(cancel) = cancel {
                        CsvDataSource::open_with_sample_rate_cancellable(
                            path,
                            sample_rate_hz,
                            cancel,
                        )
                    } else {
                        CsvDataSource::open_with_sample_rate(path, sample_rate_hz)
                    };
                    source
                        .map(|source| OpenedDataset {
                            source: Arc::new(source),
                            path: path.to_owned(),
                            kind: SourceKind::Local,
                        })
                        .map_err(|error| error.to_string())
                }
                Err(error) => Err(error),
            },
            "scope" => scope_analyzer::live::scope_source::ScopeRecordingDataSource::open(path)
                .map(|source| OpenedDataset {
                    source: Arc::new(source),
                    path: path.to_owned(),
                    kind: SourceKind::Scope,
                })
                .map_err(|error| error.to_string()),
            other => Err(format!(
                "Unsupported waveform file extension `{}`. Supported formats: .csv, .dat, .scope",
                if other.is_empty() { "(none)" } else { other }
            )),
        }
    }

    #[allow(dead_code)]
    fn open_waveform_files(
        paths: Vec<PathBuf>,
        sample_rate_hz: f64,
    ) -> (Vec<OpenedDataset>, Vec<String>) {
        Self::open_waveform_files_with_cancel(paths, sample_rate_hz, None)
    }

    fn open_waveform_files_cancellable(
        paths: Vec<PathBuf>,
        sample_rate_hz: f64,
        cancel: &DataCancelToken,
    ) -> (Vec<OpenedDataset>, Vec<String>) {
        Self::open_waveform_files_with_cancel(paths, sample_rate_hz, Some(cancel))
    }

    fn open_waveform_files_with_cancel(
        paths: Vec<PathBuf>,
        sample_rate_hz: f64,
        cancel: Option<&DataCancelToken>,
    ) -> (Vec<OpenedDataset>, Vec<String>) {
        let paths = Self::unique_import_paths(Self::expand_local_csv_counterparts(paths));
        let mut opened = Vec::new();
        let mut errors = Vec::new();
        let mut used = vec![false; paths.len()];

        let local_csv_infos = paths
            .iter()
            .map(|path| Self::local_csv_pair_info(path))
            .collect::<Vec<_>>();

        for index in 0..paths.len() {
            if let Some(cancel) = cancel {
                if let Err(error) = cancel.check() {
                    errors.push(error.to_string());
                    break;
                }
            }
            if used[index] {
                continue;
            }
            let Some(info) = &local_csv_infos[index] else {
                continue;
            };
            let Some(timestamp) = &info.filename_timestamp else {
                continue;
            };
            let wanted = match info.role {
                LocalCsvRole::Analog => LocalCsvRole::Digital,
                LocalCsvRole::Digital => LocalCsvRole::Analog,
            };
            let pair_index = paths.iter().enumerate().find_map(|(other_index, _)| {
                if other_index == index || used[other_index] {
                    return None;
                }
                let Some(other_info) = &local_csv_infos[other_index] else {
                    return None;
                };
                (other_info.role == wanted
                    && other_info.filename_timestamp.as_ref() == Some(timestamp))
                .then_some(other_index)
            });
            let Some(pair_index) = pair_index else {
                continue;
            };

            Self::open_local_csv_pair_by_indices(
                &paths,
                &mut used,
                &mut opened,
                &mut errors,
                index,
                pair_index,
                info.role,
                sample_rate_hz,
                cancel,
            );
        }

        for index in 0..paths.len() {
            if let Some(cancel) = cancel {
                if let Err(error) = cancel.check() {
                    errors.push(error.to_string());
                    break;
                }
            }
            if used[index] {
                continue;
            }
            let Some(info) = &local_csv_infos[index] else {
                continue;
            };
            let wanted = match info.role {
                LocalCsvRole::Analog => LocalCsvRole::Digital,
                LocalCsvRole::Digital => LocalCsvRole::Analog,
            };
            let pair_index = paths.iter().enumerate().find_map(|(other_index, _)| {
                if other_index == index || used[other_index] {
                    return None;
                }
                let Some(other_info) = &local_csv_infos[other_index] else {
                    return None;
                };
                (other_info.role == wanted
                    && (other_info.key == info.key || paths.len() == 2)
                    && !Self::filename_timestamps_conflict(info, other_info))
                .then_some(other_index)
            });
            let Some(pair_index) = pair_index else {
                continue;
            };

            Self::open_local_csv_pair_by_indices(
                &paths,
                &mut used,
                &mut opened,
                &mut errors,
                index,
                pair_index,
                info.role,
                sample_rate_hz,
                cancel,
            );
        }

        for index in 0..paths.len() {
            if let Some(cancel) = cancel {
                if let Err(error) = cancel.check() {
                    errors.push(error.to_string());
                    break;
                }
            }
            if used[index] {
                continue;
            }
            let Some(info) = &local_csv_infos[index] else {
                continue;
            };
            let Some(modified_millis) = info.modified_millis else {
                continue;
            };
            let wanted = match info.role {
                LocalCsvRole::Analog => LocalCsvRole::Digital,
                LocalCsvRole::Digital => LocalCsvRole::Analog,
            };
            let pair_index = paths
                .iter()
                .enumerate()
                .filter_map(|(other_index, _)| {
                    if other_index == index || used[other_index] {
                        return None;
                    }
                    let other_info = local_csv_infos[other_index].as_ref()?;
                    let other_millis = other_info.modified_millis?;
                    if other_info.role != wanted
                        || Self::filename_timestamps_conflict(info, other_info)
                    {
                        return None;
                    }
                    let diff = (modified_millis - other_millis).abs();
                    (diff <= LOCAL_CSV_PAIR_MTIME_WINDOW_MS).then_some((diff, other_index))
                })
                .min_by_key(|(diff, _)| *diff)
                .map(|(_, other_index)| other_index);
            let Some(pair_index) = pair_index else {
                continue;
            };

            Self::open_local_csv_pair_by_indices(
                &paths,
                &mut used,
                &mut opened,
                &mut errors,
                index,
                pair_index,
                info.role,
                sample_rate_hz,
                cancel,
            );
        }

        let remaining_local_csv = paths
            .iter()
            .enumerate()
            .filter_map(|(index, path)| {
                (!used[index] && Self::is_local_csv_file(path)).then_some(index)
            })
            .collect::<Vec<_>>();
        if remaining_local_csv.len() == 2 {
            let first = remaining_local_csv[0];
            let second = remaining_local_csv[1];
            let display_path = format!("{} + {}", paths[first].display(), paths[second].display());
            let result = Self::worker_result("Import worker panicked.", || {
                Self::open_local_csv_pair_by_content_with_cancel(
                    &paths[first],
                    &paths[second],
                    sample_rate_hz,
                    cancel,
                )
            });
            match result {
                Ok(dataset) => {
                    used[first] = true;
                    used[second] = true;
                    opened.push(dataset);
                }
                Err(error) => errors.push(format!("{display_path}: {error}")),
            }
        }

        for (index, path) in paths.into_iter().enumerate() {
            if let Some(cancel) = cancel {
                if let Err(error) = cancel.check() {
                    errors.push(error.to_string());
                    break;
                }
            }
            if used[index] {
                continue;
            }
            let display_path = path.display().to_string();
            let result = Self::worker_result("Import worker panicked.", || {
                Self::open_waveform_file_with_cancel(&path, sample_rate_hz, cancel)
            });
            match result {
                Ok(dataset) => opened.push(dataset),
                Err(error) => errors.push(format!("{display_path}: {error}")),
            }
        }

        (opened, errors)
    }

    #[allow(clippy::too_many_arguments)]
    fn open_local_csv_pair_by_indices(
        paths: &[PathBuf],
        used: &mut [bool],
        opened: &mut Vec<OpenedDataset>,
        errors: &mut Vec<String>,
        index: usize,
        pair_index: usize,
        role: LocalCsvRole,
        sample_rate_hz: f64,
        cancel: Option<&DataCancelToken>,
    ) {
        let analog_index = if role == LocalCsvRole::Analog {
            index
        } else {
            pair_index
        };
        let digital_index = if role == LocalCsvRole::Digital {
            index
        } else {
            pair_index
        };
        let display_path = format!(
            "{} + {}",
            paths[analog_index].display(),
            paths[digital_index].display()
        );
        let result = Self::worker_result("Import worker panicked.", || {
            Self::open_local_csv_pair_with_cancel(
                &paths[analog_index],
                &paths[digital_index],
                sample_rate_hz,
                cancel,
            )
        });
        match result {
            Ok(dataset) => {
                used[index] = true;
                used[pair_index] = true;
                opened.push(dataset);
            }
            Err(error) => errors.push(format!("{display_path}: {error}")),
        }
    }

    fn expand_local_csv_counterparts(paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut expanded = paths;
        let initial_len = expanded.len();
        let mut known_paths = expanded
            .iter()
            .map(|path| Self::local_path_key(path))
            .collect::<HashSet<_>>();

        for index in 0..initial_len {
            let Some(info) = Self::local_csv_pair_info(&expanded[index]) else {
                continue;
            };
            let Some(counterpart) =
                Self::find_local_csv_counterpart(&expanded[index], &info, &known_paths)
            else {
                continue;
            };
            known_paths.insert(Self::local_path_key(&counterpart));
            expanded.push(counterpart);
        }

        expanded
    }

    fn unique_import_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut seen = HashSet::new();
        paths
            .into_iter()
            .filter(|path| seen.insert(Self::local_path_key(path)))
            .collect()
    }

    fn find_local_csv_counterpart(
        path: &Path,
        info: &LocalCsvPairInfo,
        known_paths: &HashSet<String>,
    ) -> Option<PathBuf> {
        let parent = path.parent()?;
        let wanted = match info.role {
            LocalCsvRole::Analog => LocalCsvRole::Digital,
            LocalCsvRole::Digital => LocalCsvRole::Analog,
        };
        let mut timestamp_matches = Vec::new();
        let mut modified_matches = Vec::new();

        for entry in fs::read_dir(parent).ok()?.flatten() {
            let candidate = entry.path();
            if candidate == path || known_paths.contains(&Self::local_path_key(&candidate)) {
                continue;
            }
            let Some(candidate_info) = Self::local_csv_pair_info(&candidate) else {
                continue;
            };
            if candidate_info.role != wanted {
                continue;
            }
            if info.filename_timestamp.is_some()
                && info.filename_timestamp == candidate_info.filename_timestamp
            {
                timestamp_matches.push(candidate);
                continue;
            }
            let Some(left_millis) = info.modified_millis else {
                continue;
            };
            let Some(right_millis) = candidate_info.modified_millis else {
                continue;
            };
            if Self::filename_timestamps_conflict(info, &candidate_info) {
                continue;
            }
            let diff = (left_millis - right_millis).abs();
            if diff <= LOCAL_CSV_PAIR_MTIME_WINDOW_MS {
                modified_matches.push((diff, candidate));
            }
        }

        timestamp_matches.sort();
        timestamp_matches.into_iter().next().or_else(|| {
            modified_matches
                .into_iter()
                .min_by_key(|(diff, _)| *diff)
                .map(|(_, path)| path)
        })
    }

    fn local_csv_pair_info(path: &Path) -> Option<LocalCsvPairInfo> {
        let (role, key) = Self::local_csv_merge_role(path)?;
        Some(LocalCsvPairInfo {
            role,
            key,
            filename_timestamp: Self::filename_timestamp_key(path),
            modified_millis: Self::file_modified_millis(path),
        })
    }

    fn filename_timestamps_conflict(left: &LocalCsvPairInfo, right: &LocalCsvPairInfo) -> bool {
        matches!(
            (&left.filename_timestamp, &right.filename_timestamp),
            (Some(left), Some(right)) if left != right
        )
    }

    fn filename_timestamp_key(path: &Path) -> Option<String> {
        let stem = path.file_stem()?.to_string_lossy();
        let mut run = String::new();
        for ch in stem.chars().chain(std::iter::once('_')) {
            if ch.is_ascii_digit() {
                run.push(ch);
                continue;
            }
            if run.len() >= 14 {
                for start in 0..=run.len() - 14 {
                    let candidate = &run[start..start + 14];
                    if Self::looks_like_compact_timestamp(candidate) {
                        return Some(candidate.to_owned());
                    }
                }
            }
            run.clear();
        }
        None
    }

    fn looks_like_compact_timestamp(raw: &str) -> bool {
        if raw.len() != 14 || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        let parse = |range: std::ops::Range<usize>| raw[range].parse::<u32>().ok();
        let Some(year) = parse(0..4) else {
            return false;
        };
        let Some(month) = parse(4..6) else {
            return false;
        };
        let Some(day) = parse(6..8) else {
            return false;
        };
        let Some(hour) = parse(8..10) else {
            return false;
        };
        let Some(minute) = parse(10..12) else {
            return false;
        };
        let Some(second) = parse(12..14) else {
            return false;
        };
        (1970..=2099).contains(&year)
            && (1..=12).contains(&month)
            && (1..=31).contains(&day)
            && hour <= 23
            && minute <= 59
            && second <= 59
    }

    fn file_modified_millis(path: &Path) -> Option<i128> {
        let modified = fs::metadata(path).ok()?.modified().ok()?;
        Some(modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as i128)
    }

    fn local_path_key(path: &Path) -> String {
        path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_ascii_lowercase()
    }

    #[allow(dead_code)]
    fn open_local_csv_pair(
        analog_path: &Path,
        digital_path: &Path,
        sample_rate_hz: f64,
    ) -> Result<OpenedDataset, String> {
        Self::open_local_csv_pair_with_cancel(analog_path, digital_path, sample_rate_hz, None)
    }

    fn open_local_csv_pair_with_cancel(
        analog_path: &Path,
        digital_path: &Path,
        sample_rate_hz: f64,
        cancel: Option<&DataCancelToken>,
    ) -> Result<OpenedDataset, String> {
        if Self::looks_like_indexed_local_csv_pair(analog_path, digital_path) {
            return Self::open_indexed_local_csv_pair_with_cancel(
                analog_path,
                digital_path,
                sample_rate_hz,
                cancel,
            );
        }

        let analog = if let Some(cancel) = cancel {
            CsvDataSource::open_with_sample_rate_cancellable(analog_path, sample_rate_hz, cancel)
        } else {
            CsvDataSource::open_with_sample_rate(analog_path, sample_rate_hz)
        }
        .map_err(|error| error.to_string())?;
        let digital = if let Some(cancel) = cancel {
            CsvDataSource::open_with_sample_rate_cancellable(digital_path, sample_rate_hz, cancel)
        } else {
            CsvDataSource::open_with_sample_rate(digital_path, sample_rate_hz)
        }
        .map_err(|error| error.to_string())?;
        Self::open_local_csv_pair_sources(analog_path, analog, digital_path, digital)
    }

    #[allow(dead_code)]
    fn open_local_csv_pair_by_content(
        first_path: &Path,
        second_path: &Path,
        sample_rate_hz: f64,
    ) -> Result<OpenedDataset, String> {
        Self::open_local_csv_pair_by_content_with_cancel(
            first_path,
            second_path,
            sample_rate_hz,
            None,
        )
    }

    fn open_local_csv_pair_by_content_with_cancel(
        first_path: &Path,
        second_path: &Path,
        sample_rate_hz: f64,
        cancel: Option<&DataCancelToken>,
    ) -> Result<OpenedDataset, String> {
        let first_indexed_role = Self::local_indexed_csv_role(first_path);
        let second_indexed_role = Self::local_indexed_csv_role(second_path);
        match (first_indexed_role, second_indexed_role) {
            (Some(LocalCsvRole::Analog), Some(LocalCsvRole::Digital)) => {
                return Self::open_indexed_local_csv_pair_with_cancel(
                    first_path,
                    second_path,
                    sample_rate_hz,
                    cancel,
                );
            }
            (Some(LocalCsvRole::Digital), Some(LocalCsvRole::Analog)) => {
                return Self::open_indexed_local_csv_pair_with_cancel(
                    second_path,
                    first_path,
                    sample_rate_hz,
                    cancel,
                );
            }
            _ => {}
        }

        let first = if let Some(cancel) = cancel {
            CsvDataSource::open_with_sample_rate_cancellable(first_path, sample_rate_hz, cancel)
        } else {
            CsvDataSource::open_with_sample_rate(first_path, sample_rate_hz)
        }
        .map_err(|error| error.to_string())?;
        let second = if let Some(cancel) = cancel {
            CsvDataSource::open_with_sample_rate_cancellable(second_path, sample_rate_hz, cancel)
        } else {
            CsvDataSource::open_with_sample_rate(second_path, sample_rate_hz)
        }
        .map_err(|error| error.to_string())?;
        let first_digital = Self::csv_source_looks_digital(&first);
        let second_digital = Self::csv_source_looks_digital(&second);
        match (first_digital, second_digital) {
            (false, true) => {
                Self::open_local_csv_pair_sources(first_path, first, second_path, second)
            }
            (true, false) => {
                Self::open_local_csv_pair_sources(second_path, second, first_path, first)
            }
            _ => Err("Could not distinguish analog and digital local CSV files.".to_owned()),
        }
    }

    #[allow(dead_code)]
    fn open_indexed_local_csv_pair(
        analog_path: &Path,
        digital_path: &Path,
        sample_rate_hz: f64,
    ) -> Result<OpenedDataset, String> {
        Self::open_indexed_local_csv_pair_with_cancel(
            analog_path,
            digital_path,
            sample_rate_hz,
            None,
        )
    }

    fn open_indexed_local_csv_pair_with_cancel(
        analog_path: &Path,
        digital_path: &Path,
        sample_rate_hz: f64,
        cancel: Option<&DataCancelToken>,
    ) -> Result<OpenedDataset, String> {
        let analog = if let Some(cancel) = cancel {
            CsvDataSource::open_skipping_first_column_with_sample_rate_cancellable(
                analog_path,
                sample_rate_hz,
                cancel,
            )
        } else {
            CsvDataSource::open_skipping_first_column_with_sample_rate(analog_path, sample_rate_hz)
        }
        .map_err(|error| error.to_string())?;
        let digital = if let Some(cancel) = cancel {
            CsvDataSource::open_skipping_first_column_with_sample_rate_cancellable(
                digital_path,
                sample_rate_hz,
                cancel,
            )
        } else {
            CsvDataSource::open_skipping_first_column_with_sample_rate(digital_path, sample_rate_hz)
        }
        .map_err(|error| error.to_string())?;
        let source_name = Self::merged_local_csv_name(analog_path, digital_path);
        let digital_name = digital_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("DDATA.csv")
            .to_owned();
        let digital_source = MergedLeadingBitsDataSource::new(Arc::new(digital), digital_name)
            .map_err(|error| error.to_string())?;
        let source = CombinedDataSource::new(
            source_name,
            vec![
                (Arc::new(analog) as Arc<dyn DataSource>, false),
                (Arc::new(digital_source) as Arc<dyn DataSource>, true),
            ],
        )
        .map_err(|error| error.to_string())?;
        let source: Arc<dyn DataSource> =
            if source.metadata().channels.len() == VARIABLE_NAMES.len() {
                let source_name = source.metadata().source_name.clone();
                Arc::new(
                    RenamedDataSource::new(Arc::new(source), source_name, &VARIABLE_NAMES)
                        .map_err(|error| error.to_string())?,
                )
            } else {
                Arc::new(source)
            };

        Ok(OpenedDataset {
            source,
            path: analog_path.to_owned(),
            kind: SourceKind::Local,
        })
    }

    fn open_local_csv_pair_sources(
        analog_path: &Path,
        analog: CsvDataSource,
        digital_path: &Path,
        digital: CsvDataSource,
    ) -> Result<OpenedDataset, String> {
        let source_name = Self::merged_local_csv_name(analog_path, digital_path);
        let digital_source = Self::local_digital_source(digital_path, digital);
        let source = CombinedDataSource::new(
            source_name,
            vec![
                (Arc::new(analog) as Arc<dyn DataSource>, false),
                (digital_source, true),
            ],
        )
        .map_err(|error| error.to_string())?;

        Ok(OpenedDataset {
            source: Arc::new(source),
            path: analog_path.to_owned(),
            kind: SourceKind::Local,
        })
    }

    fn local_digital_source(path: &Path, source: CsvDataSource) -> Arc<dyn DataSource> {
        if Self::should_expand_ddata_bitfields(path, &source) {
            let bitfield_channels = source
                .metadata()
                .channels
                .iter()
                .take(3)
                .map(|channel| channel.index)
                .collect::<Vec<_>>();
            let source_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("DDATA.csv")
                .to_owned();
            Arc::new(BitfieldDigitalDataSource::new(
                Arc::new(source),
                source_name,
                bitfield_channels,
            ))
        } else {
            Arc::new(source)
        }
    }

    fn should_expand_ddata_bitfields(path: &Path, source: &CsvDataSource) -> bool {
        if source.metadata().channels.len() < 3 {
            return false;
        }
        let path_has_ddata = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.to_ascii_lowercase().contains("ddata"));
        let channels_have_ddata = source
            .metadata()
            .channels
            .iter()
            .take(3)
            .any(|channel| channel.name.to_ascii_lowercase().contains("ddata"));
        path_has_ddata || channels_have_ddata
    }

    fn is_local_csv_file(path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
            && Self::looks_like_cloud_csv(path).is_ok_and(|is_cloud| !is_cloud)
    }

    fn csv_source_looks_digital(source: &dyn DataSource) -> bool {
        let meta = source.metadata();
        let channels = meta
            .channels
            .iter()
            .take(12)
            .map(|channel| channel.index)
            .collect::<Vec<_>>();
        if channels.is_empty() {
            return false;
        }
        let Ok(block) = source.read_range(meta.start_time, meta.end_time, &channels, 512) else {
            return false;
        };
        let checked = block
            .channels
            .iter()
            .filter(|values| !values.is_empty())
            .count();
        if checked == 0 {
            return false;
        }
        let digital = block
            .channels
            .iter()
            .filter(|values| Self::samples_look_digital(values))
            .count();
        digital * 2 >= checked
    }

    fn local_csv_merge_role(path: &Path) -> Option<(LocalCsvRole, String)> {
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("csv"))
        {
            return None;
        }
        if Self::looks_like_cloud_csv(path).ok()? {
            return None;
        }

        let stem = path.file_stem()?.to_string_lossy();
        let lowered = stem.to_ascii_lowercase();
        let role = if stem.contains("模拟量")
            || stem.contains("模拟")
            || lowered.contains("analog")
            || lowered.contains("adata")
            || lowered.contains("_ai")
            || lowered.contains("-ai")
        {
            LocalCsvRole::Analog
        } else if stem.contains("数字量")
            || stem.contains("数字")
            || lowered.contains("digital")
            || lowered.contains("ddata")
            || lowered.contains("_di")
            || lowered.contains("-di")
            || lowered.contains("status")
            || lowered.contains("logic")
        {
            LocalCsvRole::Digital
        } else if let Some(role) = Self::local_indexed_csv_role(path) {
            role
        } else {
            return None;
        };
        Some((role, Self::local_csv_merge_key(&stem)))
    }

    fn looks_like_indexed_local_csv_pair(analog_path: &Path, digital_path: &Path) -> bool {
        Self::local_indexed_csv_role(analog_path) == Some(LocalCsvRole::Analog)
            && Self::local_indexed_csv_role(digital_path) == Some(LocalCsvRole::Digital)
    }

    fn local_indexed_csv_role(path: &Path) -> Option<LocalCsvRole> {
        let mut reader = csv_reader_from_path_with_headers(path, false).ok()?;
        let mut header = csv::StringRecord::new();
        if !reader.read_record(&mut header).ok()? {
            return None;
        }
        let first = header.get(0)?;
        if !Self::looks_like_sequence_column(first) {
            return None;
        }
        let mut data_columns = 0_usize;
        let mut analog_columns = 0_usize;
        let mut digital_columns = 0_usize;
        for name in header.iter().skip(1) {
            let normalized = Self::normalized_local_header(name);
            if normalized.is_empty() {
                continue;
            }
            data_columns += 1;
            if normalized.starts_with("ach") {
                analog_columns += 1;
            }
            if normalized.starts_with("dch") {
                digital_columns += 1;
            }
        }
        if data_columns == 0 {
            None
        } else if analog_columns > 0 && analog_columns * 2 >= data_columns {
            Some(LocalCsvRole::Analog)
        } else if digital_columns > 0 && digital_columns * 2 >= data_columns {
            Some(LocalCsvRole::Digital)
        } else {
            None
        }
    }

    fn looks_like_sequence_column(name: &str) -> bool {
        matches!(
            Self::normalized_local_header(name).as_str(),
            "num" | "number" | "no" | "index" | "sample" | "序号" | "列号"
        )
    }

    fn normalized_local_header(name: &str) -> String {
        name.trim()
            .trim_start_matches('\u{feff}')
            .chars()
            .filter(|ch| !matches!(ch, ' ' | '_' | '-' | '(' | ')' | '[' | ']'))
            .collect::<String>()
            .to_ascii_lowercase()
    }

    fn local_csv_merge_key(stem: &str) -> String {
        let mut key = stem.to_owned();
        for token in [
            "模拟量",
            "数字量",
            "模拟",
            "数字",
            "ADATA",
            "DDATA",
            "adata",
            "ddata",
            "analog",
            "digital",
            "status",
            "logic",
            "_ai",
            "-ai",
            "_di",
            "-di",
            "ai",
            "di",
        ] {
            key = key.replace(token, "");
            key = key.replace(&token.to_ascii_uppercase(), "");
        }
        let key = key
            .chars()
            .filter(|ch| ch.is_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if key.is_empty() {
            "local_csv_pair".to_owned()
        } else {
            key
        }
    }

    fn merged_local_csv_name(analog_path: &Path, digital_path: &Path) -> String {
        let analog = analog_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("analog");
        let digital = digital_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("digital");
        let key = Self::local_csv_merge_key(analog);
        if key != "local_csv_pair" {
            key
        } else {
            format!("{analog}+{digital}")
        }
    }

    fn looks_like_cloud_csv(path: &Path) -> Result<bool, String> {
        let mut reader =
            csv_reader_from_path_with_headers(path, true).map_err(|error| error.to_string())?;
        let headers = reader.headers().map_err(|error| error.to_string())?;
        if headers.is_empty() {
            return Err("Empty CSV: missing header or data.".to_owned());
        }
        Ok(headers.iter().any(|header| {
            header
                .trim_start_matches('\u{feff}')
                .eq_ignore_ascii_case("Content")
        }))
    }

    fn current_names_config(&self) -> NamesConfig {
        NamesConfig {
            display_names: self.display_names.clone(),
        }
    }

    fn current_display_config(&self) -> DisplayConfig {
        DisplayConfig {
            channel_colors: self
                .channel_colors
                .iter()
                .map(|color| color.to_array())
                .collect(),
            line_widths: self.line_widths.clone(),
            line_patterns: self.line_patterns.clone(),
            channel_scales: self.channel_scales.clone(),
            channel_panes: self.channel_panes.clone(),
            derived_visible: self.derived_visible.clone(),
            derived_colors: self
                .derived_colors
                .iter()
                .map(|color| color.to_array())
                .collect(),
            derived_line_patterns: self.derived_line_patterns.clone(),
            derived_panes: self.derived_panes.clone(),
            pll_sync_source: self.pll_sync_source,
            pll_source_channels: self.pll_source_channels,
            dq_source_channels: self.dq_source_channels,
            time_sync_source_channels: self.time_sync_source_channels,
            fft_channel: self.fft_channel,
            wheel_zoom_sensitivity: self.wheel_zoom_sensitivity,
            sample_rate_hz: self.sample_rate_hz,
            harmonic_base_hz: self.harmonic_base_hz,
            scope_layout_rows: self.scope_layout_rows,
            scope_layout_cols: self.scope_layout_cols,
            language: self.language,
            theme_mode: self.theme_mode,
            export_arrow_size: self.export_arrow_size,
            export_arrow_color_style: self.export_arrow_color_style,
            export_style_preset: ExportStylePreset::Screenshot,
            export_pane_scope: self.export_pane_scope,
            export_time_range_mode: self.export_time_range_mode,
            export_manual_start: self.export_manual_start,
            export_manual_end: self.export_manual_end,
            export_arrow_line_style: self.export_arrow_line_style,
            export_manual_arrow_head_enabled: self.export_manual_arrow_head_enabled,
            export_arrow_custom_color: self.export_arrow_custom_color.to_array(),
            export_label_scale: self.export_label_scale,
            export_label_font_style: self.export_label_font_style,
            export_resolution: self.export_resolution,
            export_dpi: self.export_dpi,
            export_dpi_value: self.export_dpi_value(),
            export_image_format: self.export_image_format,
            export_annotations_visible: self.export_annotations_visible,
            export_auto_variable_labels_enabled: self.export_auto_variable_labels_enabled,
            export_cursor_table_enabled: self.export_cursor_table_enabled,
        }
    }

    fn current_dataset_config(&self) -> DatasetConfig {
        DatasetConfig {
            primary_dataset_name: self.primary_dataset_name.clone(),
            primary_visible: self.visible.clone(),
            primary_line_pattern: self.dataset_line_pattern(0),
            sync_time_axes: self.sync_time_axes,
            time_sync_source_channels: self.time_sync_source_channels,
            imported: self
                .imported_datasets
                .iter()
                .map(|dataset| DatasetGroupConfig {
                    display_name: dataset.display_name.clone(),
                    visible: dataset.visible.clone(),
                    line_pattern: dataset.line_pattern,
                    time_offset: dataset.time_offset,
                })
                .collect(),
        }
    }

    fn current_runtime_config(&self) -> RuntimeConfig {
        RuntimeConfig {
            display_names: self.display_names.clone(),
            visible: self.visible.clone(),
            channel_colors: self
                .channel_colors
                .iter()
                .map(|color| color.to_array())
                .collect(),
            line_widths: self.line_widths.clone(),
            line_patterns: self.line_patterns.clone(),
            channel_scales: self.channel_scales.clone(),
            channel_panes: self.channel_panes.clone(),
            derived_visible: self.derived_visible.clone(),
            derived_colors: self
                .derived_colors
                .iter()
                .map(|color| color.to_array())
                .collect(),
            derived_line_patterns: self.derived_line_patterns.clone(),
            derived_panes: self.derived_panes.clone(),
            pll_sync_source: self.pll_sync_source,
            pll_source_channels: self.pll_source_channels,
            dq_source_channels: self.dq_source_channels,
            time_sync_source_channels: self.time_sync_source_channels,
            fft_channel: self.fft_channel,
            wheel_zoom_sensitivity: self.wheel_zoom_sensitivity,
            sample_rate_hz: self.sample_rate_hz,
            harmonic_base_hz: self.harmonic_base_hz,
            scope_layout_rows: self.scope_layout_rows,
            scope_layout_cols: self.scope_layout_cols,
            language: self.language,
            theme_mode: self.theme_mode,
            shortcuts: self.shortcuts,
            export_arrow_size: self.export_arrow_size,
            export_arrow_color_style: self.export_arrow_color_style,
            export_style_preset: ExportStylePreset::Screenshot,
            export_pane_scope: self.export_pane_scope,
            export_time_range_mode: self.export_time_range_mode,
            export_manual_start: self.export_manual_start,
            export_manual_end: self.export_manual_end,
            export_arrow_line_style: self.export_arrow_line_style,
            export_manual_arrow_head_enabled: self.export_manual_arrow_head_enabled,
            export_arrow_custom_color: self.export_arrow_custom_color.to_array(),
            export_label_scale: self.export_label_scale,
            export_label_font_style: self.export_label_font_style,
            export_resolution: self.export_resolution,
            export_dpi: self.export_dpi,
            export_dpi_value: self.export_dpi_value(),
            export_image_format: self.export_image_format,
            export_annotations_visible: self.export_annotations_visible,
            export_auto_variable_labels_enabled: self.export_auto_variable_labels_enabled,
            export_cursor_table_enabled: self.export_cursor_table_enabled,
        }
    }

    fn apply_names_config(&mut self, config: NamesConfig) {
        self.apply_display_names(config.display_names);
    }

    fn apply_display_config(&mut self, config: DisplayConfig) {
        let old_scales = self.channel_scales.clone();
        let old_derived_visible = self.derived_visible.clone();
        let old_pll_source_channels = self.pll_source_channels;
        let old_dq_source_channels = self.dq_source_channels;
        let old_time_sync_source_channels = self.time_sync_source_channels;
        let old_sample_rate_hz = self.sample_rate_hz;
        let old_harmonic_base_hz = self.harmonic_base_hz;
        let old_fft_channel = self.fft_channel;
        for (index, color) in config
            .channel_colors
            .into_iter()
            .enumerate()
            .take(self.channel_colors.len())
        {
            self.channel_colors[index] =
                Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]);
        }
        for (index, width) in config
            .line_widths
            .into_iter()
            .enumerate()
            .take(self.line_widths.len())
        {
            self.line_widths[index] = width.clamp(MIN_CHANNEL_LINE_WIDTH, MAX_CHANNEL_LINE_WIDTH);
        }
        for (index, pattern) in config
            .line_patterns
            .into_iter()
            .enumerate()
            .take(self.line_patterns.len())
        {
            self.line_patterns[index] = pattern;
        }
        for (index, scale) in config
            .channel_scales
            .into_iter()
            .enumerate()
            .take(self.channel_scales.len())
        {
            self.channel_scales[index] = Self::sanitize_channel_scale(scale);
        }

        self.scope_layout_rows = config.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS);
        self.scope_layout_cols = config.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS);
        let pane_count = self.scope_pane_count();
        for (index, pane) in config
            .channel_panes
            .into_iter()
            .enumerate()
            .take(self.channel_panes.len())
        {
            self.channel_panes[index] = pane.min(pane_count.saturating_sub(1));
        }
        for (index, visible) in config
            .derived_visible
            .into_iter()
            .enumerate()
            .take(self.derived_visible.len())
        {
            self.derived_visible[index] = visible;
        }
        for (index, color) in config
            .derived_colors
            .into_iter()
            .enumerate()
            .take(self.derived_colors.len())
        {
            self.derived_colors[index] =
                Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]);
        }
        for (index, pattern) in config
            .derived_line_patterns
            .into_iter()
            .enumerate()
            .take(self.derived_line_patterns.len())
        {
            self.derived_line_patterns[index] = pattern;
        }
        for (index, pane) in config
            .derived_panes
            .into_iter()
            .enumerate()
            .take(self.derived_panes.len())
        {
            self.derived_panes[index] = pane.min(pane_count.saturating_sub(1));
        }

        self.pll_sync_source = config.pll_sync_source;
        let channel_options = self.fft_channel_options();
        self.pll_source_channels =
            if Self::valid_three_phase_selection(config.pll_source_channels, &channel_options) {
                config.pll_source_channels
            } else {
                self.preferred_pll_source_channels(&channel_options)
                    .or_else(|| Self::default_sequence_channels_from_options(&channel_options))
                    .unwrap_or(self.pll_source_channels)
            };
        self.dq_source_channels = config.dq_source_channels;
        let time_sync_options = self.primary_time_sync_channel_options();
        self.time_sync_source_channels = if Self::valid_three_phase_selection(
            config.time_sync_source_channels,
            &time_sync_options,
        ) {
            config.time_sync_source_channels
        } else {
            self.preferred_time_sync_source_channels(&time_sync_options)
                .or_else(|| Self::default_sequence_channels_from_options(&time_sync_options))
                .unwrap_or(self.time_sync_source_channels)
        };
        let channel_count = self.display_names.len();
        if channel_count > 0 {
            self.fft_channel = config.fft_channel.min(channel_count - 1);
        }
        self.wheel_zoom_sensitivity = config
            .wheel_zoom_sensitivity
            .clamp(MIN_WHEEL_ZOOM_SENSITIVITY, MAX_WHEEL_ZOOM_SENSITIVITY);
        self.sample_rate_hz = config.sample_rate_hz.clamp(1.0, 10_000_000.0);
        self.harmonic_base_hz = config.harmonic_base_hz.clamp(0.001, 10_000_000.0);
        self.language = config.language;
        self.theme_mode = config.theme_mode;
        self.export_arrow_size = config
            .export_arrow_size
            .clamp(MIN_EXPORT_ARROW_SIZE, MAX_EXPORT_ARROW_SIZE);
        self.export_arrow_color_style = config.export_arrow_color_style;
        let _ = config.export_style_preset;
        self.export_style_preset = ExportStylePreset::Screenshot;
        self.export_pane_scope = config.export_pane_scope;
        self.export_time_range_mode = config.export_time_range_mode;
        self.export_manual_start = config.export_manual_start;
        self.export_manual_end = config.export_manual_end;
        self.export_arrow_line_style = config.export_arrow_line_style;
        self.export_manual_arrow_head_enabled = config.export_manual_arrow_head_enabled;
        self.export_arrow_custom_color = Color32::from_rgba_premultiplied(
            config.export_arrow_custom_color[0],
            config.export_arrow_custom_color[1],
            config.export_arrow_custom_color[2],
            config.export_arrow_custom_color[3],
        );
        self.export_label_scale = Self::effective_export_label_scale(config.export_label_scale);
        self.export_label_font_style = config.export_label_font_style;
        self.export_resolution = config.export_resolution;
        self.export_dpi = config.export_dpi;
        self.export_dpi_value = config.export_dpi_value.clamp(50, 2400);
        self.sync_export_resolution_to_dpi();
        self.export_image_format = config.export_image_format;
        self.export_annotations_visible = config.export_annotations_visible;
        self.export_auto_variable_labels_enabled = config.export_auto_variable_labels_enabled;
        self.export_cursor_table_enabled = config.export_cursor_table_enabled;

        self.hovered_channel = None;
        let scale_changed = old_scales != self.channel_scales;
        let derived_input_changed = old_derived_visible != self.derived_visible
            || old_pll_source_channels != self.pll_source_channels
            || old_dq_source_channels != self.dq_source_channels
            || (old_sample_rate_hz - self.sample_rate_hz).abs() > f64::EPSILON
            || (old_harmonic_base_hz - self.harmonic_base_hz).abs() > f64::EPSILON;
        let fft_input_changed = scale_changed
            || old_fft_channel != self.fft_channel
            || (old_sample_rate_hz - self.sample_rate_hz).abs() > f64::EPSILON
            || (old_harmonic_base_hz - self.harmonic_base_hz).abs() > f64::EPSILON;
        if scale_changed {
            self.clear_y_overrides();
            self.needs_plot_reload = true;
            self.needs_compare_plot_reload = true;
            self.measurement_cache = None;
            self.derived_measurement_cache = None;
        }
        if derived_input_changed {
            self.needs_derived_reload = true;
            self.derived_measurement_cache = None;
        }
        if fft_input_changed {
            self.needs_fft_reload = true;
        }
        if old_time_sync_source_channels != self.time_sync_source_channels {
            self.time_sync_status.clear();
        }
        self.fft_channel_user_selected = false;
    }

    fn apply_dataset_config(&mut self, config: DatasetConfig) {
        let old_primary_visible = self.visible.clone();
        let old_sync_time_axes = self.sync_time_axes;
        let old_imported_state = self
            .imported_datasets
            .iter()
            .map(|dataset| {
                (
                    dataset.visible.clone(),
                    dataset.time_offset.to_bits(),
                    dataset.plot_cache_key.clone(),
                )
            })
            .collect::<Vec<_>>();

        self.primary_dataset_name = config.primary_dataset_name;
        for (index, visible) in config
            .primary_visible
            .into_iter()
            .enumerate()
            .take(self.visible.len())
        {
            self.visible[index] = visible;
        }
        self.set_dataset_line_pattern(0, config.primary_line_pattern);
        self.sync_time_axes = config.sync_time_axes;
        let time_sync_options = self.primary_time_sync_channel_options();
        self.time_sync_source_channels = if Self::valid_three_phase_selection(
            config.time_sync_source_channels,
            &time_sync_options,
        ) {
            config.time_sync_source_channels
        } else {
            self.preferred_time_sync_source_channels(&time_sync_options)
                .or_else(|| Self::default_sequence_channels_from_options(&time_sync_options))
                .unwrap_or(self.time_sync_source_channels)
        };
        self.time_sync_source_channels_user_selected = true;

        for (index, group) in config.imported.into_iter().enumerate() {
            if let Some(dataset) = self.imported_datasets.get_mut(index) {
                dataset.display_name = group.display_name;
                for (channel_index, visible) in group
                    .visible
                    .into_iter()
                    .enumerate()
                    .take(dataset.visible.len())
                {
                    dataset.visible[channel_index] = visible;
                }
                dataset.line_pattern = group.line_pattern;
                dataset.time_offset = group.time_offset;
            }
        }

        let primary_visibility_changed = old_primary_visible != self.visible;
        let imported_data_changed = old_sync_time_axes != self.sync_time_axes
            || self
                .imported_datasets
                .iter()
                .enumerate()
                .any(|(index, dataset)| {
                    old_imported_state
                        .get(index)
                        .is_some_and(|(visible, time_offset_bits, _)| {
                            *visible != dataset.visible
                                || *time_offset_bits != dataset.time_offset.to_bits()
                        })
                });
        if imported_data_changed {
            let sync_time_axes = self.sync_time_axes;
            for (index, dataset) in self.imported_datasets.iter_mut().enumerate() {
                if old_imported_state.get(index).is_some_and(
                    |(visible, time_offset_bits, plot_key)| {
                        *visible != dataset.visible
                            || *time_offset_bits != dataset.time_offset.to_bits()
                            || old_sync_time_axes != sync_time_axes
                                && plot_key.as_ref() == dataset.plot_cache_key.as_ref()
                    },
                ) {
                    dataset.plot_cache = SampleBlock::default();
                    dataset.plot_summary = None;
                    dataset.prepared_plot_cache = PreparedPlotSeries::default();
                    dataset.prepared_plot_summary = None;
                    dataset.plot_cache_key = None;
                }
            }
        }

        self.time_sync_status.clear();
        if primary_visibility_changed || imported_data_changed {
            self.clear_y_overrides();
            self.pane_y_bounds_cache_key = None;
        }
        if primary_visibility_changed {
            self.needs_plot_reload = true;
            self.needs_fft_reload = true;
            self.measurement_cache = None;
            self.derived_measurement_cache = None;
            self.fft_results.clear();
            self.fft_channel_user_selected = false;
        }
        if primary_visibility_changed || imported_data_changed {
            self.needs_compare_plot_reload = true;
        }
    }

    fn apply_display_names(&mut self, display_names: Vec<String>) {
        let channel_count = self.display_names.len();
        for (index, name) in display_names.into_iter().enumerate().take(channel_count) {
            self.display_names[index] = name;
        }
    }

    fn apply_runtime_config(&mut self, config: RuntimeConfig) {
        let old_visible = self.visible.clone();
        let old_scales = self.channel_scales.clone();
        let old_derived_visible = self.derived_visible.clone();
        let old_pll_source_channels = self.pll_source_channels;
        let old_dq_source_channels = self.dq_source_channels;
        let old_time_sync_source_channels = self.time_sync_source_channels;
        let old_sample_rate_hz = self.sample_rate_hz;
        let old_harmonic_base_hz = self.harmonic_base_hz;
        let old_fft_channel = self.fft_channel;
        self.apply_display_names(config.display_names.clone());
        self.language = config.language;
        self.theme_mode = config.theme_mode;
        self.shortcuts = config.shortcuts;
        let channel_count = self.display_names.len();
        for (index, visible) in config
            .visible
            .into_iter()
            .enumerate()
            .take(self.visible.len())
        {
            self.visible[index] = visible;
        }
        for (index, color) in config
            .channel_colors
            .into_iter()
            .enumerate()
            .take(self.channel_colors.len())
        {
            self.channel_colors[index] =
                Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]);
        }
        for (index, width) in config
            .line_widths
            .into_iter()
            .enumerate()
            .take(self.line_widths.len())
        {
            self.line_widths[index] = width.clamp(MIN_CHANNEL_LINE_WIDTH, MAX_CHANNEL_LINE_WIDTH);
        }
        for (index, pattern) in config
            .line_patterns
            .into_iter()
            .enumerate()
            .take(self.line_patterns.len())
        {
            self.line_patterns[index] = pattern;
        }
        for (index, scale) in config
            .channel_scales
            .into_iter()
            .enumerate()
            .take(self.channel_scales.len())
        {
            self.channel_scales[index] = Self::sanitize_channel_scale(scale);
        }
        let pane_count = self.scope_pane_count();
        for (index, pane) in config
            .channel_panes
            .into_iter()
            .enumerate()
            .take(self.channel_panes.len())
        {
            self.channel_panes[index] = pane.min(pane_count.saturating_sub(1));
        }
        for (index, visible) in config
            .derived_visible
            .into_iter()
            .enumerate()
            .take(self.derived_visible.len())
        {
            self.derived_visible[index] = visible;
        }
        for (index, color) in config
            .derived_colors
            .into_iter()
            .enumerate()
            .take(self.derived_colors.len())
        {
            self.derived_colors[index] =
                Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]);
        }
        for (index, pattern) in config
            .derived_line_patterns
            .into_iter()
            .enumerate()
            .take(self.derived_line_patterns.len())
        {
            self.derived_line_patterns[index] = pattern;
        }
        for (index, pane) in config
            .derived_panes
            .into_iter()
            .enumerate()
            .take(self.derived_panes.len())
        {
            self.derived_panes[index] = pane.min(pane_count.saturating_sub(1));
        }
        self.pll_sync_source = config.pll_sync_source;
        let channel_options = self.fft_channel_options();
        self.pll_source_channels =
            if Self::valid_three_phase_selection(config.pll_source_channels, &channel_options) {
                config.pll_source_channels
            } else {
                self.preferred_pll_source_channels(&channel_options)
                    .or_else(|| Self::default_sequence_channels_from_options(&channel_options))
                    .unwrap_or(self.pll_source_channels)
            };
        self.dq_source_channels = config.dq_source_channels;
        self.dq_source_channels_user_selected = true;
        let time_sync_options = self.primary_time_sync_channel_options();
        self.time_sync_source_channels = if Self::valid_three_phase_selection(
            config.time_sync_source_channels,
            &time_sync_options,
        ) {
            config.time_sync_source_channels
        } else {
            self.preferred_time_sync_source_channels(&time_sync_options)
                .or_else(|| Self::default_sequence_channels_from_options(&time_sync_options))
                .unwrap_or(self.time_sync_source_channels)
        };
        self.time_sync_source_channels_user_selected = true;
        if channel_count > 0 {
            self.fft_channel = config.fft_channel.min(channel_count - 1);
        }
        self.fft_channel_user_selected = false;
        self.wheel_zoom_sensitivity = config
            .wheel_zoom_sensitivity
            .clamp(MIN_WHEEL_ZOOM_SENSITIVITY, MAX_WHEEL_ZOOM_SENSITIVITY);
        self.sample_rate_hz = config.sample_rate_hz.clamp(1.0, 10_000_000.0);
        self.harmonic_base_hz = config.harmonic_base_hz.clamp(0.001, 10_000_000.0);
        self.scope_layout_rows = config.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS);
        self.scope_layout_cols = config.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS);
        self.export_arrow_size = config
            .export_arrow_size
            .clamp(MIN_EXPORT_ARROW_SIZE, MAX_EXPORT_ARROW_SIZE);
        self.export_arrow_color_style = config.export_arrow_color_style;
        let _ = config.export_style_preset;
        self.export_style_preset = ExportStylePreset::Screenshot;
        self.export_pane_scope = config.export_pane_scope;
        self.export_time_range_mode = config.export_time_range_mode;
        self.export_manual_start = config.export_manual_start;
        self.export_manual_end = config.export_manual_end;
        self.export_arrow_line_style = config.export_arrow_line_style;
        self.export_manual_arrow_head_enabled = config.export_manual_arrow_head_enabled;
        self.export_arrow_custom_color = Color32::from_rgba_premultiplied(
            config.export_arrow_custom_color[0],
            config.export_arrow_custom_color[1],
            config.export_arrow_custom_color[2],
            config.export_arrow_custom_color[3],
        );
        self.export_label_scale = Self::effective_export_label_scale(config.export_label_scale);
        self.export_label_font_style = config.export_label_font_style;
        self.export_resolution = config.export_resolution;
        self.export_dpi = config.export_dpi;
        self.export_dpi_value = config.export_dpi_value.clamp(50, 2400);
        self.sync_export_resolution_to_dpi();
        self.export_image_format = config.export_image_format;
        self.export_annotations_visible = config.export_annotations_visible;
        self.export_auto_variable_labels_enabled = config.export_auto_variable_labels_enabled;
        self.export_cursor_table_enabled = config.export_cursor_table_enabled;
        self.hovered_channel = None;
        let visibility_changed = old_visible != self.visible;
        let scale_changed = old_scales != self.channel_scales;
        let derived_input_changed = old_derived_visible != self.derived_visible
            || old_pll_source_channels != self.pll_source_channels
            || old_dq_source_channels != self.dq_source_channels
            || (old_sample_rate_hz - self.sample_rate_hz).abs() > f64::EPSILON
            || (old_harmonic_base_hz - self.harmonic_base_hz).abs() > f64::EPSILON;
        let fft_input_changed = scale_changed
            || old_fft_channel != self.fft_channel
            || (old_sample_rate_hz - self.sample_rate_hz).abs() > f64::EPSILON
            || (old_harmonic_base_hz - self.harmonic_base_hz).abs() > f64::EPSILON;
        if visibility_changed || scale_changed {
            self.clear_y_overrides();
            self.needs_plot_reload = true;
            self.needs_compare_plot_reload = true;
            self.measurement_cache = None;
            self.derived_measurement_cache = None;
        }
        if derived_input_changed || visibility_changed || scale_changed {
            self.needs_derived_reload = true;
            self.derived_measurement_cache = None;
        }
        if fft_input_changed {
            self.needs_fft_reload = true;
        }
        if old_time_sync_source_channels != self.time_sync_source_channels {
            self.time_sync_status.clear();
        }
    }

    fn reload_cloud_with_current_sample_rate(&mut self) {
        self.needs_fft_reload = true;
        let main_cloud_path = (self.source_kind == Some(SourceKind::Cloud))
            .then(|| self.loaded_path.clone())
            .flatten();
        let imported_cloud_paths = self
            .imported_datasets
            .iter()
            .enumerate()
            .filter(|(_, dataset)| dataset.kind == SourceKind::Cloud)
            .map(|(index, dataset)| (index, dataset.path.clone()))
            .collect::<Vec<_>>();
        if main_cloud_path.is_none() && imported_cloud_paths.is_empty() {
            self.needs_fft_reload = true;
            return;
        }
        let config = self.current_runtime_config();
        let primary_dataset_name = self.primary_dataset_name.clone();
        if let Some(path) = main_cloud_path {
            match CloudCsvDataSource::open_with_sample_rate(&path, self.sample_rate_hz) {
                Ok(source) => {
                    self.set_source(Arc::new(source), path, SourceKind::Cloud);
                    self.apply_runtime_config(config);
                    self.primary_dataset_name = primary_dataset_name;
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
        for (index, path) in imported_cloud_paths {
            match CloudCsvDataSource::open_with_sample_rate(&path, self.sample_rate_hz) {
                Ok(source) => {
                    if let Some(dataset) = self.imported_datasets.get_mut(index) {
                        dataset.source = Arc::new(source);
                        dataset.plot_cache = SampleBlock::default();
                        dataset.plot_summary = None;
                        dataset.prepared_plot_cache = PreparedPlotSeries::default();
                        dataset.prepared_plot_summary = None;
                    }
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
        self.needs_compare_plot_reload = true;
    }

    fn export_dataset(
        &mut self,
        dataset_index: usize,
        format: DatasetExportFormat,
        visible_only: bool,
    ) {
        let Some(source) = self.dataset_source_by_index(dataset_index) else {
            self.last_error = Some(self.tr("请先导入数据。", "Import data first.").to_owned());
            return;
        };
        let Some(meta) = self.dataset_meta_by_index(dataset_index).cloned() else {
            self.last_error = Some(self.tr("请先导入数据。", "Import data first.").to_owned());
            return;
        };
        if meta.channels.is_empty() || meta.sample_count == 0 {
            self.last_error = Some(
                self.tr("当前数据为空，无法导出。", "The current dataset is empty.")
                    .to_owned(),
            );
            return;
        }

        let default_name = self.default_export_file_name(dataset_index, &meta, format);
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter(format.filter_name(self.language), &[format.extension()])
            .set_file_name(default_name)
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension(format.extension());
        }
        let channels = self.export_channels_for_dataset(dataset_index, visible_only);
        if channels.is_empty() {
            self.last_error = Some(
                self.tr(
                    "没有可导出的通道。",
                    "No channels are available for export.",
                )
                .to_owned(),
            );
            return;
        }

        match self.write_dataset_export(
            source,
            &meta,
            dataset_index,
            &channels,
            &path,
            format,
            meta.start_time,
            meta.end_time,
        ) {
            Ok(rows) => {
                tracing::info!("exported {rows} rows to {}", path.display());
                self.last_error = Some(Self::export_finished_message(rows, self.language));
            }
            Err(error) => {
                self.last_error = Some(Self::export_failed_message(&error, self.language));
            }
        }
    }

    fn default_export_file_name(
        &self,
        dataset_index: usize,
        meta: &DatasetMeta,
        format: DatasetExportFormat,
    ) -> String {
        let stem = self
            .dataset_path_by_index(dataset_index)
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .unwrap_or(&meta.source_name);
        format!("{stem}{}.{}", format.suffix(), format.extension())
    }

    fn export_cursor_range_dataset_channels(
        &mut self,
        dataset_index: usize,
        format: DatasetExportFormat,
        visible_only: bool,
    ) {
        let Some((start_time, end_time)) = self.cursor_export_range_for_dataset(dataset_index)
        else {
            self.last_error = Some(
                self.tr("请选择有效的 X1/X2 区间。", "Select a valid X1/X2 range.")
                    .to_owned(),
            );
            return;
        };
        let Some(source) = self.dataset_source_by_index(dataset_index) else {
            self.last_error = Some(self.tr("请先导入数据。", "Import data first.").to_owned());
            return;
        };
        let Some(meta) = self.dataset_meta_by_index(dataset_index).cloned() else {
            self.last_error = Some(self.tr("请先导入数据。", "Import data first.").to_owned());
            return;
        };
        if meta.channels.is_empty() || meta.sample_count == 0 {
            self.last_error = Some(
                self.tr("当前数据为空，无法导出。", "The current dataset is empty.")
                    .to_owned(),
            );
            return;
        }
        let channels = self.export_channels_for_dataset(dataset_index, visible_only);
        if channels.is_empty() {
            self.last_error = Some(
                self.tr(
                    "没有可导出的通道。",
                    "No channels are available for export.",
                )
                .to_owned(),
            );
            return;
        }

        let default_name =
            self.default_range_export_file_name(dataset_index, &meta, format, start_time, end_time);
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter(format.filter_name(self.language), &[format.extension()])
            .set_file_name(default_name)
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension(format.extension());
        }

        match self.write_dataset_export(
            source,
            &meta,
            dataset_index,
            &channels,
            &path,
            format,
            start_time,
            end_time,
        ) {
            Ok(rows) => {
                tracing::info!(
                    "exported {rows} split rows ({start_time:.6}..{end_time:.6}) to {}",
                    path.display()
                );
                self.last_error = Some(Self::export_cursor_finished_message(rows, self.language));
            }
            Err(error) => {
                self.last_error = Some(Self::export_cursor_failed_message(&error, self.language));
            }
        }
    }

    fn export_cursor_range_batch(
        &mut self,
        dataset_indices: Vec<usize>,
        formats: Vec<DatasetExportFormat>,
        visible_only: bool,
    ) {
        let targets = dataset_indices
            .into_iter()
            .filter_map(|dataset_index| {
                let range = self.cursor_export_range_for_dataset(dataset_index)?;
                let source = self.dataset_source_by_index(dataset_index)?;
                let meta = self.dataset_meta_by_index(dataset_index)?.clone();
                let channels = self.export_channels_for_dataset(dataset_index, visible_only);
                (!channels.is_empty()).then_some((dataset_index, source, meta, range, channels))
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            self.last_error = Some(
                self.tr(
                    "没有可导出的数据组或通道。",
                    "No datasets or channels are available for export.",
                )
                .to_owned(),
            );
            return;
        }
        let Some(folder) = rfd::FileDialog::new().pick_folder() else {
            return;
        };

        let mut exported_files = 0usize;
        let mut exported_rows = 0u64;
        let mut errors = Vec::new();
        for (dataset_index, source, meta, (start_time, end_time), channels) in targets {
            for format in &formats {
                let file_name = self.default_batch_range_export_file_name(
                    dataset_index,
                    &meta,
                    *format,
                    start_time,
                    end_time,
                );
                let path = folder.join(file_name);
                match self.write_dataset_export(
                    source.clone(),
                    &meta,
                    dataset_index,
                    &channels,
                    &path,
                    *format,
                    start_time,
                    end_time,
                ) {
                    Ok(rows) => {
                        exported_files += 1;
                        exported_rows += rows;
                        tracing::info!(
                            "exported {rows} split rows ({start_time:.6}..{end_time:.6}) to {}",
                            path.display()
                        );
                    }
                    Err(error) => errors.push(format!("{}: {error}", path.display())),
                }
            }
        }

        if errors.is_empty() {
            self.last_error = Some(match self.language {
                Language::Zh => {
                    format!("已批量导出 {exported_files} 个文件，共 {exported_rows} 行。")
                }
                Language::En => {
                    format!("Exported {exported_files} files, {exported_rows} rows total.")
                }
            });
        } else {
            self.last_error = Some(match self.language {
                Language::Zh => format!(
                    "批量导出完成，但有 {} 个文件失败：\n{}",
                    errors.len(),
                    errors.join("\n")
                ),
                Language::En => format!(
                    "Batch export finished with {} failed files:\n{}",
                    errors.len(),
                    errors.join("\n")
                ),
            });
        }
    }

    fn default_range_export_file_name(
        &self,
        dataset_index: usize,
        meta: &DatasetMeta,
        format: DatasetExportFormat,
        start_time: f64,
        end_time: f64,
    ) -> String {
        let stem = self
            .dataset_path_by_index(dataset_index)
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .unwrap_or(&meta.source_name);
        format!(
            "{stem}_split_{start_time:.5}_{end_time:.5}{}.{}",
            format.suffix(),
            format.extension()
        )
    }

    fn default_batch_range_export_file_name(
        &self,
        dataset_index: usize,
        meta: &DatasetMeta,
        format: DatasetExportFormat,
        start_time: f64,
        end_time: f64,
    ) -> String {
        let dataset = Self::sanitize_file_component(&self.dataset_short_label(dataset_index));
        let base = Self::sanitize_file_component(&self.default_range_export_file_name(
            dataset_index,
            meta,
            format,
            start_time,
            end_time,
        ));
        format!("{dataset}_{base}")
    }

    fn sanitize_file_component(value: &str) -> String {
        let sanitized = value
            .chars()
            .map(|ch| match ch {
                '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                ch if ch.is_control() => '_',
                ch => ch,
            })
            .collect::<String>();
        let sanitized = sanitized.trim_matches([' ', '.']).trim();
        if sanitized.is_empty() {
            "export".to_owned()
        } else {
            sanitized.to_owned()
        }
    }

    fn export_channels_for_dataset(&self, dataset_index: usize, visible_only: bool) -> Vec<usize> {
        if visible_only {
            if dataset_index == 0 {
                self.selected_channels()
            } else {
                self.selected_imported_channels(dataset_index - 1)
            }
        } else {
            self.dataset_meta_by_index(dataset_index)
                .map(|meta| {
                    meta.channels
                        .iter()
                        .map(|channel| channel.index)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        }
    }

    fn cursor_export_scope_label(&self, visible_only: bool) -> &'static str {
        if visible_only {
            self.t(UiText::ExportCheckedChannels)
        } else {
            self.t(UiText::ExportAllChannels)
        }
    }

    fn export_data_menu(&mut self, ui: &mut egui::Ui, dataset_indices: &[usize]) {
        if dataset_indices.len() <= 1 {
            self.export_dataset_range_menu(ui, 0);
            return;
        }

        let labels = dataset_indices
            .iter()
            .map(|dataset_index| (*dataset_index, self.dataset_label(*dataset_index)))
            .collect::<Vec<_>>();
        for (dataset_index, label) in labels {
            ui.menu_button(label, |ui| {
                self.export_dataset_range_menu(ui, dataset_index);
            });
        }
    }

    fn export_dataset_range_menu(&mut self, ui: &mut egui::Ui, dataset_index: usize) {
        ui.menu_button(self.t(UiText::ExportAllRange), |ui| {
            self.export_dataset_full_scope_menu(ui, dataset_index, true);
            self.export_dataset_full_scope_menu(ui, dataset_index, false);
        });

        let cursor_range_available = self
            .cursor_export_range_for_dataset(dataset_index)
            .is_some();
        ui.add_enabled_ui(cursor_range_available, |ui| {
            ui.menu_button(self.t(UiText::ExportCursorRangeData), |ui| {
                self.cursor_export_dataset_menu(ui, dataset_index, true);
                self.cursor_export_dataset_menu(ui, dataset_index, false);
            });
        });
    }

    fn export_dataset_full_scope_menu(
        &mut self,
        ui: &mut egui::Ui,
        dataset_index: usize,
        visible_only: bool,
    ) {
        let channels = self.export_channels_for_dataset(dataset_index, visible_only);
        let label = format!(
            "{} ({})",
            self.cursor_export_scope_label(visible_only),
            channels.len()
        );
        ui.add_enabled_ui(!channels.is_empty(), |ui| {
            ui.menu_button(label, |ui| {
                for format in DatasetExportFormat::ALL {
                    if ui.button(format.label(self.language)).clicked() {
                        self.export_dataset(dataset_index, format, visible_only);
                        ui.close_menu();
                    }
                }
            });
        });
    }

    fn cursor_export_dataset_menu(
        &mut self,
        ui: &mut egui::Ui,
        dataset_index: usize,
        visible_only: bool,
    ) {
        let channels = self.export_channels_for_dataset(dataset_index, visible_only);
        let label = format!(
            "{} ({})",
            self.cursor_export_scope_label(visible_only),
            channels.len()
        );
        ui.add_enabled_ui(!channels.is_empty(), |ui| {
            ui.menu_button(label, |ui| {
                for format in DatasetExportFormat::ALL {
                    if ui.button(format.label(self.language)).clicked() {
                        self.export_cursor_range_dataset_channels(
                            dataset_index,
                            format,
                            visible_only,
                        );
                        ui.close_menu();
                    }
                }
                ui.separator();
                if ui.button(self.t(UiText::ExportAllFormats)).clicked() {
                    self.export_cursor_range_batch(
                        vec![dataset_index],
                        DatasetExportFormat::ALL.to_vec(),
                        visible_only,
                    );
                    ui.close_menu();
                }
            });
        });
    }

    fn cursor_export_batch_menu(
        &mut self,
        ui: &mut egui::Ui,
        dataset_indices: &[usize],
        visible_only: bool,
    ) {
        let channel_count = dataset_indices
            .iter()
            .map(|dataset_index| {
                self.export_channels_for_dataset(*dataset_index, visible_only)
                    .len()
            })
            .sum::<usize>();
        let label = format!(
            "{} ({})",
            self.cursor_export_scope_label(visible_only),
            channel_count
        );
        ui.add_enabled_ui(channel_count > 0, |ui| {
            ui.menu_button(label, |ui| {
                for format in DatasetExportFormat::ALL {
                    if ui.button(format.label(self.language)).clicked() {
                        self.export_cursor_range_batch(
                            dataset_indices.to_vec(),
                            vec![format],
                            visible_only,
                        );
                        ui.close_menu();
                    }
                }
                ui.separator();
                if ui.button(self.t(UiText::ExportAllFormats)).clicked() {
                    self.export_cursor_range_batch(
                        dataset_indices.to_vec(),
                        DatasetExportFormat::ALL.to_vec(),
                        visible_only,
                    );
                    ui.close_menu();
                }
            });
        });
    }

    fn export_waveform_png(&mut self) {
        if self.source.is_none() {
            self.last_error = Some(self.tr("请先导入数据。", "Import data first.").to_owned());
            return;
        }

        self.poll_plot_worker();
        self.poll_compare_plot_worker();
        self.reload_derived_curve_cache();
        if self.needs_plot_reload {
            self.reload_plot_cache(DEFAULT_PLOT_PIXEL_WIDTH);
        }
        if self.needs_compare_plot_reload {
            self.reload_compare_plot_cache(DEFAULT_PLOT_PIXEL_WIDTH);
        }
        if self.plot_worker.is_some()
            || self.compare_plot_worker.is_some()
            || self.derived_curve_worker.is_some()
        {
            self.last_error = Some(
                self.tr(
                    "波形数据正在刷新，请稍后再导出图片。",
                    "Waveform data is refreshing. Please export again in a moment.",
                )
                .to_owned(),
            );
            return;
        }

        let selections = self.current_plot_selections();
        let has_channels = !selections.primary.is_empty()
            || !selections.derived.is_empty()
            || selections
                .imported
                .iter()
                .any(|channels| !channels.is_empty());
        if !has_channels {
            self.last_error = Some(
                self.tr(
                    "请至少勾选一条曲线后再导出。",
                    "Select at least one curve to export.",
                )
                .to_owned(),
            );
            return;
        }

        let labels = self.current_export_curve_labels(&selections);
        self.export_preview_selections = Some(selections);
        self.sync_export_label_overrides(&labels);
        self.export_manual_start = self.view_start;
        self.export_manual_end = self.view_end;
        self.export_preview_undo_stack.clear();
        self.export_preview_redo_stack.clear();
        self.export_preview_zoom_mode = ExportPreviewZoomMode::Fit;
        self.export_preview_zoom_scale = 1.0;
        self.export_preview_display_scale = 1.0;
        self.show_export_preview = true;
        self.export_preview_dirty = true;
        self.export_preview_error = None;
    }

    fn open_batch_waveform_export(&mut self) {
        if self.source.is_none() {
            self.last_error = Some(self.tr("请先导入数据。", "Import data first.").to_owned());
            return;
        }
        self.poll_plot_worker();
        self.poll_compare_plot_worker();
        self.reload_derived_curve_cache();
        if self.needs_plot_reload {
            self.reload_plot_cache(DEFAULT_PLOT_PIXEL_WIDTH);
        }
        if self.needs_compare_plot_reload {
            self.reload_compare_plot_cache(DEFAULT_PLOT_PIXEL_WIDTH);
        }
        if self.plot_worker.is_some()
            || self.compare_plot_worker.is_some()
            || self.derived_curve_worker.is_some()
        {
            self.last_error = Some(
                self.tr(
                    "波形数据正在刷新，请稍后再批量导出图片。",
                    "Waveform data is refreshing; try batch export again shortly.",
                )
                .to_owned(),
            );
            return;
        }
        let selections = self.current_plot_selections();
        if Self::plot_selection_curve_count(&selections) == 0 {
            self.last_error = Some(
                self.tr(
                    "请至少勾选一条曲线后再批量导出。",
                    "Select at least one curve before batch export.",
                )
                .to_owned(),
            );
            return;
        }
        if self.batch_export_windows.is_empty() {
            self.batch_export_windows.push(BatchExportTimeWindow {
                enabled: true,
                start: self.view_start.min(self.view_end),
                end: self.view_start.max(self.view_end),
            });
            if self.show_cursor_a && self.show_cursor_b {
                let start = self.cursor_a.min(self.cursor_b);
                let end = self.cursor_a.max(self.cursor_b);
                if end > start {
                    self.batch_export_windows.push(BatchExportTimeWindow {
                        enabled: true,
                        start,
                        end,
                    });
                }
            }
        }
        self.batch_export_last_summary = None;
        self.show_batch_export = true;
    }

    fn default_waveform_png_name(&self) -> String {
        let stem = self
            .loaded_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .or_else(|| self.meta().map(|meta| meta.source_name.as_str()))
            .unwrap_or("waveform");
        format!("{stem}_waveform.png")
    }

    fn default_waveform_svg_name(&self) -> String {
        let stem = self
            .loaded_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .or_else(|| self.meta().map(|meta| meta.source_name.as_str()))
            .unwrap_or("waveform");
        format!("{stem}_waveform.svg")
    }

    fn default_waveform_image_name(&self) -> String {
        match self.export_image_format {
            ExportImageFormat::Png => self.default_waveform_png_name(),
            ExportImageFormat::Svg => self.default_waveform_svg_name(),
        }
    }

    fn default_waveform_docx_name(&self) -> String {
        let stem = self
            .loaded_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .or_else(|| self.meta().map(|meta| meta.source_name.as_str()))
            .unwrap_or("waveform");
        format!("{stem}_waveform_document.docx")
    }

    fn write_current_waveform_png(
        &self,
        path: &Path,
        selections: &PlotSelections,
    ) -> Result<(), String> {
        self.write_current_waveform_image(path, selections, ExportImageFormat::Png)
    }

    fn write_current_waveform_svg(
        &self,
        path: &Path,
        selections: &PlotSelections,
    ) -> Result<(), String> {
        self.write_current_waveform_image(path, selections, ExportImageFormat::Svg)
    }

    fn write_current_waveform_image(
        &self,
        path: &Path,
        selections: &PlotSelections,
        format: ExportImageFormat,
    ) -> Result<(), String> {
        self.render_current_waveform_image(selections, format)?
            .save(path)
    }

    fn render_current_waveform_image(
        &self,
        selections: &PlotSelections,
        format: ExportImageFormat,
    ) -> Result<RenderedWaveformImage, String> {
        match format {
            ExportImageFormat::Png => Ok(RenderedWaveformImage::Png {
                canvas: self.render_current_waveform_canvas(selections)?,
                dpi: self.export_dpi_value(),
            }),
            ExportImageFormat::Svg => Ok(RenderedWaveformImage::Svg(
                self.render_current_waveform_svg(selections)?,
            )),
        }
    }

    fn export_report_source_name(&self) -> String {
        self.loaded_path
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .or_else(|| self.meta().map(|meta| meta.source_name.as_str()))
            .unwrap_or("waveform")
            .to_owned()
    }

    fn export_report_sample_rate(&self) -> Option<String> {
        self.sample_rate_hz
            .is_finite()
            .then(|| format!("{:.3} Hz", self.sample_rate_hz))
    }

    fn export_cursor_table_for_report(
        &self,
        selections: &PlotSelections,
    ) -> Option<crate::word_export::CursorTable> {
        if !self.export_cursor_table_enabled || !(self.show_cursor_a || self.show_cursor_b) {
            return None;
        }
        let labels = self.current_export_curve_labels(selections);
        if labels.is_empty() {
            return None;
        }

        let mut headers = vec![self.tr("变量", "Variable").to_owned()];
        if self.show_cursor_a {
            headers.push("Y@X1".to_owned());
        }
        if self.show_cursor_b {
            headers.push("Y@X2".to_owned());
        }
        if self.show_cursor_a && self.show_cursor_b {
            headers.push("ΔY".to_owned());
        }

        let value_columns = headers.len().saturating_sub(1);
        let rows = labels
            .into_iter()
            .map(|label| {
                let mut row = vec![label.name];
                row.extend(std::iter::repeat("--".to_owned()).take(value_columns));
                row
            })
            .collect();
        Some(crate::word_export::CursorTable { headers, rows })
    }

    fn build_word_report_figure(
        &self,
        caption: String,
        selections: &PlotSelections,
    ) -> Result<crate::word_export::WordReportFigure, String> {
        let canvas = self.render_current_waveform_canvas(selections)?;
        let size = canvas.size();
        let png = canvas
            .encode_png_with_dpi(Some(self.export_dpi_value()))
            .map_err(|error| error.to_string())?;
        Ok(crate::word_export::WordReportFigure {
            caption,
            png,
            width_px: size[0],
            height_px: size[1],
            cursor_table: self.export_cursor_table_for_report(selections),
        })
    }

    fn build_word_report(
        &self,
        figures: Vec<crate::word_export::WordReportFigure>,
        range_summary: String,
    ) -> crate::word_export::WordReport {
        let exported_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| format!("Unix {}", duration.as_secs()))
            .unwrap_or_else(|_| "Unknown".to_owned());
        let source_name = self.export_report_source_name();
        crate::word_export::WordReport {
            title: "波形导出".to_owned(),
            experiment_name: source_name.clone(),
            source_name,
            exported_at,
            sample_rate: self.export_report_sample_rate(),
            time_range_summary: range_summary,
            include_cursor_tables: self.export_cursor_table_enabled,
            figures,
        }
    }

    fn render_current_waveform_canvas(
        &self,
        selections: &PlotSelections,
    ) -> Result<Canvas, String> {
        self.render_current_waveform_canvas_with_layout(selections)
            .map(|(canvas, _)| canvas)
    }

    fn export_render_layer_order() -> [ExportRenderLayer; 3] {
        [
            ExportRenderLayer::BaseWaveform,
            ExportRenderLayer::VariableLabels,
            ExportRenderLayer::ManualAnnotations,
        ]
    }

    fn should_draw_export_overlay_annotations(visible: bool) -> bool {
        visible
    }

    fn should_draw_export_variable_labels(
        annotations_visible: bool,
        auto_label_enabled: bool,
    ) -> bool {
        annotations_visible && auto_label_enabled
    }

    fn render_current_waveform_canvas_with_layout_shared(
        &self,
        selections: &PlotSelections,
    ) -> Result<(Canvas, Vec<ExportLabelPlacement>), String> {
        let scene = self.export_waveform_render_scene(selections)?;
        let palette = self.export_style_palette();
        let mut canvas = Canvas::new(scene.width, scene.height, palette.canvas_bg);
        let label_layout = self.render_waveform_scene_on_canvas(&mut canvas, &scene, selections);
        Ok((canvas, label_layout))
    }

    fn render_current_waveform_svg_shared(
        &self,
        selections: &PlotSelections,
    ) -> Result<SvgCanvas, String> {
        let scene = self.export_waveform_render_scene(selections)?;
        let palette = self.export_style_palette();
        let mut canvas = SvgCanvas::new(scene.width, scene.height, palette.canvas_bg);
        self.render_waveform_scene_on_canvas(&mut canvas, &scene, selections);
        Ok(canvas)
    }

    fn export_waveform_render_scene(
        &self,
        selections: &PlotSelections,
    ) -> Result<ExportWaveformRenderScene, String> {
        let (x_min, x_max) = self.export_time_range()?;
        if x_max <= x_min {
            return Err(self
                .tr("当前时间范围无效。", "The current time range is invalid.")
                .to_owned());
        }

        let source_rows = self.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS);
        let source_cols = self.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS);
        let source_pane_count = source_rows * source_cols;
        let pane_indices = self.export_pane_indices(source_pane_count);
        let export_pane_count = pane_indices.len().max(1);
        let (rows, cols) = if self.export_pane_scope == ExportPaneScope::Active {
            (1, 1)
        } else {
            (source_rows, source_cols)
        };
        let pane_selections = self.pane_plot_selections(selections, source_pane_count);
        let y_bounds = self.current_y_bounds_for_panes(&pane_selections, source_pane_count);
        let resolution_scale = self.export_resolution.scale();
        let width = self.export_resolution.width();
        let margin = 16_i32 * resolution_scale;
        let gap = 16_i32 * resolution_scale;
        let pane_w = ((width as i32 - margin * 2 - gap * (cols.saturating_sub(1) as i32))
            / cols as i32)
            .max(260);
        let cursor_rows = self.max_export_cursor_table_rows(selections, source_pane_count);
        let cursor_table_reserved =
            self.export_cursor_table_reserved_height(cursor_rows, resolution_scale);
        let default_bottom_reserved = 54_i32 * resolution_scale;
        let bottom_reserved = default_bottom_reserved.max(cursor_table_reserved);
        let pane_h = ((pane_w as f32 * 0.45).round() as i32)
            .clamp(260 * resolution_scale, 480 * resolution_scale);
        let pane_h = pane_h + (bottom_reserved - default_bottom_reserved).max(0);
        let height =
            (margin + rows as i32 * pane_h + (rows as i32 - 1) * gap + margin).max(480) as usize;
        let canvas_bounds = ClipRect {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };

        Ok(ExportWaveformRenderScene {
            x_min,
            x_max,
            source_pane_count,
            export_pane_count,
            cols,
            pane_indices,
            y_bounds,
            resolution_scale,
            width,
            height,
            margin,
            gap,
            pane_w,
            pane_h,
            bottom_reserved,
            canvas_bounds,
        })
    }

    fn render_waveform_scene_on_canvas<C: WaveformCanvas>(
        &self,
        canvas: &mut C,
        scene: &ExportWaveformRenderScene,
        selections: &PlotSelections,
    ) -> Vec<ExportLabelPlacement> {
        let _render_layer_order = Self::export_render_layer_order();
        let mut label_cursor = 0usize;
        let mut label_layout = Vec::new();
        let mut pending_label_panes = Vec::new();

        for (export_index, pane_index) in scene.pane_indices.iter().copied().enumerate() {
            let row = export_index / scene.cols;
            let col = export_index % scene.cols;
            let left = scene.margin + col as i32 * (scene.pane_w + scene.gap);
            let top = scene.margin + row as i32 * (scene.pane_h + scene.gap);
            let plot = ClipRect {
                left: left + 64 * scene.resolution_scale,
                top: top + 12 * scene.resolution_scale,
                right: left + scene.pane_w - 12 * scene.resolution_scale,
                bottom: top + scene.pane_h - scene.bottom_reserved,
            };
            let pane_title = if scene.export_pane_count > 1 {
                format!("Pane {}", pane_index + 1)
            } else {
                "Waveform".to_owned()
            };
            let bounds = scene
                .y_bounds
                .get(pane_index)
                .copied()
                .unwrap_or_else(|| Self::finalize_y_bounds(f64::INFINITY, f64::NEG_INFINITY));
            self.draw_export_pane(
                canvas,
                plot,
                pane_index,
                scene.source_pane_count,
                selections,
                bounds,
                scene.x_min,
                scene.x_max,
                &pane_title,
                &mut label_cursor,
                scene.canvas_bounds,
                &mut pending_label_panes,
            );
        }
        if Self::should_draw_export_variable_labels(
            self.export_annotations_visible,
            self.export_auto_variable_labels_enabled,
        ) {
            self.draw_export_variable_label_annotations(
                canvas,
                &mut pending_label_panes,
                &mut label_layout,
            );
        }
        if Self::should_draw_export_overlay_annotations(self.export_annotations_visible) {
            self.draw_export_text_annotations(canvas);
            self.draw_export_arrow_annotations(canvas);
            self.draw_export_rectangle_annotations(canvas);
            self.draw_export_ink_strokes(canvas);
        }

        label_layout
    }

    #[allow(unreachable_code)]
    fn render_current_waveform_canvas_with_layout(
        &self,
        selections: &PlotSelections,
    ) -> Result<(Canvas, Vec<ExportLabelPlacement>), String> {
        return self.render_current_waveform_canvas_with_layout_shared(selections);
        let _render_layer_order = Self::export_render_layer_order();
        let (x_min, x_max) = self.export_time_range()?;
        if x_max <= x_min {
            return Err(self
                .tr("当前时间范围无效。", "The current time range is invalid.")
                .to_owned());
        }

        let source_rows = self.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS);
        let source_cols = self.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS);
        let source_pane_count = source_rows * source_cols;
        let pane_indices = self.export_pane_indices(source_pane_count);
        let export_pane_count = pane_indices.len().max(1);
        let (rows, cols) = if self.export_pane_scope == ExportPaneScope::Active {
            (1, 1)
        } else {
            (source_rows, source_cols)
        };
        let pane_selections = self.pane_plot_selections(selections, source_pane_count);
        let y_bounds = self.current_y_bounds_for_panes(&pane_selections, source_pane_count);
        let resolution_scale = self.export_resolution.scale();
        let width = self.export_resolution.width();
        let margin = 16_i32 * resolution_scale;
        let gap = 16_i32 * resolution_scale;
        let pane_w = ((width as i32 - margin * 2 - gap * (cols.saturating_sub(1) as i32))
            / cols as i32)
            .max(260);
        let cursor_rows = self.max_export_cursor_table_rows(selections, source_pane_count);
        let cursor_table_reserved =
            self.export_cursor_table_reserved_height(cursor_rows, resolution_scale);
        let default_bottom_reserved = 54_i32 * resolution_scale;
        let bottom_reserved = default_bottom_reserved.max(cursor_table_reserved);
        let pane_h = ((pane_w as f32 * 0.45).round() as i32)
            .clamp(260 * resolution_scale, 480 * resolution_scale);
        let pane_h = pane_h + (bottom_reserved - default_bottom_reserved).max(0);
        let height =
            (margin + rows as i32 * pane_h + (rows as i32 - 1) * gap + margin).max(480) as usize;
        let palette = self.export_style_palette();
        let mut canvas = Canvas::new(width, height, palette.canvas_bg);
        let canvas_bounds = ClipRect {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        let mut label_cursor = 0usize;
        let mut label_layout = Vec::new();
        let mut pending_label_panes = Vec::new();

        for (export_index, pane_index) in pane_indices.into_iter().enumerate() {
            let row = export_index / cols;
            let col = export_index % cols;
            let left = margin + col as i32 * (pane_w + gap);
            let top = margin + row as i32 * (pane_h + gap);
            let plot = ClipRect {
                left: left + 64 * resolution_scale,
                top: top + 12 * resolution_scale,
                right: left + pane_w - 12 * resolution_scale,
                bottom: top + pane_h - bottom_reserved,
            };
            let pane_title = if export_pane_count > 1 {
                format!("Pane {}", pane_index + 1)
            } else {
                "Waveform".to_owned()
            };
            let bounds = y_bounds
                .get(pane_index)
                .copied()
                .unwrap_or_else(|| Self::finalize_y_bounds(f64::INFINITY, f64::NEG_INFINITY));
            self.draw_export_pane(
                &mut canvas,
                plot,
                pane_index,
                source_pane_count,
                selections,
                bounds,
                x_min,
                x_max,
                &pane_title,
                &mut label_cursor,
                canvas_bounds,
                &mut pending_label_panes,
            );
        }
        if Self::should_draw_export_variable_labels(
            self.export_annotations_visible,
            self.export_auto_variable_labels_enabled,
        ) {
            self.draw_export_variable_label_annotations(
                &mut canvas,
                &mut pending_label_panes,
                &mut label_layout,
            );
        }
        if Self::should_draw_export_overlay_annotations(self.export_annotations_visible) {
            self.draw_export_text_annotations(&mut canvas);
            self.draw_export_arrow_annotations(&mut canvas);
            self.draw_export_rectangle_annotations(&mut canvas);
            self.draw_export_ink_strokes(&mut canvas);
        }

        Ok((canvas, label_layout))
    }

    #[allow(unreachable_code)]
    fn render_current_waveform_svg(
        &self,
        selections: &PlotSelections,
    ) -> Result<SvgCanvas, String> {
        return self.render_current_waveform_svg_shared(selections);
        let (x_min, x_max) = self.export_time_range()?;
        if x_max <= x_min {
            return Err(self
                .tr("当前时间范围无效。", "The current time range is invalid.")
                .to_owned());
        }

        let source_rows = self.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS);
        let source_cols = self.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS);
        let source_pane_count = source_rows * source_cols;
        let pane_indices = self.export_pane_indices(source_pane_count);
        let export_pane_count = pane_indices.len().max(1);
        let (rows, cols) = if self.export_pane_scope == ExportPaneScope::Active {
            (1, 1)
        } else {
            (source_rows, source_cols)
        };
        let pane_selections = self.pane_plot_selections(selections, source_pane_count);
        let y_bounds = self.current_y_bounds_for_panes(&pane_selections, source_pane_count);
        let resolution_scale = self.export_resolution.scale();
        let width = self.export_resolution.width();
        let margin = 16_i32 * resolution_scale;
        let gap = 16_i32 * resolution_scale;
        let pane_w = ((width as i32 - margin * 2 - gap * (cols.saturating_sub(1) as i32))
            / cols as i32)
            .max(260);
        let cursor_rows = self.max_export_cursor_table_rows(selections, source_pane_count);
        let cursor_table_reserved =
            self.export_cursor_table_reserved_height(cursor_rows, resolution_scale);
        let default_bottom_reserved = 54_i32 * resolution_scale;
        let bottom_reserved = default_bottom_reserved.max(cursor_table_reserved);
        let pane_h = ((pane_w as f32 * 0.45).round() as i32)
            .clamp(260 * resolution_scale, 480 * resolution_scale);
        let pane_h = pane_h + (bottom_reserved - default_bottom_reserved).max(0);
        let height =
            (margin + rows as i32 * pane_h + (rows as i32 - 1) * gap + margin).max(480) as usize;
        let palette = self.export_style_palette();
        let mut canvas = SvgCanvas::new(width, height, palette.canvas_bg);
        let canvas_bounds = ClipRect {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        let mut label_cursor = 0usize;
        let mut label_layout = Vec::new();
        let mut pending_label_panes = Vec::new();

        for (export_index, pane_index) in pane_indices.into_iter().enumerate() {
            let row = export_index / cols;
            let col = export_index % cols;
            let left = margin + col as i32 * (pane_w + gap);
            let top = margin + row as i32 * (pane_h + gap);
            let plot = ClipRect {
                left: left + 64 * resolution_scale,
                top: top + 12 * resolution_scale,
                right: left + pane_w - 12 * resolution_scale,
                bottom: top + pane_h - bottom_reserved,
            };
            let pane_title = if export_pane_count > 1 {
                format!("Pane {}", pane_index + 1)
            } else {
                "Waveform".to_owned()
            };
            let bounds = y_bounds
                .get(pane_index)
                .copied()
                .unwrap_or_else(|| Self::finalize_y_bounds(f64::INFINITY, f64::NEG_INFINITY));
            self.draw_export_pane(
                &mut canvas,
                plot,
                pane_index,
                source_pane_count,
                selections,
                bounds,
                x_min,
                x_max,
                &pane_title,
                &mut label_cursor,
                canvas_bounds,
                &mut pending_label_panes,
            );
        }
        if Self::should_draw_export_variable_labels(
            self.export_annotations_visible,
            self.export_auto_variable_labels_enabled,
        ) {
            self.draw_export_variable_label_annotations(
                &mut canvas,
                &mut pending_label_panes,
                &mut label_layout,
            );
        }
        if Self::should_draw_export_overlay_annotations(self.export_annotations_visible) {
            self.draw_export_text_annotations(&mut canvas);
            self.draw_export_arrow_annotations(&mut canvas);
            self.draw_export_rectangle_annotations(&mut canvas);
            self.draw_export_ink_strokes(&mut canvas);
        }

        Ok(canvas)
    }

    fn export_pane_indices(&self, pane_count: usize) -> Vec<usize> {
        match self.export_pane_scope {
            ExportPaneScope::All => (0..pane_count).collect(),
            ExportPaneScope::Active => {
                vec![self.active_scope_pane.min(pane_count.saturating_sub(1))]
            }
        }
    }

    fn max_export_cursor_table_rows(
        &self,
        selections: &PlotSelections,
        pane_count: usize,
    ) -> usize {
        if !self.export_cursor_table_enabled || !(self.show_cursor_a || self.show_cursor_b) {
            return 0;
        }
        self.export_pane_indices(pane_count)
            .into_iter()
            .map(|pane_index| self.export_curve_count_for_pane(selections, pane_index, pane_count))
            .max()
            .unwrap_or(0)
    }

    fn export_curve_count_for_pane(
        &self,
        selections: &PlotSelections,
        pane_index: usize,
        pane_count: usize,
    ) -> usize {
        let primary = selections
            .primary
            .iter()
            .filter(|channel_index| {
                self.channel_in_scope_pane(**channel_index, pane_index, pane_count)
            })
            .count();
        let imported = selections
            .imported
            .iter()
            .flat_map(|channels| channels.iter())
            .filter(|channel_index| {
                self.channel_in_scope_pane(**channel_index, pane_index, pane_count)
            })
            .count();
        let derived = selections
            .derived
            .iter()
            .filter(|derived_index| {
                self.derived_in_scope_pane(**derived_index, pane_index, pane_count)
            })
            .count();
        primary + imported + derived
    }

    fn export_cursor_table_reserved_height(
        &self,
        curve_count: usize,
        resolution_scale: i32,
    ) -> i32 {
        if !self.export_cursor_table_enabled || curve_count == 0 {
            return 0;
        }
        let table_scale = self.export_cursor_table_text_scale();
        let table_h = Self::export_cursor_table_height(curve_count, table_scale);
        62 + table_h + 12 * resolution_scale.max(1)
    }

    fn export_cursor_table_text_scale(&self) -> i32 {
        Self::effective_export_cursor_table_scale(self.export_label_scale)
    }

    fn effective_export_label_scale(scale: i32) -> i32 {
        scale.clamp(MIN_EXPORT_LABEL_SCALE, MAX_EXPORT_LABEL_SCALE)
    }

    fn effective_export_cursor_table_scale(scale: i32) -> i32 {
        scale.clamp(1, 4)
    }

    fn export_cursor_table_height(curve_count: usize, scale: i32) -> i32 {
        let text_h = Canvas::text_height(scale);
        let row_h = text_h + 9;
        let title_h = text_h + 10;
        title_h + row_h * (curve_count as i32 + 1) + 8
    }

    fn format_export_cursor_value(value: Option<f64>) -> String {
        value
            .filter(|value| value.is_finite())
            .map(|value| format!("{value:.3}"))
            .unwrap_or_else(|| "--".to_owned())
    }

    fn export_time_range(&self) -> Result<(f64, f64), String> {
        let (start, end) = match self.export_time_range_mode {
            ExportTimeRangeMode::View => (self.view_start, self.view_end),
            ExportTimeRangeMode::Cursor => {
                if !(self.show_cursor_a && self.show_cursor_b) {
                    return Err(self
                        .tr(
                            "请先显示 X1 和 X2 光标。",
                            "Show both X1 and X2 cursors first.",
                        )
                        .to_owned());
                }
                (
                    self.cursor_a.min(self.cursor_b),
                    self.cursor_a.max(self.cursor_b),
                )
            }
            ExportTimeRangeMode::Manual => (
                self.export_manual_start.min(self.export_manual_end),
                self.export_manual_start.max(self.export_manual_end),
            ),
        };
        if !start.is_finite() || !end.is_finite() || end <= start {
            return Err(self
                .tr("导出时间范围无效。", "The export time range is invalid.")
                .to_owned());
        }
        let view_start = self.view_start.min(self.view_end);
        let view_end = self.view_start.max(self.view_end);
        let start = start.max(view_start);
        let end = end.min(view_end);
        if end <= start {
            return Err(self
                .tr(
                    "导出时间范围需要位于当前示波器视图内。",
                    "The export time range must be inside the current scope view.",
                )
                .to_owned());
        }
        Ok((start, end))
    }

    fn current_export_curve_labels(&self, selections: &PlotSelections) -> Vec<ExportCurveLabel> {
        let rows = self.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS);
        let cols = self.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS);
        let pane_count = rows * cols;
        let pane_indices = self.export_pane_indices(pane_count);
        let mut labels = Vec::new();
        for pane_index in pane_indices {
            for channel_index in &selections.primary {
                if self.channel_in_scope_pane(*channel_index, pane_index, pane_count) {
                    labels.push(ExportCurveLabel {
                        name: self.channel_name(*channel_index),
                        color: self.plot_channel_color(*channel_index, 0, pane_index, pane_count),
                        source: ExportCurveLabelSource::Primary(*channel_index),
                    });
                }
            }
            for (dataset_index, dataset) in self.imported_datasets.iter().enumerate() {
                let compare_selected = selections
                    .imported
                    .get(dataset_index)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                for channel_index in compare_selected {
                    if self.channel_in_scope_pane(*channel_index, pane_index, pane_count) {
                        labels.push(ExportCurveLabel {
                            name: format!(
                                "{}: {}",
                                dataset.display_name,
                                self.channel_name(*channel_index)
                            ),
                            color: self.plot_channel_color(
                                *channel_index,
                                dataset_index + 1,
                                pane_index,
                                pane_count,
                            ),
                            source: ExportCurveLabelSource::Imported {
                                dataset_index,
                                channel_index: *channel_index,
                            },
                        });
                    }
                }
            }
            for derived_index in &selections.derived {
                if self.derived_in_scope_pane(*derived_index, pane_index, pane_count) {
                    labels.push(ExportCurveLabel {
                        name: self.derived_channel_name(*derived_index),
                        color: self.derived_channel_color(*derived_index),
                        source: ExportCurveLabelSource::Derived(*derived_index),
                    });
                }
            }
        }
        labels
    }

    fn sync_export_label_overrides(&mut self, labels: &[ExportCurveLabel]) {
        let old = std::mem::take(&mut self.export_label_overrides);
        self.export_label_overrides = labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                old.get(index)
                    .filter(|value| !value.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| label.name.clone())
            })
            .collect();
        self.export_label_positions.resize(labels.len(), None);
        self.export_label_anchor_x.resize(labels.len(), None);
    }

    fn export_label_for(&self, index: usize, default_label: String) -> String {
        self.export_label_overrides
            .get(index)
            .filter(|label| !label.trim().is_empty())
            .cloned()
            .unwrap_or(default_label)
    }

    fn set_export_label_canvas_position(&mut self, index: usize, position: [i32; 2]) {
        if index >= self.export_label_positions.len() {
            self.export_label_positions.resize(index + 1, None);
        }
        self.export_label_positions[index] = Some(position);
    }

    fn set_export_label_anchor_x(&mut self, index: usize, x: f64) {
        if index >= self.export_label_anchor_x.len() {
            self.export_label_anchor_x.resize(index + 1, None);
        }
        self.export_label_anchor_x[index] = Some(x);
    }

    fn export_preview_state(&self) -> ExportPreviewEditState {
        ExportPreviewEditState {
            label_overrides: self.export_label_overrides.clone(),
            label_positions: self.export_label_positions.clone(),
            label_anchor_x: self.export_label_anchor_x.clone(),
            preview_selections: self.export_preview_selections.clone(),
            text_annotations: self.export_text_annotations.clone(),
            ink_strokes: self.export_ink_strokes.clone(),
            arrow_annotations: self.export_arrow_annotations.clone(),
            rectangle_annotations: self.export_rectangle_annotations.clone(),
            arrow_size: self.export_arrow_size,
            arrow_color_style: self.export_arrow_color_style,
            style_preset: ExportStylePreset::Screenshot,
            pane_scope: self.export_pane_scope,
            time_range_mode: self.export_time_range_mode,
            manual_start: self.export_manual_start,
            manual_end: self.export_manual_end,
            arrow_line_style: self.export_arrow_line_style,
            manual_arrow_head_enabled: self.export_manual_arrow_head_enabled,
            arrow_custom_color: self.export_arrow_custom_color,
            shape_kind: self.export_shape_kind,
            label_scale: self.export_label_scale,
            label_font_style: self.export_label_font_style,
            resolution: self.export_resolution,
            dpi: self.export_dpi,
            dpi_value: self.export_dpi_value(),
            image_format: self.export_image_format,
            annotations_visible: self.export_annotations_visible,
            auto_variable_labels_enabled: self.export_auto_variable_labels_enabled,
            cursor_table_enabled: self.export_cursor_table_enabled,
        }
    }

    fn restore_export_preview_state(&mut self, state: ExportPreviewEditState) {
        self.export_label_overrides = state.label_overrides;
        self.export_label_positions = state.label_positions;
        self.export_label_anchor_x = state.label_anchor_x;
        self.export_preview_selections = state.preview_selections;
        self.export_text_annotations = state.text_annotations;
        self.export_ink_strokes = state.ink_strokes;
        self.export_arrow_annotations = state.arrow_annotations;
        self.export_rectangle_annotations = state.rectangle_annotations;
        self.export_arrow_size = state.arrow_size;
        self.export_arrow_color_style = state.arrow_color_style;
        let _ = state.style_preset;
        self.export_style_preset = ExportStylePreset::Screenshot;
        self.export_pane_scope = state.pane_scope;
        self.export_time_range_mode = state.time_range_mode;
        self.export_manual_start = state.manual_start;
        self.export_manual_end = state.manual_end;
        self.export_arrow_line_style = state.arrow_line_style;
        self.export_manual_arrow_head_enabled = state.manual_arrow_head_enabled;
        self.export_arrow_custom_color = state.arrow_custom_color;
        self.export_shape_kind = state.shape_kind;
        self.export_label_scale = state.label_scale;
        self.export_label_font_style = state.label_font_style;
        self.export_resolution = state.resolution;
        self.export_dpi = state.dpi;
        self.export_dpi_value = state.dpi_value.clamp(50, 2400);
        self.sync_export_resolution_to_dpi();
        self.export_image_format = state.image_format;
        self.export_annotations_visible = state.annotations_visible;
        self.export_auto_variable_labels_enabled = state.auto_variable_labels_enabled;
        self.export_cursor_table_enabled = state.cursor_table_enabled;
        self.export_preview_drag = None;
        self.export_preview_anchor_drag = None;
        self.export_arrow_drag = None;
        self.export_rectangle_drag = None;
        self.export_annotation_selection = None;
        self.export_preview_dirty = true;
    }

    fn clear_export_annotation_interaction_state(&mut self) {
        self.export_preview_drag = None;
        self.export_preview_anchor_drag = None;
        self.export_preview_text_drag = None;
        self.export_ink_drag = None;
        self.export_arrow_drag = None;
        self.export_rectangle_drag = None;
        self.export_preview_edit_label_index = None;
        self.export_preview_edit_label_focus_pending = false;
        self.export_preview_edit_text_index = None;
        self.export_preview_edit_text_focus_pending = false;
        self.export_annotation_selection = None;
    }

    fn push_export_preview_undo(&mut self, before: ExportPreviewEditState) {
        if self.export_preview_state() == before {
            return;
        }
        const MAX_EXPORT_PREVIEW_HISTORY: usize = 80;
        self.export_preview_undo_stack.push(before);
        if self.export_preview_undo_stack.len() > MAX_EXPORT_PREVIEW_HISTORY {
            self.export_preview_undo_stack.remove(0);
        }
        self.export_preview_redo_stack.clear();
    }

    fn undo_export_preview_edit(&mut self) {
        let Some(previous) = self.export_preview_undo_stack.pop() else {
            return;
        };
        let current = self.export_preview_state();
        self.export_preview_redo_stack.push(current);
        self.restore_export_preview_state(previous);
    }

    fn redo_export_preview_edit(&mut self) {
        let Some(next) = self.export_preview_redo_stack.pop() else {
            return;
        };
        let current = self.export_preview_state();
        self.export_preview_undo_stack.push(current);
        self.restore_export_preview_state(next);
    }

    fn export_preview_plot_selections(&self) -> PlotSelections {
        self.export_preview_selections
            .clone()
            .unwrap_or_else(|| self.current_plot_selections())
    }

    fn set_export_selection_channel(
        channels: &mut Vec<usize>,
        channel_index: usize,
        selected: bool,
    ) -> bool {
        if selected {
            if channels.contains(&channel_index) {
                false
            } else {
                channels.push(channel_index);
                channels.sort_unstable();
                true
            }
        } else {
            let old_len = channels.len();
            channels.retain(|index| *index != channel_index);
            old_len != channels.len()
        }
    }

    fn remove_export_curve_label_source(
        selections: &mut PlotSelections,
        source: ExportCurveLabelSource,
    ) -> bool {
        match source {
            ExportCurveLabelSource::Primary(channel_index) => {
                Self::set_export_selection_channel(&mut selections.primary, channel_index, false)
            }
            ExportCurveLabelSource::Imported {
                dataset_index,
                channel_index,
            } => selections
                .imported
                .get_mut(dataset_index)
                .is_some_and(|channels| {
                    Self::set_export_selection_channel(channels, channel_index, false)
                }),
            ExportCurveLabelSource::Derived(derived_index) => {
                Self::set_export_selection_channel(&mut selections.derived, derived_index, false)
            }
        }
    }

    fn refresh_export_preview(&mut self, ctx: &egui::Context) {
        let selections = self.export_preview_plot_selections();
        match self.render_current_waveform_canvas_with_layout(&selections) {
            Ok((canvas, label_layout)) => {
                let size = canvas.size();
                let image = egui::ColorImage::from_rgba_unmultiplied(size, canvas.pixels());
                self.export_preview_texture = Some(ctx.load_texture(
                    "waveform_export_preview",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
                self.export_preview_size = size;
                self.export_preview_label_layout = label_layout;
                self.export_preview_error = None;
            }
            Err(error) => {
                self.export_preview_texture = None;
                self.export_preview_size = [0, 0];
                self.export_preview_label_layout.clear();
                self.export_preview_error = Some(error);
            }
        }
        self.export_preview_dirty = false;
    }

    fn mark_export_preview_dirty(&mut self) {
        if self.show_export_preview {
            self.export_preview_dirty = true;
        }
    }

    fn batch_export_window(&mut self, ctx: &egui::Context) {
        if !self.show_batch_export {
            return;
        }

        let mut open = self.show_batch_export;
        egui::Window::new(self.tr(
            "批量导出波形 / DOCX",
            "Batch Export Waveform / DOCX",
        ))
            .open(&mut open)
            .default_width(760.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(self.tr("数据组", "Datasets"));
                    egui::ComboBox::from_id_source("batch_export_dataset_mode")
                        .selected_text(self.batch_export_dataset_mode.label(self.language))
                        .show_ui(ui, |ui| {
                            for mode in BatchExportDatasetMode::ALL {
                                ui.selectable_value(
                                    &mut self.batch_export_dataset_mode,
                                    mode,
                                    mode.label(self.language),
                                );
                            }
                        });
                    ui.label(self.tr("示波器布局", "Scope panes"));
                    egui::ComboBox::from_id_source("batch_export_pane_mode")
                        .selected_text(self.batch_export_pane_mode.label(self.language))
                        .show_ui(ui, |ui| {
                            for mode in BatchExportPaneMode::ALL {
                                ui.selectable_value(
                                    &mut self.batch_export_pane_mode,
                                    mode,
                                    mode.label(self.language),
                                );
                            }
                        });
                });
                ui.label(
                    self.tr(
                        "批量导出会使用当前图片导出分辨率、DPI、箭头和标注设置。",
                        "Batch export uses the current image export resolution, DPI, arrows, and labels.",
                    ),
                );
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(self.tr("添加当前视图", "Add Current View")).clicked() {
                        self.batch_export_windows.push(BatchExportTimeWindow {
                            enabled: true,
                            start: self.view_start.min(self.view_end),
                            end: self.view_start.max(self.view_end),
                        });
                    }
                    if ui
                        .add_enabled(
                            self.show_cursor_a && self.show_cursor_b,
                            egui::Button::new(self.tr("添加 X1-X2", "Add X1-X2")),
                        )
                        .clicked()
                    {
                        self.batch_export_windows.push(BatchExportTimeWindow {
                            enabled: true,
                            start: self.cursor_a.min(self.cursor_b),
                            end: self.cursor_a.max(self.cursor_b),
                        });
                    }
                    if ui.button(self.tr("添加空窗口", "Add Window")).clicked() {
                        self.batch_export_windows.push(BatchExportTimeWindow {
                            enabled: true,
                            start: self.view_start.min(self.view_end),
                            end: self.view_start.max(self.view_end),
                        });
                    }
                });

                let mut remove_index = None;
                egui::Grid::new("batch_export_windows_grid")
                    .num_columns(5)
                    .spacing([10.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("");
                        ui.strong(self.tr("开始 X(s)", "Start X(s)"));
                        ui.strong(self.tr("结束 X(s)", "End X(s)"));
                        ui.strong(self.tr("窗口", "Window"));
                        ui.label("");
                        ui.end_row();

                        let language = self.language;
                        for (index, window) in self.batch_export_windows.iter_mut().enumerate() {
                            ui.checkbox(&mut window.enabled, "");
                            ui.add(
                                egui::DragValue::new(&mut window.start)
                                    .speed(0.001)
                                    .max_decimals(6),
                            );
                            ui.add(
                                egui::DragValue::new(&mut window.end)
                                    .speed(0.001)
                                    .max_decimals(6),
                            );
                            ui.label(format!("#{}", index + 1));
                            let remove_label = match language {
                                Language::Zh => "删除",
                                Language::En => "Remove",
                            };
                            if ui.button(remove_label).clicked() {
                                remove_index = Some(index);
                            }
                            ui.end_row();
                        }
                    });
                if let Some(index) = remove_index {
                    self.batch_export_windows.remove(index);
                }

                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .button(self.tr("选择文件夹并导出 PNG", "Choose Folder and Export PNG"))
                        .clicked()
                    {
                        self.run_batch_waveform_png_export();
                    }
                    if ui
                        .button(self.tr("选择文件并导出 DOCX", "Choose File and Export DOCX"))
                        .clicked()
                    {
                        self.run_batch_waveform_word_export();
                    }
                    if let Some(summary) = &self.batch_export_last_summary {
                        ui.label(summary);
                    }
                });
            });
        self.show_batch_export = open;
    }

    fn export_preview_window(&mut self, ctx: &egui::Context) {
        if !self.show_export_preview {
            return;
        }
        if self.export_preview_dirty {
            self.refresh_export_preview(ctx);
        }

        let mut open = self.show_export_preview;
        egui::Window::new(self.tr("导出图片预览", "Export Image Preview"))
            .open(&mut open)
            .default_width(1120.0)
            .default_height(760.0)
            .resizable(true)
            .show(ctx, |ui| {
                self.export_preview_settings_bar(ui, ctx);
                ui.separator();
                self.export_preview_tool_strip(ui, ctx);
                self.export_preview_tool_options_bar(ui, ctx);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.set_min_height(540.0);
                    ui.vertical(|ui| {
                        self.export_preview_canvas(ui, ctx);
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width(280.0);
                        self.export_preview_property_panel(ui, ctx);
                    });
                });
                ui.separator();
                self.export_preview_actions(ui);
            });
        self.show_export_preview = open;
        if !self.show_export_preview {
            self.export_preview_texture = None;
            self.export_preview_selections = None;
            self.export_preview_label_layout.clear();
            self.export_preview_drag = None;
            self.export_preview_anchor_drag = None;
            self.export_preview_text_drag = None;
            self.export_arrow_drag = None;
            self.export_rectangle_drag = None;
            self.export_preview_edit_label_index = None;
            self.export_preview_edit_label_focus_pending = false;
            self.export_preview_edit_text_index = None;
            self.export_preview_edit_text_focus_pending = false;
            self.export_annotation_selection = None;
        }
    }

    fn export_tool_icon_textures(&mut self, ctx: &egui::Context) -> ExportToolIconTextures {
        if self.export_tool_icons.is_none() {
            self.export_tool_icons = Some(ExportToolIconTextures {
                select: Self::load_export_tool_icon(
                    ctx,
                    "export-tool-select",
                    EXPORT_TOOL_ICON_SELECT,
                ),
                text: Self::load_export_tool_icon(ctx, "export-tool-text", EXPORT_TOOL_ICON_TEXT),
                arrow: Self::load_export_tool_icon(
                    ctx,
                    "export-tool-arrow",
                    EXPORT_TOOL_ICON_ARROW,
                ),
                rectangle: Self::load_export_tool_icon(
                    ctx,
                    "export-tool-rectangle",
                    EXPORT_TOOL_ICON_RECTANGLE,
                ),
                brush: Self::load_export_tool_icon(
                    ctx,
                    "export-tool-brush",
                    EXPORT_TOOL_ICON_BRUSH,
                ),
                eraser: Self::load_export_tool_icon(
                    ctx,
                    "export-tool-eraser",
                    EXPORT_TOOL_ICON_ERASER,
                ),
                undo: Self::load_export_tool_icon(ctx, "export-tool-undo", EXPORT_TOOL_ICON_UNDO),
                redo: Self::load_export_tool_icon(ctx, "export-tool-redo", EXPORT_TOOL_ICON_REDO),
            });
        }
        self.export_tool_icons
            .as_ref()
            .expect("export tool icons are initialized")
            .clone()
    }

    fn load_export_tool_icon(
        ctx: &egui::Context,
        name: &'static str,
        rgba: &'static [u8],
    ) -> egui::TextureHandle {
        debug_assert_eq!(
            rgba.len(),
            EXPORT_TOOL_ICON_SIZE[0] * EXPORT_TOOL_ICON_SIZE[1] * 4
        );
        let image = egui::ColorImage::from_rgba_unmultiplied(EXPORT_TOOL_ICON_SIZE, rgba);
        ctx.load_texture(name, image, egui::TextureOptions::LINEAR)
    }

    fn export_tool_icon_button(
        ui: &mut egui::Ui,
        texture: &egui::TextureHandle,
        selected: bool,
        tooltip: &str,
    ) -> egui::Response {
        let image = egui::Image::from_texture((
            texture.id(),
            egui::vec2(EXPORT_TOOL_ICON_DISPLAY_SIZE, EXPORT_TOOL_ICON_DISPLAY_SIZE),
        ));
        ui.add(
            egui::ImageButton::new(image)
                .selected(selected)
                .rounding(4.0),
        )
        .on_hover_text(tooltip)
    }

    fn export_action_icon_button(
        ui: &mut egui::Ui,
        icon: ExportPreviewActionIcon,
        tooltip: &str,
    ) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(40.0, 34.0), egui::Sense::click());
        let visuals = ui.style().interact(&response);
        ui.painter().rect_filled(rect, 3.0, visuals.bg_fill);
        ui.painter().rect_stroke(rect, 3.0, visuals.bg_stroke);
        let stroke = Stroke::new(2.0, visuals.text_color());
        match icon {
            ExportPreviewActionIcon::Save => {
                let body = egui::Rect::from_min_size(
                    rect.min + egui::vec2(11.0, 7.0),
                    egui::vec2(18.0, 21.0),
                );
                ui.painter().rect_stroke(body, 0.0, stroke);
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        body.min + egui::vec2(4.0, 3.0),
                        egui::vec2(8.0, 5.0),
                    ),
                    0.0,
                    visuals.text_color(),
                );
                ui.painter().rect_stroke(
                    egui::Rect::from_min_size(
                        body.min + egui::vec2(5.0, 13.0),
                        egui::vec2(8.0, 7.0),
                    ),
                    0.0,
                    stroke,
                );
            }
            ExportPreviewActionIcon::Copy => {
                let back = egui::Rect::from_min_size(
                    rect.min + egui::vec2(12.0, 8.0),
                    egui::vec2(14.0, 18.0),
                );
                let front = back.translate(egui::vec2(4.0, 3.0));
                ui.painter()
                    .rect_stroke(back, 0.0, Stroke::new(1.5, visuals.text_color()));
                ui.painter().rect_filled(front, 0.0, visuals.bg_fill);
                ui.painter().rect_stroke(front, 0.0, stroke);
            }
        }
        response.on_hover_text(tooltip)
    }

    fn export_preview_fit_scale(image_size: egui::Vec2, available: egui::Vec2) -> f32 {
        if image_size.x <= 0.0 || image_size.y <= 0.0 || available.x <= 0.0 || available.y <= 0.0 {
            return 1.0;
        }
        (available.x / image_size.x)
            .min(available.y / image_size.y)
            .clamp(
                EXPORT_PREVIEW_ZOOM_PRESETS[0],
                *EXPORT_PREVIEW_ZOOM_PRESETS.last().unwrap(),
            )
    }

    fn export_preview_display_scale(
        mode: ExportPreviewZoomMode,
        manual_scale: f32,
        image_size: egui::Vec2,
        available: egui::Vec2,
    ) -> f32 {
        match mode {
            ExportPreviewZoomMode::Fit => Self::export_preview_fit_scale(image_size, available),
            ExportPreviewZoomMode::Manual => manual_scale.clamp(
                EXPORT_PREVIEW_ZOOM_PRESETS[0],
                *EXPORT_PREVIEW_ZOOM_PRESETS.last().unwrap(),
            ),
        }
    }

    fn next_export_preview_zoom_scale(current: f32, direction: i32) -> f32 {
        if direction > 0 {
            EXPORT_PREVIEW_ZOOM_PRESETS
                .iter()
                .copied()
                .find(|preset| *preset > current + 0.001)
                .unwrap_or(*EXPORT_PREVIEW_ZOOM_PRESETS.last().unwrap())
        } else if direction < 0 {
            EXPORT_PREVIEW_ZOOM_PRESETS
                .iter()
                .rev()
                .copied()
                .find(|preset| *preset < current - 0.001)
                .unwrap_or(EXPORT_PREVIEW_ZOOM_PRESETS[0])
        } else {
            current.clamp(
                EXPORT_PREVIEW_ZOOM_PRESETS[0],
                *EXPORT_PREVIEW_ZOOM_PRESETS.last().unwrap(),
            )
        }
    }

    fn export_preview_zoom_label(scale: f32) -> String {
        format!("{:.0}%", scale * 100.0)
    }

    fn set_export_preview_manual_zoom(&mut self, scale: f32) {
        self.export_preview_zoom_mode = ExportPreviewZoomMode::Manual;
        self.export_preview_zoom_scale = scale.clamp(
            EXPORT_PREVIEW_ZOOM_PRESETS[0],
            *EXPORT_PREVIEW_ZOOM_PRESETS.last().unwrap(),
        );
    }

    fn step_export_preview_zoom(&mut self, direction: i32, current_display_scale: f32) {
        let base = if self.export_preview_zoom_mode == ExportPreviewZoomMode::Fit {
            current_display_scale
        } else {
            self.export_preview_zoom_scale
        };
        self.set_export_preview_manual_zoom(Self::next_export_preview_zoom_scale(base, direction));
    }

    fn fit_export_preview_to_canvas(&mut self) {
        self.export_preview_zoom_mode = ExportPreviewZoomMode::Fit;
    }

    fn export_preview_settings_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut changed = false;
        let before_controls = self.export_preview_state();
        let previous_resolution = self.export_resolution;
        let previous_dpi_value = self.export_dpi_value();
        let previous_pane_scope = self.export_pane_scope;
        let previous_time_range_mode = self.export_time_range_mode;
        let previous_manual_range = (self.export_manual_start, self.export_manual_end);
        let previous_annotations_visible = self.export_annotations_visible;
        let previous_auto_variable_labels_enabled = self.export_auto_variable_labels_enabled;
        let previous_cursor_table_enabled = self.export_cursor_table_enabled;

        ui.horizontal_wrapped(|ui| {
            ui.label(self.tr("子窗口", "Pane"));
            egui::ComboBox::from_id_source("export_preview_pane_scope")
                .selected_text(self.export_pane_scope.label(self.language))
                .show_ui(ui, |ui| {
                    for scope in ExportPaneScope::ALL {
                        changed |= ui
                            .selectable_value(
                                &mut self.export_pane_scope,
                                scope,
                                scope.label(self.language),
                            )
                            .changed();
                    }
                });
            ui.label(self.tr("时间范围", "Time range"));
            egui::ComboBox::from_id_source("export_preview_time_range")
                .selected_text(self.export_time_range_mode.label(self.language))
                .show_ui(ui, |ui| {
                    for mode in ExportTimeRangeMode::ALL {
                        changed |= ui
                            .selectable_value(
                                &mut self.export_time_range_mode,
                                mode,
                                mode.label(self.language),
                            )
                            .changed();
                    }
                });
            if self.export_time_range_mode == ExportTimeRangeMode::Manual {
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut self.export_manual_start)
                            .speed(0.0001)
                            .prefix("X0 "),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut self.export_manual_end)
                            .speed(0.0001)
                            .prefix("X1 "),
                    )
                    .changed();
            }
            ui.label(self.tr("导出质量", "Export quality"));
            changed |= self.export_dpi_controls(ui, "export_preview_dpi");
            let annotations_label = self.tr("显示标注", "Show annotations");
            changed |= ui
                .checkbox(&mut self.export_annotations_visible, annotations_label)
                .changed();
            let auto_label_text = self.tr("自动标注变量", "Auto variable labels");
            ui.add_enabled_ui(self.export_annotations_visible, |ui| {
                changed |= ui
                    .checkbox(
                        &mut self.export_auto_variable_labels_enabled,
                        auto_label_text,
                    )
                    .changed();
            });
            let cursor_table_label = self.tr("光标数据表", "Cursor Table");
            changed |= ui
                .checkbox(&mut self.export_cursor_table_enabled, cursor_table_label)
                .changed();
        });

        if changed {
            if self.export_resolution != previous_resolution
                || self.export_dpi_value() != previous_dpi_value
                || self.export_pane_scope != previous_pane_scope
                || self.export_time_range_mode != previous_time_range_mode
                || (self.export_manual_start, self.export_manual_end) != previous_manual_range
                || self.export_annotations_visible != previous_annotations_visible
                || self.export_auto_variable_labels_enabled != previous_auto_variable_labels_enabled
                || self.export_cursor_table_enabled != previous_cursor_table_enabled
            {
                self.export_label_positions.fill(None);
                self.export_label_anchor_x.fill(None);
            }
            if !self.export_annotations_visible {
                self.clear_export_annotation_interaction_state();
            }
            if self.export_pane_scope != previous_pane_scope {
                self.export_label_overrides.clear();
                self.export_label_positions.clear();
                self.export_label_anchor_x.clear();
            }
            self.push_export_preview_undo(before_controls);
            self.mark_export_preview_dirty();
            ctx.request_repaint();
        }
    }

    fn export_preview_tool_strip(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let icons = self.export_tool_icon_textures(ctx);
        let select_tool_label = self.tr("选择", "Select").to_owned();
        let text_tool_label = self.tr("文字", "Text").to_owned();
        let arrow_tool_label = self.tr("箭头", "Arrow").to_owned();
        let rectangle_tool_label = self.tr("矩形/椭圆", "Rectangle/Ellipse").to_owned();
        let brush_tool_label = self.tr("画笔", "Brush").to_owned();
        let eraser_tool_label = self.tr("橡皮", "Eraser").to_owned();
        let select_tooltip = format!("{select_tool_label} (Ctrl+6)");
        let text_tooltip = format!("{text_tool_label} (Ctrl+3)");
        let arrow_tooltip = format!("{arrow_tool_label} (Ctrl+2)");
        let rectangle_tooltip = format!("{rectangle_tool_label} (Ctrl+1)");
        let brush_tooltip = format!("{brush_tool_label} (Ctrl+4)");
        let eraser_tooltip = format!("{eraser_tool_label} (Ctrl+5)");
        let undo_label = self.tr("撤销", "Undo").to_owned();
        let redo_label = self.tr("恢复", "Redo").to_owned();
        let undo_tooltip = format!("{undo_label} (Ctrl+Z)");
        let redo_tooltip = format!("{redo_label} (Ctrl+Y)");
        let save_tooltip = self.tr("保存图片", "Save image").to_owned();
        let copy_tooltip = self.tr("复制到剪切板", "Copy to clipboard").to_owned();

        ui.horizontal_wrapped(|ui| {
            for action in Self::export_preview_primary_actions() {
                let (icon, tooltip) = match action {
                    ExportPreviewPrimaryAction::SaveImage => {
                        (ExportPreviewActionIcon::Save, save_tooltip.as_str())
                    }
                    ExportPreviewPrimaryAction::CopyImage => {
                        (ExportPreviewActionIcon::Copy, copy_tooltip.as_str())
                    }
                };
                if Self::export_action_icon_button(ui, icon, tooltip).clicked() {
                    match action {
                        ExportPreviewPrimaryAction::SaveImage => self.save_export_preview_image(),
                        ExportPreviewPrimaryAction::CopyImage => {
                            self.copy_export_preview_image_to_clipboard()
                        }
                    }
                    ctx.request_repaint();
                }
            }
            ui.separator();
            self.export_preview_zoom_controls(ui, ctx);
            ui.separator();
            let tools = [
                (
                    ExportPreviewTool::Select,
                    &icons.select,
                    select_tooltip.as_str(),
                ),
                (ExportPreviewTool::Text, &icons.text, text_tooltip.as_str()),
                (
                    ExportPreviewTool::Arrow,
                    &icons.arrow,
                    arrow_tooltip.as_str(),
                ),
                (
                    ExportPreviewTool::Rectangle,
                    &icons.rectangle,
                    rectangle_tooltip.as_str(),
                ),
                (
                    ExportPreviewTool::Brush,
                    &icons.brush,
                    brush_tooltip.as_str(),
                ),
                (
                    ExportPreviewTool::Eraser,
                    &icons.eraser,
                    eraser_tooltip.as_str(),
                ),
            ];
            for (tool, texture, label) in tools {
                if Self::export_tool_icon_button(
                    ui,
                    texture,
                    self.export_preview_tool == tool,
                    label,
                )
                .clicked()
                {
                    self.export_preview_tool = tool;
                    ctx.request_repaint();
                }
            }
            ui.separator();
            if ui
                .add_enabled_ui(!self.export_preview_undo_stack.is_empty(), |ui| {
                    Self::export_tool_icon_button(ui, &icons.undo, false, undo_tooltip.as_str())
                })
                .inner
                .clicked()
            {
                self.undo_export_preview_edit();
                ctx.request_repaint();
            }
            if ui
                .add_enabled_ui(!self.export_preview_redo_stack.is_empty(), |ui| {
                    Self::export_tool_icon_button(ui, &icons.redo, false, redo_tooltip.as_str())
                })
                .inner
                .clicked()
            {
                self.redo_export_preview_edit();
                ctx.request_repaint();
            }
        });
    }

    fn export_preview_zoom_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let display_scale = self.export_preview_display_scale;
        let zoom_out_label = self.tr("缩小", "Zoom out").to_owned();
        let fit_label = self.tr("适配", "Fit").to_owned();
        let zoom_in_label = self.tr("放大", "Zoom in").to_owned();
        if ui.button("-").on_hover_text(zoom_out_label).clicked() {
            self.step_export_preview_zoom(-1, display_scale);
            ctx.request_repaint();
        }
        if ui
            .selectable_label(
                self.export_preview_zoom_mode == ExportPreviewZoomMode::Fit,
                fit_label,
            )
            .clicked()
        {
            self.fit_export_preview_to_canvas();
            ctx.request_repaint();
        }
        if ui.button("+").on_hover_text(zoom_in_label).clicked() {
            self.step_export_preview_zoom(1, display_scale);
            ctx.request_repaint();
        }
        ui.label(Self::export_preview_zoom_label(display_scale));
    }

    fn export_preview_tool_options_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let before = self.export_preview_state();
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(self.tr("线宽", "Width"));
            for width in [1, 2, 4, 6, 8, 12] {
                let selected = self.export_brush_width == width;
                if Self::export_width_dot_button(ui, width, selected).clicked() {
                    self.export_brush_width = width;
                    changed = true;
                }
            }
            ui.separator();
            if self.export_preview_tool == ExportPreviewTool::Rectangle {
                ui.label(self.tr("形状", "Shape"));
                for shape in ShapeKind::ALL {
                    changed |= ui
                        .selectable_value(
                            &mut self.export_shape_kind,
                            shape,
                            shape.label(self.language),
                        )
                        .changed();
                }
                ui.separator();
            }
            if matches!(
                self.export_preview_tool,
                ExportPreviewTool::Arrow | ExportPreviewTool::Rectangle
            ) {
                changed |= Self::export_line_style_dropdown(
                    ui,
                    &mut self.export_arrow_line_style,
                    "export_preview_top_arrow_line",
                );
                if self.export_preview_tool == ExportPreviewTool::Arrow {
                    let arrow_head_tooltip = self
                        .tr("开/关箭头 (Tab)", "Toggle arrow head (Tab)")
                        .to_owned();
                    changed |= Self::export_arrow_head_toggle_button(
                        ui,
                        &mut self.export_manual_arrow_head_enabled,
                        arrow_head_tooltip.as_str(),
                    );
                }
                ui.separator();
            }
            if matches!(
                self.export_preview_tool,
                ExportPreviewTool::Arrow | ExportPreviewTool::Select
            ) {
                ui.label(self.tr("箭头", "Arrow"));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut self.export_arrow_size)
                            .clamp_range(MIN_EXPORT_ARROW_SIZE..=MAX_EXPORT_ARROW_SIZE)
                            .speed(0.5),
                    )
                    .changed();
                self.export_arrow_size = self
                    .export_arrow_size
                    .clamp(MIN_EXPORT_ARROW_SIZE, MAX_EXPORT_ARROW_SIZE);
                ui.separator();
            }
            if matches!(
                self.export_preview_tool,
                ExportPreviewTool::Text | ExportPreviewTool::Select
            ) {
                ui.label(self.tr("字号", "Text"));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut self.export_label_scale)
                            .clamp_range(MIN_EXPORT_LABEL_SCALE..=MAX_EXPORT_LABEL_SCALE)
                            .speed(1),
                    )
                    .changed();
                self.export_label_scale = self
                    .export_label_scale
                    .clamp(MIN_EXPORT_LABEL_SCALE, MAX_EXPORT_LABEL_SCALE);
                egui::ComboBox::from_id_source("export_preview_top_font")
                    .selected_text(self.export_label_font_style.label(self.language))
                    .show_ui(ui, |ui| {
                        for style in ExportLabelFontStyle::ALL {
                            changed |= ui
                                .selectable_value(
                                    &mut self.export_label_font_style,
                                    style,
                                    style.label(self.language),
                                )
                                .changed();
                        }
                    });
                ui.separator();
            }
            if self.export_preview_tool == ExportPreviewTool::Select {
                ui.label(self.tr("变量箭头", "Variable arrows"));
                egui::ComboBox::from_id_source("export_preview_top_arrow_color")
                    .selected_text(self.export_arrow_color_style.label(self.language))
                    .show_ui(ui, |ui| {
                        for style in ExportArrowColorStyle::ALL {
                            changed |= ui
                                .selectable_value(
                                    &mut self.export_arrow_color_style,
                                    style,
                                    style.label(self.language),
                                )
                                .changed();
                        }
                    });
                if self.export_arrow_color_style == ExportArrowColorStyle::Custom {
                    changed |= egui::color_picker::color_edit_button_srgba(
                        ui,
                        &mut self.export_arrow_custom_color,
                        egui::color_picker::Alpha::Opaque,
                    )
                    .changed();
                }
                ui.separator();
            }
            let color_tooltip = self.tr("选择颜色", "Choose color").to_owned();
            changed |= Self::export_color_palette_controls(
                ui,
                &mut self.export_brush_color,
                color_tooltip.as_str(),
            );
        });
        if changed {
            self.push_export_preview_undo(before);
            self.mark_export_preview_dirty();
            ctx.request_repaint();
        }
    }

    fn export_line_style_dropdown(
        ui: &mut egui::Ui,
        selected_style: &mut ExportArrowLineStyle,
        id: &'static str,
    ) -> bool {
        let popup_id = ui.make_persistent_id((id, "line_style_dropdown"));
        let open = ui.memory(|memory| memory.is_popup_open(popup_id));
        let (rect, response) = ui.allocate_exact_size(egui::vec2(72.0, 34.0), egui::Sense::click());
        let visuals = ui.style().interact_selectable(&response, open);
        ui.painter().rect_filled(rect, 1.0, visuals.bg_fill);
        ui.painter().rect_stroke(rect, 1.0, visuals.bg_stroke);
        let sample_rect = egui::Rect::from_min_max(
            rect.left_top() + egui::vec2(8.0, 0.0),
            rect.right_bottom() - egui::vec2(22.0, 0.0),
        )
        .shrink2(egui::vec2(0.0, 8.0));
        Self::paint_export_line_style_sample(
            ui.painter(),
            sample_rect,
            *selected_style,
            visuals.text_color(),
            false,
        );
        Self::paint_export_dropdown_chevron(ui.painter(), rect, visuals.text_color());
        if response.clicked() {
            ui.memory_mut(|memory| memory.toggle_popup(popup_id));
        }

        let mut changed = false;
        egui::popup::popup_below_widget(ui, popup_id, &response, |ui| {
            ui.set_min_width(72.0);
            for style in ExportArrowLineStyle::TOOLBAR {
                let (item_rect, item_response) =
                    ui.allocate_exact_size(egui::vec2(64.0, 28.0), egui::Sense::click());
                let item_visuals = ui
                    .style()
                    .interact_selectable(&item_response, *selected_style == style);
                ui.painter()
                    .rect_filled(item_rect, 1.0, item_visuals.bg_fill);
                ui.painter()
                    .rect_stroke(item_rect, 1.0, item_visuals.bg_stroke);
                let item_sample_rect = item_rect.shrink2(egui::vec2(9.0, 7.0));
                Self::paint_export_line_style_sample(
                    ui.painter(),
                    item_sample_rect,
                    style,
                    item_visuals.text_color(),
                    false,
                );
                if item_response.clicked() {
                    *selected_style = style;
                    changed = true;
                    ui.memory_mut(|memory| memory.close_popup());
                }
            }
        });
        changed
    }

    fn export_arrow_head_toggle_button(
        ui: &mut egui::Ui,
        enabled: &mut bool,
        tooltip: &str,
    ) -> bool {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(42.0, 34.0), egui::Sense::click());
        let visuals = ui.style().interact_selectable(&response, *enabled);
        ui.painter().rect_filled(rect, 1.0, visuals.bg_fill);
        ui.painter().rect_stroke(rect, 1.0, visuals.bg_stroke);
        Self::paint_export_line_style_sample(
            ui.painter(),
            rect.shrink2(egui::vec2(8.0, 9.0)),
            ExportArrowLineStyle::Solid,
            visuals.text_color(),
            *enabled,
        );
        if response.on_hover_text(tooltip).clicked() {
            *enabled = !*enabled;
            true
        } else {
            false
        }
    }

    fn paint_export_dropdown_chevron(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
        let center = egui::pos2(rect.right() - 13.0, rect.center().y + 1.0);
        let stroke = Stroke::new(1.2, color);
        painter.line_segment(
            [
                center + egui::vec2(-4.0, -2.0),
                center + egui::vec2(0.0, 2.0),
            ],
            stroke,
        );
        painter.line_segment(
            [
                center + egui::vec2(0.0, 2.0),
                center + egui::vec2(4.0, -2.0),
            ],
            stroke,
        );
    }

    fn paint_export_line_style_sample(
        painter: &egui::Painter,
        rect: egui::Rect,
        style: ExportArrowLineStyle,
        color: Color32,
        with_head: bool,
    ) {
        let width = if style == ExportArrowLineStyle::Thick {
            3.0
        } else {
            1.8
        };
        let draw_line = |painter: &egui::Painter, y_offset: f32| {
            let y = rect.center().y + y_offset;
            let start = egui::pos2(rect.left(), y);
            let end = egui::pos2(rect.right(), y);
            match style.stroke_style() {
                StrokeStyle::Solid => {
                    painter.line_segment([start, end], Stroke::new(width, color));
                }
                StrokeStyle::Dashed => {
                    let mut x = start.x;
                    while x < end.x {
                        let x2 = (x + 10.0).min(end.x);
                        painter.line_segment(
                            [egui::pos2(x, y), egui::pos2(x2, y)],
                            Stroke::new(width, color),
                        );
                        x += 16.0;
                    }
                }
                StrokeStyle::Dotted => {
                    let mut x = start.x;
                    while x <= end.x {
                        painter.circle_filled(egui::pos2(x, y), width.max(1.5), color);
                        x += 7.0;
                    }
                }
            }
        };

        if style == ExportArrowLineStyle::Double {
            draw_line(painter, -3.0);
            draw_line(painter, 3.0);
        } else {
            draw_line(painter, 0.0);
        }

        if with_head {
            let end = egui::pos2(rect.right(), rect.center().y);
            let stroke = Stroke::new(width, color);
            painter.line_segment([end, end + egui::vec2(-7.0, -5.0)], stroke);
            painter.line_segment([end, end + egui::vec2(-7.0, 5.0)], stroke);
        }
    }

    fn export_color_palette_controls(
        ui: &mut egui::Ui,
        selected_color: &mut Color32,
        tooltip: &str,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            changed |=
                Self::export_large_color_picker_button(ui, selected_color, tooltip).changed();
            ui.separator();
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
                for row in Self::export_color_palette_rows() {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(2.0, 0.0);
                        for color in row {
                            if Self::export_color_swatch_button(ui, color, *selected_color == color)
                                .clicked()
                            {
                                *selected_color = color;
                                changed = true;
                            }
                        }
                    });
                }
            });
        });
        changed
    }

    fn export_large_color_picker_button(
        ui: &mut egui::Ui,
        color: &mut Color32,
        tooltip: &str,
    ) -> egui::Response {
        let popup_id = ui.make_persistent_id("export_large_color_picker_popup");
        let open = ui.memory(|memory| memory.is_popup_open(popup_id));
        let (rect, response) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::click());
        let visuals = ui.style().interact_selectable(&response, open);
        Self::paint_export_large_color_picker_swatch(ui.painter(), rect, *color, visuals.bg_fill);
        if response.clicked() {
            ui.memory_mut(|memory| memory.toggle_popup(popup_id));
        }
        let mut response = response.on_hover_text(tooltip);
        let popup_changed = egui::popup::popup_below_widget(ui, popup_id, &response, |ui| {
            ui.set_min_width(300.0);
            ui.spacing_mut().slider_width = 220.0;
            egui::color_picker::color_picker_color32(ui, color, egui::color_picker::Alpha::Opaque)
        })
        .unwrap_or(false);
        if popup_changed {
            response.mark_changed();
        }
        response
    }

    fn paint_export_large_color_picker_swatch(
        painter: &egui::Painter,
        rect: egui::Rect,
        color: Color32,
        focus_fill: Color32,
    ) {
        let outer = rect.shrink(1.0);
        painter.rect_filled(outer, 0.0, focus_fill);
        let fill_rect = outer.shrink(3.0);
        painter.rect_filled(fill_rect, 0.0, color);
        Self::paint_export_color_picker_corner(painter, fill_rect);
        painter.rect_stroke(outer, 0.0, Stroke::new(2.0, Color32::BLACK));
    }

    fn export_color_picker_corner_triangle(rect: egui::Rect) -> [egui::Pos2; 3] {
        let size = rect.width().min(rect.height()) * 0.5;
        [
            rect.right_bottom(),
            egui::pos2(rect.right() - size, rect.bottom()),
            egui::pos2(rect.right(), rect.bottom() - size),
        ]
    }

    fn paint_export_color_picker_corner(painter: &egui::Painter, rect: egui::Rect) {
        let points = Self::export_color_picker_corner_triangle(rect);
        let mut mesh = egui::epaint::Mesh::default();
        mesh.colored_vertex(points[0], Color32::from_rgb(0, 170, 255));
        mesh.colored_vertex(points[1], Color32::from_rgb(255, 245, 0));
        mesh.colored_vertex(points[2], Color32::from_rgb(180, 0, 255));
        mesh.add_triangle(0, 1, 2);
        painter.add(egui::Shape::mesh(mesh));
        painter.line_segment(
            [points[1], points[2]],
            Stroke::new(1.0, Color32::from_rgb(30, 30, 30)),
        );
    }

    fn export_width_dot_button(ui: &mut egui::Ui, width: i32, selected: bool) -> egui::Response {
        let size = egui::vec2(24.0, 22.0);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        let visuals = ui.style().interact_selectable(&response, selected);
        ui.painter().rect_filled(
            rect,
            3.0,
            if selected {
                visuals.bg_fill
            } else {
                Color32::TRANSPARENT
            },
        );
        ui.painter().rect_stroke(rect, 3.0, visuals.bg_stroke);
        ui.painter().circle_filled(
            rect.center(),
            (width as f32 * 0.5).clamp(2.0, 6.0),
            visuals.text_color(),
        );
        response
    }

    fn export_color_swatch_button(
        ui: &mut egui::Ui,
        color: Color32,
        selected: bool,
    ) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
        ui.painter().rect_filled(rect.shrink(2.0), 0.0, color);
        let stroke = if selected {
            Stroke::new(2.0, Color32::from_rgb(20, 96, 180))
        } else {
            Stroke::new(1.0, Color32::DARK_GRAY)
        };
        ui.painter().rect_stroke(rect.shrink(1.0), 0.0, stroke);
        response
    }

    fn export_color_palette() -> [Color32; 18] {
        [
            Color32::BLACK,
            Color32::DARK_GRAY,
            Color32::from_rgb(128, 0, 32),
            Color32::from_rgb(220, 20, 38),
            Color32::from_rgb(255, 128, 0),
            Color32::YELLOW,
            Color32::from_rgb(34, 177, 76),
            Color32::from_rgb(0, 162, 232),
            Color32::from_rgb(63, 72, 204),
            Color32::from_rgb(163, 73, 164),
            Color32::WHITE,
            Color32::LIGHT_GRAY,
            Color32::from_rgb(185, 122, 87),
            Color32::from_rgb(255, 174, 201),
            Color32::from_rgb(255, 201, 14),
            Color32::from_rgb(181, 230, 29),
            Color32::from_rgb(153, 217, 234),
            Color32::from_rgb(112, 146, 190),
        ]
    }

    fn export_color_palette_rows() -> [[Color32; 9]; 2] {
        let palette = Self::export_color_palette();
        [
            [
                palette[0], palette[1], palette[2], palette[3], palette[4], palette[5], palette[6],
                palette[7], palette[8],
            ],
            [
                palette[9],
                palette[10],
                palette[11],
                palette[12],
                palette[13],
                palette[14],
                palette[15],
                palette[16],
                palette[17],
            ],
        ]
    }

    fn export_preview_property_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.export_preview_variable_selection_panel(ui, ctx);
        ui.separator();
        self.export_preview_text_annotations_panel(ui, ctx);
        ui.separator();
        self.export_preview_variable_labels_panel(ui, ctx);
    }

    fn export_preview_variable_selection_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let before = self.export_preview_state();
        let mut selections = self.export_preview_plot_selections();
        selections
            .imported
            .resize_with(self.imported_datasets.len(), Vec::new);
        let mut changed = false;

        egui::CollapsingHeader::new(self.tr("导出变量", "Export Variables"))
            .default_open(true)
            .show(ui, |ui| {
                if let Some(meta) = self.meta() {
                    egui::CollapsingHeader::new(self.dataset_label(0))
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(160.0)
                                .show(ui, |ui| {
                                    for channel in &meta.channels {
                                        let channel_index = channel.index;
                                        let mut selected =
                                            selections.primary.contains(&channel_index);
                                        ui.horizontal(|ui| {
                                            let (rect, _) = ui.allocate_exact_size(
                                                egui::vec2(12.0, 12.0),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter().rect_filled(
                                                rect,
                                                2.0,
                                                self.channel_color(channel_index),
                                            );
                                            if ui
                                                .checkbox(
                                                    &mut selected,
                                                    self.channel_name(channel_index),
                                                )
                                                .changed()
                                            {
                                                changed |= Self::set_export_selection_channel(
                                                    &mut selections.primary,
                                                    channel_index,
                                                    selected,
                                                );
                                            }
                                        });
                                    }
                                });
                        });
                }

                for dataset_index in 0..self.imported_datasets.len() {
                    let Some(meta) = self.imported_meta(dataset_index) else {
                        continue;
                    };
                    let dataset_label = self.dataset_label(dataset_index + 1);
                    egui::CollapsingHeader::new(dataset_label)
                        .default_open(false)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(140.0)
                                .show(ui, |ui| {
                                    for channel in &meta.channels {
                                        let channel_index = channel.index;
                                        let mut selected = selections
                                            .imported
                                            .get(dataset_index)
                                            .map(|channels| channels.contains(&channel_index))
                                            .unwrap_or(false);
                                        ui.horizontal(|ui| {
                                            let (rect, _) = ui.allocate_exact_size(
                                                egui::vec2(12.0, 12.0),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter().rect_filled(
                                                rect,
                                                2.0,
                                                self.plot_channel_color(
                                                    channel_index,
                                                    dataset_index + 1,
                                                    0,
                                                    1,
                                                ),
                                            );
                                            if ui
                                                .checkbox(&mut selected, channel.name.clone())
                                                .changed()
                                            {
                                                if let Some(channels) =
                                                    selections.imported.get_mut(dataset_index)
                                                {
                                                    changed |= Self::set_export_selection_channel(
                                                        channels,
                                                        channel_index,
                                                        selected,
                                                    );
                                                }
                                            }
                                        });
                                    }
                                });
                        });
                }

                egui::CollapsingHeader::new(self.tr("派生曲线", "Derived Curves"))
                    .default_open(!selections.derived.is_empty())
                    .show(ui, |ui| {
                        for derived_index in 0..self.derived_output_count() {
                            let mut selected = selections.derived.contains(&derived_index);
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(12.0, 12.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(
                                    rect,
                                    2.0,
                                    self.derived_channel_color(derived_index),
                                );
                                if ui
                                    .checkbox(
                                        &mut selected,
                                        self.derived_channel_name(derived_index),
                                    )
                                    .changed()
                                {
                                    changed |= Self::set_export_selection_channel(
                                        &mut selections.derived,
                                        derived_index,
                                        selected,
                                    );
                                }
                            });
                        }
                    });
            });

        if changed {
            self.export_preview_selections = Some(selections);
            let labels = self.current_export_curve_labels(&self.export_preview_plot_selections());
            self.sync_export_label_overrides(&labels);
            self.export_label_positions.fill(None);
            self.export_label_anchor_x.fill(None);
            self.push_export_preview_undo(before);
            self.mark_export_preview_dirty();
            ctx.request_repaint();
        }
    }

    fn export_preview_text_annotations_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::CollapsingHeader::new(self.tr("文字标注", "Text Notes"))
            .default_open(!self.export_text_annotations.is_empty())
            .show(ui, |ui| {
                if self.export_text_annotations.is_empty() {
                    ui.label(self.tr("暂无文字标注。", "No text notes."));
                    return;
                }
                let mut delete_index = None;
                for index in 0..self.export_text_annotations.len() {
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                        let color =
                            Self::annotation_color32(self.export_text_annotations[index].color);
                        ui.painter().rect_filled(rect, 2.0, color);

                        let before = self.export_preview_state();
                        let changed = {
                            let annotation = &mut self.export_text_annotations[index];
                            ui.add(
                                egui::TextEdit::singleline(&mut annotation.text)
                                    .desired_width(150.0),
                            )
                            .changed()
                        };
                        if changed {
                            self.push_export_preview_undo(before);
                            self.mark_export_preview_dirty();
                            ctx.request_repaint();
                        }
                        if ui
                            .button("×")
                            .on_hover_text(self.tr("删除", "Delete"))
                            .clicked()
                        {
                            delete_index = Some(index);
                        }
                    });
                }
                if let Some(index) = delete_index {
                    let before = self.export_preview_state();
                    self.export_text_annotations.remove(index);
                    self.push_export_preview_undo(before);
                    self.mark_export_preview_dirty();
                    ctx.request_repaint();
                }
            });
    }

    fn export_preview_variable_labels_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let selections = self.export_preview_plot_selections();
        let labels = self.current_export_curve_labels(&selections);
        if labels.len() != self.export_label_overrides.len() {
            self.sync_export_label_overrides(&labels);
            self.mark_export_preview_dirty();
        }

        egui::CollapsingHeader::new(self.tr("变量名标注", "Variable Labels"))
            .default_open(true)
            .show(ui, |ui| {
                let mut delete_source = None;
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for (index, label) in labels.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(14.0, 14.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(rect, 2.0, label.color);
                                let before_label = self.export_preview_state();
                                let changed = {
                                    let value = self
                                        .export_label_overrides
                                        .get_mut(index)
                                        .expect("label overrides synced above");
                                    ui.add(egui::TextEdit::singleline(value).desired_width(220.0))
                                        .changed()
                                };
                                if changed {
                                    self.push_export_preview_undo(before_label);
                                    self.mark_export_preview_dirty();
                                    ctx.request_repaint();
                                }
                                if ui
                                    .button("×")
                                    .on_hover_text(self.tr("删除", "Delete"))
                                    .clicked()
                                {
                                    delete_source = Some(label.source);
                                }
                            });
                        }
                    });
                if let Some(source) = delete_source {
                    let before = self.export_preview_state();
                    let mut selections = self.export_preview_plot_selections();
                    if Self::remove_export_curve_label_source(&mut selections, source) {
                        self.export_preview_selections = Some(selections);
                        let labels = self
                            .current_export_curve_labels(&self.export_preview_plot_selections());
                        self.sync_export_label_overrides(&labels);
                        self.export_label_positions.fill(None);
                        self.export_label_anchor_x.fill(None);
                        self.push_export_preview_undo(before);
                        self.mark_export_preview_dirty();
                        ctx.request_repaint();
                    }
                }
            });
    }

    fn export_preview_actions(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button(self.tr("保存图片", "Save Image")).clicked() {
                self.save_export_preview_image();
            }
            if ui.button(self.tr("导出 Word", "Export Word")).clicked() {
                self.save_export_preview_word();
            }
            if self.export_preview_size != [0, 0] {
                let dpi = self.export_dpi_value() as f32;
                ui.label(format!(
                    "{} x {} px @ {} DPI ({:.2} x {:.2} in)",
                    self.export_preview_size[0],
                    self.export_preview_size[1],
                    self.export_dpi_value(),
                    self.export_preview_size[0] as f32 / dpi,
                    self.export_preview_size[1] as f32 / dpi,
                ));
            }
        });
    }

    fn export_preview_canvas(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(error) = &self.export_preview_error {
            ui.label(RichText::new(self.localized_error_message(error)).color(Color32::LIGHT_RED));
            return;
        }
        let Some(texture) = &self.export_preview_texture else {
            ui.label(self.tr("正在生成预览...", "Generating preview..."));
            return;
        };
        let texture_id = texture.id();
        let image_size = egui::vec2(
            self.export_preview_size[0] as f32,
            self.export_preview_size[1] as f32,
        );
        egui::ScrollArea::both().show(ui, |ui| {
            let available = ui.available_size();
            let scale = Self::export_preview_display_scale(
                self.export_preview_zoom_mode,
                self.export_preview_zoom_scale,
                image_size,
                available,
            );
            self.export_preview_display_scale = scale;
            let response = ui.add(egui::Image::from_texture((texture_id, image_size * scale)));
            self.handle_export_preview_zoom_input(ui, response.rect, scale, ctx);
            self.export_preview_image_interactions(ui, response.rect, scale, ctx);
            if self.export_annotations_visible {
                self.export_preview_inline_text_editor(ui, response.rect, scale, ctx);
            }
        });
    }

    fn handle_export_preview_zoom_input(
        &mut self,
        ui: &egui::Ui,
        image_rect: egui::Rect,
        display_scale: f32,
        ctx: &egui::Context,
    ) {
        let direction = ui.input(|input| {
            if !input.modifiers.ctrl {
                return 0;
            }
            let Some(pointer_pos) = input.pointer.hover_pos() else {
                return 0;
            };
            if !image_rect.contains(pointer_pos) {
                return 0;
            }
            let scroll = input.raw_scroll_delta.y + input.smooth_scroll_delta.y;
            if scroll > 0.0 {
                1
            } else if scroll < 0.0 {
                -1
            } else {
                0
            }
        });
        if direction != 0 {
            self.step_export_preview_zoom(direction, display_scale);
            ctx.request_repaint();
        }
    }

    fn export_preview_image_interactions(
        &mut self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        scale: f32,
        ctx: &egui::Context,
    ) {
        if scale <= 0.0 {
            return;
        }
        if !self.export_annotations_visible {
            return;
        }
        if self.export_preview_tool == ExportPreviewTool::Arrow {
            self.export_preview_arrow_interactions(ui, image_rect, scale, ctx);
            return;
        }
        if self.export_preview_tool == ExportPreviewTool::Rectangle {
            self.export_preview_rectangle_interactions(ui, image_rect, scale, ctx);
            return;
        }
        if matches!(
            self.export_preview_tool,
            ExportPreviewTool::Brush | ExportPreviewTool::Eraser
        ) {
            self.export_preview_ink_interactions(ui, image_rect, scale, ctx);
            return;
        }
        if self.export_preview_tool == ExportPreviewTool::Text {
            self.export_preview_text_placement_interactions(ui, image_rect, scale, ctx);
            return;
        }
        if self.export_preview_manual_annotation_selection_interactions(ui, image_rect, scale, ctx)
        {
            return;
        }
        for placement in self.export_preview_label_layout.clone() {
            let label_rect = Self::canvas_rect_to_preview(image_rect, placement.label_rect, scale);
            let label_hit_rect = label_rect.expand((8.0 * scale).clamp(4.0, 12.0));
            let anchor_rect =
                Self::canvas_rect_to_preview(image_rect, placement.anchor_rect, scale);
            let anchor_pos = egui::pos2(
                image_rect.left() + placement.anchor_point[0] as f32 * scale,
                image_rect.top() + placement.anchor_point[1] as f32 * scale,
            );
            let pointer_on_label = ui
                .ctx()
                .pointer_hover_pos()
                .is_some_and(|pos| label_hit_rect.contains(pos));
            let anchor_id = ui
                .id()
                .with(("export_preview_anchor", placement.label_index));
            let anchor_sense = if pointer_on_label {
                egui::Sense::hover()
            } else {
                egui::Sense::click_and_drag()
            };
            let anchor_response = ui.interact(anchor_rect, anchor_id, anchor_sense);
            let anchor_active = anchor_response.dragged_by(PointerButton::Primary)
                || anchor_response.drag_started_by(PointerButton::Primary);
            let anchor_radius = (5.0 * scale).clamp(4.0, 8.0);
            ui.painter().circle_filled(
                anchor_pos,
                anchor_radius,
                Color32::from_rgba_premultiplied(255, 255, 255, 210),
            );
            ui.painter().circle_stroke(
                anchor_pos,
                anchor_radius,
                Stroke::new(1.5, Color32::from_rgb(220, 20, 38)),
            );
            if anchor_response.hovered() {
                ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::Grab);
            }
            if anchor_response.drag_started_by(PointerButton::Primary) {
                self.export_preview_anchor_drag = Some(ExportPreviewAnchorDrag {
                    label_index: placement.label_index,
                    before_state: self.export_preview_state(),
                    undo_recorded: false,
                });
            }
            if anchor_response.dragged_by(PointerButton::Primary) {
                let before_anchor = if let Some(drag) = self.export_preview_anchor_drag.as_mut() {
                    if drag.label_index == placement.label_index {
                        if !drag.undo_recorded {
                            drag.undo_recorded = true;
                            Some(drag.before_state.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(pointer_pos) = anchor_response.interact_pointer_pos() {
                    if let Ok((x_min, x_max)) = self.export_time_range() {
                        let canvas_x = ((pointer_pos.x - image_rect.left()) / scale).round() as i32;
                        let clamped_x =
                            canvas_x.clamp(placement.plot_rect.left, placement.plot_rect.right);
                        let next_x = x_min
                            + (clamped_x - placement.plot_rect.left) as f64
                                / (placement.plot_rect.right - placement.plot_rect.left) as f64
                                * (x_max - x_min);
                        self.set_export_label_anchor_x(placement.label_index, next_x);
                        if let Some(before_anchor) = before_anchor {
                            self.push_export_preview_undo(before_anchor);
                        }
                        self.mark_export_preview_dirty();
                        ctx.request_repaint();
                        ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::Grabbing);
                    }
                }
            }
            if anchor_response.drag_stopped_by(PointerButton::Primary)
                && self
                    .export_preview_anchor_drag
                    .as_ref()
                    .is_some_and(|drag| drag.label_index == placement.label_index)
            {
                self.export_preview_anchor_drag = None;
            }
            let id = ui
                .id()
                .with(("export_preview_label", placement.label_index));
            let response = ui.interact(label_hit_rect, id, egui::Sense::click_and_drag());
            let selected = self.export_annotation_selection
                == Some(ExportAnnotationSelection::VariableLabel(
                    placement.label_index,
                ));
            if (response.hovered() || selected) && !anchor_active {
                ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::Grab);
                ui.painter().rect_stroke(
                    label_rect.expand(2.0),
                    2.0,
                    Stroke::new(1.0, Color32::from_rgb(20, 96, 180)),
                );
            }
            if response.clicked_by(PointerButton::Primary) && !anchor_active {
                self.export_annotation_selection = Some(ExportAnnotationSelection::VariableLabel(
                    placement.label_index,
                ));
                ctx.request_repaint();
            }
            if response.double_clicked() && !anchor_active {
                self.export_preview_edit_label_index = Some(placement.label_index);
                self.export_preview_edit_label_focus_pending = true;
                ctx.request_repaint();
            }
            if self.export_preview_inline_label_editor(
                ui,
                image_rect,
                label_rect,
                placement.label_index,
                ctx,
            ) {
                continue;
            }
            if response.drag_started_by(PointerButton::Primary) && !anchor_active {
                self.export_annotation_selection = Some(ExportAnnotationSelection::VariableLabel(
                    placement.label_index,
                ));
                let before_drag = self.export_preview_state();
                let start_pos = self
                    .export_label_positions
                    .get(placement.label_index)
                    .and_then(|position| *position)
                    .unwrap_or([placement.label_rect[0] + 4, placement.label_rect[1] + 3]);
                self.export_preview_drag = Some(ExportPreviewDrag {
                    label_index: placement.label_index,
                    start_pos,
                    before_state: before_drag,
                    undo_recorded: false,
                });
            }
            if response.dragged_by(PointerButton::Primary) {
                let drag_info = if let Some(drag) = self.export_preview_drag.as_mut() {
                    if drag.label_index == placement.label_index {
                        if !drag.undo_recorded {
                            drag.undo_recorded = true;
                            Some((drag.start_pos, Some(drag.before_state.clone())))
                        } else {
                            Some((drag.start_pos, None))
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some((start_pos, before_drag)) = drag_info {
                    let delta = response.drag_delta() / scale;
                    let text_w = (placement.label_rect[2] - placement.label_rect[0] - 8).max(1);
                    let text_h = (placement.label_rect[3] - placement.label_rect[1] - 7).max(1);
                    let next = Self::clamp_export_label_position(
                        [
                            start_pos[0] + delta.x.round() as i32,
                            start_pos[1] + delta.y.round() as i32,
                        ],
                        placement.drag_bounds,
                        text_w,
                        text_h,
                    );
                    self.set_export_label_canvas_position(placement.label_index, [next.0, next.1]);
                    if let Some(before_drag) = before_drag {
                        self.push_export_preview_undo(before_drag);
                    }
                    self.mark_export_preview_dirty();
                    ctx.request_repaint();
                    ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::Grabbing);
                }
            }
            if response.drag_stopped_by(PointerButton::Primary)
                && self
                    .export_preview_drag
                    .as_ref()
                    .is_some_and(|drag| drag.label_index == placement.label_index)
            {
                self.export_preview_drag = None;
            }
        }
        self.export_preview_text_interactions(ui, image_rect, scale, ctx);
    }

    fn export_preview_manual_annotation_selection_interactions(
        &mut self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        scale: f32,
        ctx: &egui::Context,
    ) -> bool {
        self.paint_selected_export_manual_annotation(ui.painter(), image_rect, scale);
        let Some(pointer_pos) = ui.ctx().pointer_hover_pos() else {
            return false;
        };
        if !image_rect.contains(pointer_pos) {
            return false;
        }
        let canvas_pos =
            Self::canvas_point(Self::preview_pos_to_canvas(image_rect, scale, pointer_pos));
        let hit = self.hit_test_export_manual_annotation(canvas_pos, scale);
        if hit.is_some() {
            ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::PointingHand);
        }
        if ui.input(|input| input.pointer.button_clicked(PointerButton::Primary)) {
            self.export_annotation_selection = hit;
            ctx.request_repaint();
            return hit.is_some();
        }
        false
    }

    fn hit_test_export_manual_annotation(
        &self,
        canvas_pos: CanvasPoint,
        scale: f32,
    ) -> Option<ExportAnnotationSelection> {
        let tolerance = (8.0 / scale.max(0.05)).max(6.0) as i32;
        let tolerance_sq = (tolerance as f64).powi(2);
        for (index, stroke) in self.export_ink_strokes.iter().enumerate().rev() {
            if stroke.points.windows(2).any(|segment| {
                export_annotation::point_segment_distance_sq(canvas_pos, segment[0], segment[1])
                    <= tolerance_sq
            }) || stroke.points.iter().any(|point| {
                export_annotation::point_segment_distance_sq(canvas_pos, *point, *point)
                    <= tolerance_sq
            }) {
                return Some(ExportAnnotationSelection::Ink(index));
            }
        }
        for (index, rectangle) in self.export_rectangle_annotations.iter().enumerate().rev() {
            if Self::expanded_canvas_rect_contains(rectangle.rect, tolerance, canvas_pos) {
                return Some(ExportAnnotationSelection::Rectangle(index));
            }
        }
        for (index, arrow) in self.export_arrow_annotations.iter().enumerate().rev() {
            if export_annotation::point_segment_distance_sq(canvas_pos, arrow.start, arrow.end)
                <= tolerance_sq
            {
                return Some(ExportAnnotationSelection::Arrow(index));
            }
        }
        None
    }

    fn expanded_canvas_rect_contains(rect: CanvasRect, expand: i32, point: CanvasPoint) -> bool {
        point.x >= rect.left - expand
            && point.x <= rect.right + expand
            && point.y >= rect.top - expand
            && point.y <= rect.bottom + expand
    }

    fn paint_selected_export_manual_annotation(
        &self,
        painter: &egui::Painter,
        image_rect: egui::Rect,
        scale: f32,
    ) {
        let stroke = Stroke::new(2.0, Color32::from_rgb(20, 96, 180));
        match self.export_annotation_selection {
            Some(ExportAnnotationSelection::Arrow(index)) => {
                if let Some(arrow) = self.export_arrow_annotations.get(index) {
                    painter.line_segment(
                        [
                            Self::canvas_point_to_preview(image_rect, arrow.start, scale),
                            Self::canvas_point_to_preview(image_rect, arrow.end, scale),
                        ],
                        stroke,
                    );
                }
            }
            Some(ExportAnnotationSelection::Rectangle(index)) => {
                if let Some(rectangle) = self.export_rectangle_annotations.get(index) {
                    let rect = Self::canvas_rect_to_preview(
                        image_rect,
                        [
                            rectangle.rect.left,
                            rectangle.rect.top,
                            rectangle.rect.right,
                            rectangle.rect.bottom,
                        ],
                        scale,
                    );
                    painter.rect_stroke(rect.expand(2.0), 2.0, stroke);
                }
            }
            Some(ExportAnnotationSelection::Ink(index)) => {
                if let Some(stroke_annotation) = self.export_ink_strokes.get(index) {
                    for segment in stroke_annotation.points.windows(2) {
                        painter.line_segment(
                            [
                                Self::canvas_point_to_preview(image_rect, segment[0], scale),
                                Self::canvas_point_to_preview(image_rect, segment[1], scale),
                            ],
                            stroke,
                        );
                    }
                    if stroke_annotation.points.len() == 1 {
                        painter.circle_stroke(
                            Self::canvas_point_to_preview(
                                image_rect,
                                stroke_annotation.points[0],
                                scale,
                            ),
                            4.0,
                            stroke,
                        );
                    }
                }
            }
            Some(ExportAnnotationSelection::VariableLabel(_))
            | Some(ExportAnnotationSelection::Text(_))
            | None => {}
        }
    }

    fn export_preview_arrow_interactions(
        &mut self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        scale: f32,
        ctx: &egui::Context,
    ) {
        let id = ui.id().with("export_preview_arrow_tool");
        let response = ui.interact(image_rect, id, egui::Sense::click_and_drag());
        if response.hovered() {
            ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::Crosshair);
        }
        if response.drag_started_by(PointerButton::Primary) {
            let Some(pointer_pos) = response.interact_pointer_pos() else {
                return;
            };
            let canvas_pos = Self::preview_pos_to_canvas(image_rect, scale, pointer_pos);
            let before = self.export_preview_state();
            self.export_arrow_annotations.push(ExportArrowAnnotation {
                start: Self::canvas_point(canvas_pos),
                end: Self::canvas_point(canvas_pos),
                color: Self::annotation_color(self.export_brush_color),
                width: self.export_brush_width.clamp(1, 32),
                head_size: Self::manual_export_arrow_head_size(
                    self.export_manual_arrow_head_enabled,
                    self.export_arrow_size,
                ),
                line_style: Self::annotation_line_style(self.export_arrow_line_style),
            });
            self.export_arrow_drag = Some(ExportArrowDrag {
                arrow_index: self.export_arrow_annotations.len().checked_sub(1),
                before_state: before,
                undo_recorded: false,
            });
            self.export_annotation_selection = self
                .export_arrow_annotations
                .len()
                .checked_sub(1)
                .map(ExportAnnotationSelection::Arrow);
        }
        if response.dragged_by(PointerButton::Primary) {
            let Some(pointer_pos) = response.interact_pointer_pos() else {
                return;
            };
            let canvas_pos = Self::preview_pos_to_canvas(image_rect, scale, pointer_pos);
            let before = self.export_arrow_drag.as_mut().and_then(|drag| {
                if !drag.undo_recorded {
                    drag.undo_recorded = true;
                    Some(drag.before_state.clone())
                } else {
                    None
                }
            });
            if let Some(index) = self
                .export_arrow_drag
                .as_ref()
                .and_then(|drag| drag.arrow_index)
            {
                if let Some(arrow) = self.export_arrow_annotations.get_mut(index) {
                    arrow.end = Self::canvas_point(canvas_pos);
                }
            }
            if let Some(before) = before {
                self.push_export_preview_undo(before);
            }
            self.mark_export_preview_dirty();
            ctx.request_repaint();
        }
        if response.drag_stopped_by(PointerButton::Primary) {
            self.export_arrow_drag = None;
        }
    }

    fn export_preview_rectangle_interactions(
        &mut self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        scale: f32,
        ctx: &egui::Context,
    ) {
        let id = ui.id().with("export_preview_rectangle_tool");
        let response = ui.interact(image_rect, id, egui::Sense::click_and_drag());
        if response.hovered() {
            ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::Crosshair);
        }
        if response.drag_started_by(PointerButton::Primary) {
            let Some(pointer_pos) = response.interact_pointer_pos() else {
                return;
            };
            let canvas_pos =
                Self::canvas_point(Self::preview_pos_to_canvas(image_rect, scale, pointer_pos));
            let before = self.export_preview_state();
            self.export_rectangle_annotations.push(
                ExportRectangleAnnotation::from_corners_with_kind(
                    canvas_pos,
                    canvas_pos,
                    self.export_shape_kind,
                    Self::annotation_color(self.export_brush_color),
                    self.export_brush_width.clamp(1, 32),
                    Self::annotation_line_style(self.export_arrow_line_style),
                ),
            );
            self.export_rectangle_drag = Some(ExportRectangleDrag {
                rectangle_index: self.export_rectangle_annotations.len().checked_sub(1),
                start: canvas_pos,
                before_state: before,
                undo_recorded: false,
            });
            self.export_annotation_selection = self
                .export_rectangle_annotations
                .len()
                .checked_sub(1)
                .map(ExportAnnotationSelection::Rectangle);
        }
        if response.dragged_by(PointerButton::Primary) {
            let Some(pointer_pos) = response.interact_pointer_pos() else {
                return;
            };
            let canvas_pos =
                Self::canvas_point(Self::preview_pos_to_canvas(image_rect, scale, pointer_pos));
            let before = self.export_rectangle_drag.as_mut().and_then(|drag| {
                if !drag.undo_recorded {
                    drag.undo_recorded = true;
                    Some(drag.before_state.clone())
                } else {
                    None
                }
            });
            let drag_start = self.export_rectangle_drag.as_ref().map(|drag| drag.start);
            if let (Some(index), Some(start)) = (
                self.export_rectangle_drag
                    .as_ref()
                    .and_then(|drag| drag.rectangle_index),
                drag_start,
            ) {
                if let Some(rectangle) = self.export_rectangle_annotations.get_mut(index) {
                    *rectangle = ExportRectangleAnnotation::from_corners_with_kind(
                        start,
                        canvas_pos,
                        rectangle.kind,
                        rectangle.color,
                        rectangle.width,
                        rectangle.line_style,
                    );
                }
            }
            if let Some(before) = before {
                self.push_export_preview_undo(before);
            }
            self.mark_export_preview_dirty();
            ctx.request_repaint();
        }
        if response.drag_stopped_by(PointerButton::Primary) {
            self.export_rectangle_drag = None;
        }
    }

    fn export_preview_ink_interactions(
        &mut self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        scale: f32,
        ctx: &egui::Context,
    ) {
        let id = ui.id().with("export_preview_ink_tool");
        let response = ui.interact(image_rect, id, egui::Sense::click_and_drag());
        if response.hovered() {
            ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::Crosshair);
        }
        let Some(pointer_pos) = response.interact_pointer_pos() else {
            if response.drag_stopped_by(PointerButton::Primary) {
                self.export_ink_drag = None;
            }
            return;
        };
        let canvas_pos = CanvasPoint::new(
            ((pointer_pos.x - image_rect.left()) / scale).round() as i32,
            ((pointer_pos.y - image_rect.top()) / scale).round() as i32,
        );
        if response.drag_started_by(PointerButton::Primary) {
            let before = self.export_preview_state();
            let stroke_index = if self.export_preview_tool == ExportPreviewTool::Brush {
                self.export_ink_strokes.push(ExportInkStroke {
                    points: vec![canvas_pos],
                    color: Self::annotation_color(self.export_brush_color),
                    width: self.export_brush_width.clamp(1, 32),
                });
                Some(self.export_ink_strokes.len() - 1)
            } else {
                None
            };
            self.export_ink_drag = Some(ExportInkDrag {
                stroke_index,
                before_state: before,
                undo_recorded: false,
            });
            self.export_annotation_selection = stroke_index.map(ExportAnnotationSelection::Ink);
        }
        if response.dragged_by(PointerButton::Primary)
            || response.clicked_by(PointerButton::Primary)
        {
            let before = self
                .export_ink_drag
                .as_ref()
                .map(|drag| drag.before_state.clone());
            match self.export_preview_tool {
                ExportPreviewTool::Brush => {
                    if let Some(index) = self
                        .export_ink_drag
                        .as_ref()
                        .and_then(|drag| drag.stroke_index)
                    {
                        if let Some(stroke) = self.export_ink_strokes.get_mut(index) {
                            if stroke.points.last().copied() != Some(canvas_pos) {
                                stroke.points.push(canvas_pos);
                            }
                        }
                    }
                }
                ExportPreviewTool::Eraser => {
                    let radius = (self.export_brush_width.max(8) * 3) as f64;
                    self.export_ink_strokes
                        .retain(|stroke| !Self::stroke_near_point(stroke, canvas_pos, radius));
                }
                ExportPreviewTool::Select
                | ExportPreviewTool::Text
                | ExportPreviewTool::Arrow
                | ExportPreviewTool::Rectangle => {}
            }
            if let Some(drag) = self.export_ink_drag.as_mut() {
                if !drag.undo_recorded {
                    drag.undo_recorded = true;
                    if let Some(before) = before {
                        self.push_export_preview_undo(before);
                    }
                }
            }
            self.mark_export_preview_dirty();
            ctx.request_repaint();
        }
        if response.drag_stopped_by(PointerButton::Primary) {
            self.export_ink_drag = None;
        }
    }

    fn export_preview_text_placement_interactions(
        &mut self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        scale: f32,
        ctx: &egui::Context,
    ) {
        let id = ui.id().with("export_preview_text_placement");
        let response = ui.interact(image_rect, id, egui::Sense::click());
        if response.hovered() {
            ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::Crosshair);
        }
        if response.clicked_by(PointerButton::Primary) {
            let Some(pointer_pos) = response.interact_pointer_pos() else {
                return;
            };
            let canvas_pos = Self::preview_pos_to_canvas(image_rect, scale, pointer_pos);
            self.add_export_text_annotation_at(canvas_pos);
            self.export_preview_edit_text_index = self.export_text_annotations.len().checked_sub(1);
            self.export_preview_edit_text_focus_pending = true;
            self.export_annotation_selection = self
                .export_text_annotations
                .len()
                .checked_sub(1)
                .map(ExportAnnotationSelection::Text);
            self.export_preview_tool = ExportPreviewTool::Select;
            ctx.request_repaint();
        }
    }

    fn export_preview_text_interactions(
        &mut self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        scale: f32,
        ctx: &egui::Context,
    ) {
        for (index, annotation) in self.export_text_annotations.clone().into_iter().enumerate() {
            if self.export_preview_edit_text_index == Some(index) {
                continue;
            }
            let text_w = Canvas::text_width(&annotation.text, annotation.scale).max(36);
            let text_h = Canvas::text_height(annotation.scale).max(18);
            let position = annotation.position.to_array();
            let rect = Self::canvas_rect_to_preview(
                image_rect,
                [
                    position[0] - 4,
                    position[1] - 4,
                    position[0] + text_w + 4,
                    position[1] + text_h + 4,
                ],
                scale,
            );
            let response = ui.interact(
                rect,
                ui.id().with(("export_text_annotation", index)),
                egui::Sense::click_and_drag(),
            );
            let selected =
                self.export_annotation_selection == Some(ExportAnnotationSelection::Text(index));
            if response.hovered() || selected {
                ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::Grab);
                ui.painter().rect_stroke(
                    rect.expand(2.0),
                    2.0,
                    Stroke::new(1.0, Color32::from_rgb(20, 96, 180)),
                );
            }
            if response.clicked_by(PointerButton::Primary) {
                self.export_annotation_selection = Some(ExportAnnotationSelection::Text(index));
                ctx.request_repaint();
            }
            if response.double_clicked() {
                self.export_preview_edit_text_index = Some(index);
                self.export_preview_edit_text_focus_pending = true;
                ctx.request_repaint();
            }
            if response.drag_started_by(PointerButton::Primary) {
                self.export_annotation_selection = Some(ExportAnnotationSelection::Text(index));
                self.export_preview_text_drag = Some(ExportPreviewTextDrag {
                    text_index: index,
                    start_pos: position,
                    before_state: self.export_preview_state(),
                    undo_recorded: false,
                });
            }
            if response.dragged_by(PointerButton::Primary) {
                let drag_info = self.export_preview_text_drag.as_mut().and_then(|drag| {
                    (drag.text_index == index).then(|| {
                        let before = if !drag.undo_recorded {
                            drag.undo_recorded = true;
                            Some(drag.before_state.clone())
                        } else {
                            None
                        };
                        (drag.start_pos, before)
                    })
                });
                if let Some((start_pos, before)) = drag_info {
                    let delta = response.drag_delta() / scale;
                    let bounds = ClipRect {
                        left: 0,
                        top: 0,
                        right: self.export_preview_size[0] as i32,
                        bottom: self.export_preview_size[1] as i32,
                    };
                    let next = Self::clamp_export_label_position(
                        [
                            start_pos[0] + delta.x.round() as i32,
                            start_pos[1] + delta.y.round() as i32,
                        ],
                        bounds,
                        text_w,
                        text_h,
                    );
                    if let Some(text) = self.export_text_annotations.get_mut(index) {
                        text.position = CanvasPoint::new(next.0, next.1);
                    }
                    if let Some(before) = before {
                        self.push_export_preview_undo(before);
                    }
                    self.mark_export_preview_dirty();
                    ctx.request_repaint();
                }
            }
            if response.drag_stopped_by(PointerButton::Primary) {
                if self
                    .export_preview_text_drag
                    .as_ref()
                    .is_some_and(|drag| drag.text_index == index)
                {
                    self.export_preview_text_drag = None;
                }
            }
        }
    }

    fn canvas_rect_to_preview(
        image_rect: egui::Rect,
        canvas_rect: [i32; 4],
        scale: f32,
    ) -> egui::Rect {
        egui::Rect::from_min_max(
            egui::pos2(
                image_rect.left() + canvas_rect[0] as f32 * scale,
                image_rect.top() + canvas_rect[1] as f32 * scale,
            ),
            egui::pos2(
                image_rect.left() + canvas_rect[2] as f32 * scale,
                image_rect.top() + canvas_rect[3] as f32 * scale,
            ),
        )
    }

    fn canvas_point_to_preview(
        image_rect: egui::Rect,
        point: CanvasPoint,
        scale: f32,
    ) -> egui::Pos2 {
        egui::pos2(
            image_rect.left() + point.x as f32 * scale,
            image_rect.top() + point.y as f32 * scale,
        )
    }

    fn preview_pos_to_canvas(
        image_rect: egui::Rect,
        scale: f32,
        pointer_pos: egui::Pos2,
    ) -> [i32; 2] {
        [
            ((pointer_pos.x - image_rect.left()) / scale).round() as i32,
            ((pointer_pos.y - image_rect.top()) / scale).round() as i32,
        ]
    }

    fn export_preview_inline_label_editor(
        &mut self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        label_rect: egui::Rect,
        label_index: usize,
        ctx: &egui::Context,
    ) -> bool {
        if self.export_preview_edit_label_index != Some(label_index) {
            return false;
        }
        if label_index >= self.export_label_overrides.len() {
            self.export_preview_edit_label_index = None;
            self.export_preview_edit_label_focus_pending = false;
            return false;
        }

        let editor_id = ui
            .id()
            .with(("export_preview_inline_label_editor", label_index));
        let width = label_rect.width().max(180.0).min(520.0);
        let editor_pos = egui::pos2(
            label_rect
                .left()
                .clamp(image_rect.left(), image_rect.right() - 24.0),
            label_rect
                .top()
                .clamp(image_rect.top(), image_rect.bottom() - 24.0),
        );
        let before_label = self.export_preview_state();
        let mut changed = false;
        let mut close_requested = false;
        let mut focus_pending = self.export_preview_edit_label_focus_pending;

        egui::Area::new(editor_id)
            .order(egui::Order::Foreground)
            .fixed_pos(editor_pos)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(Color32::from_rgb(255, 255, 255))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(20, 96, 180)))
                    .rounding(2.0)
                    .inner_margin(egui::Margin::same(2.0))
                    .show(ui, |ui| {
                        let value = self
                            .export_label_overrides
                            .get_mut(label_index)
                            .expect("inline label editor index checked above");
                        let response = ui.add(
                            egui::TextEdit::singleline(value)
                                .desired_width(width)
                                .font(egui::TextStyle::Body),
                        );
                        if focus_pending {
                            response.request_focus();
                            focus_pending = false;
                        }
                        changed |= response.changed();
                        if response.has_focus()
                            && ui.input(|input| {
                                input.key_pressed(egui::Key::Enter)
                                    || input.key_pressed(egui::Key::Escape)
                            })
                        {
                            close_requested = true;
                        }
                        if response.lost_focus() {
                            close_requested = true;
                        }
                    });
            });

        self.export_preview_edit_label_focus_pending = focus_pending;
        if changed {
            self.push_export_preview_undo(before_label);
            self.mark_export_preview_dirty();
            ctx.request_repaint();
        }
        if close_requested {
            self.export_preview_edit_label_index = None;
            self.export_preview_edit_label_focus_pending = false;
            ctx.request_repaint();
        }
        true
    }

    fn export_inline_text_editor_rect(
        image_rect: egui::Rect,
        annotation: &ExportTextAnnotation,
        scale: f32,
    ) -> egui::Rect {
        let text_w = Canvas::text_width(&annotation.text, annotation.scale).max(120) as f32;
        let text_h = Canvas::text_height(annotation.scale).max(18) as f32;
        let width = (text_w * scale).max(160.0).min(520.0);
        let height = (text_h * scale).max(28.0);
        let desired_pos = egui::pos2(
            image_rect.left() + annotation.position.x as f32 * scale,
            image_rect.top() + annotation.position.y as f32 * scale,
        );
        let x = desired_pos.x.clamp(
            image_rect.left(),
            (image_rect.right() - width).max(image_rect.left()),
        );
        let y = desired_pos.y.clamp(
            image_rect.top(),
            (image_rect.bottom() - height).max(image_rect.top()),
        );
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(width, height))
    }

    fn export_preview_inline_text_editor(
        &mut self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        scale: f32,
        ctx: &egui::Context,
    ) {
        let Some(index) = self.export_preview_edit_text_index else {
            return;
        };
        if index >= self.export_text_annotations.len() {
            self.export_preview_edit_text_index = None;
            self.export_preview_edit_text_focus_pending = false;
            return;
        }

        let editor_rect = Self::export_inline_text_editor_rect(
            image_rect,
            &self.export_text_annotations[index],
            scale,
        );
        let editor_id = ui.id().with(("export_preview_inline_text_editor", index));
        let before = self.export_preview_state();
        let mut changed = false;
        let mut close_requested = false;
        let mut focus_pending = self.export_preview_edit_text_focus_pending;

        egui::Area::new(editor_id)
            .order(egui::Order::Foreground)
            .fixed_pos(editor_rect.min)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(Color32::from_rgb(255, 255, 255))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(20, 96, 180)))
                    .rounding(2.0)
                    .inner_margin(egui::Margin::same(2.0))
                    .show(ui, |ui| {
                        let hint_text = self.tr("文字标注", "Text note");
                        let annotation = self
                            .export_text_annotations
                            .get_mut(index)
                            .expect("text editor index checked above");
                        let response = ui.add_sized(
                            editor_rect.size(),
                            egui::TextEdit::singleline(&mut annotation.text)
                                .font(egui::TextStyle::Body)
                                .hint_text(hint_text),
                        );
                        if focus_pending {
                            response.request_focus();
                            focus_pending = false;
                        }
                        changed |= response.changed();
                        if response.has_focus()
                            && ui.input(|input| {
                                input.key_pressed(egui::Key::Enter)
                                    || input.key_pressed(egui::Key::Escape)
                            })
                        {
                            close_requested = true;
                        }
                        if response.lost_focus() {
                            close_requested = true;
                        }
                    });
            });

        self.export_preview_edit_text_focus_pending = focus_pending;
        if changed {
            self.push_export_preview_undo(before);
            self.mark_export_preview_dirty();
            ctx.request_repaint();
        }
        if close_requested {
            self.export_preview_edit_text_index = None;
            self.export_preview_edit_text_focus_pending = false;
            ctx.request_repaint();
        }
    }

    #[allow(dead_code)]
    fn export_preview_text_editor(&mut self, ctx: &egui::Context) {
        let Some(index) = self.export_preview_edit_text_index else {
            return;
        };
        if index >= self.export_text_annotations.len() {
            self.export_preview_edit_text_index = None;
            return;
        }
        let mut open = true;
        let mut close_requested = false;
        let title = self.tr("编辑文字", "Edit Text").to_owned();
        let size_label = self.tr("字号", "Size").to_owned();
        let ok_label = self.tr("确定", "OK").to_owned();
        let delete_label = self.tr("删除", "Delete").to_owned();
        let mut focus_pending = self.export_preview_edit_text_focus_pending;
        egui::Window::new(title)
            .id(egui::Id::new("export_preview_text_editor"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                let before = self.export_preview_state();
                let mut changed = false;
                {
                    let annotation = self
                        .export_text_annotations
                        .get_mut(index)
                        .expect("text editor index checked above");
                    let response = ui
                        .add(egui::TextEdit::singleline(&mut annotation.text).desired_width(320.0));
                    if focus_pending {
                        response.request_focus();
                        focus_pending = false;
                    }
                    changed |= response.changed();
                    changed |= ui
                        .add(
                            egui::Slider::new(
                                &mut annotation.scale,
                                MIN_EXPORT_LABEL_SCALE..=MAX_EXPORT_LABEL_SCALE,
                            )
                            .text(size_label.as_str()),
                        )
                        .changed();
                    let mut color = Self::annotation_color32(annotation.color);
                    let color_changed = egui::color_picker::color_edit_button_srgba(
                        ui,
                        &mut color,
                        egui::color_picker::Alpha::Opaque,
                    )
                    .changed();
                    if color_changed {
                        annotation.color = Self::annotation_color(color);
                        changed = true;
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button(ok_label.as_str()).clicked() {
                        close_requested = true;
                    }
                    if ui.button(delete_label.as_str()).clicked() {
                        let before = self.export_preview_state();
                        self.export_text_annotations.remove(index);
                        self.push_export_preview_undo(before);
                        self.mark_export_preview_dirty();
                        close_requested = true;
                    }
                });
                if changed {
                    self.push_export_preview_undo(before);
                    self.mark_export_preview_dirty();
                    ctx.request_repaint();
                }
            });
        self.export_preview_edit_text_focus_pending = focus_pending;
        if !open || close_requested {
            self.export_preview_edit_text_index = None;
            self.export_preview_edit_text_focus_pending = false;
        }
    }

    fn save_export_preview_image(&mut self) {
        match self.export_image_format {
            ExportImageFormat::Png => self.save_export_preview_png(),
            ExportImageFormat::Svg => self.save_export_preview_svg(),
        }
    }

    fn save_export_preview_png(&mut self) {
        let selections = self.export_preview_plot_selections();
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter(self.tr("PNG 图片", "PNG image"), &["png"])
            .set_file_name(self.default_waveform_image_name())
            .save_file()
        else {
            return;
        };
        ensure_export_extension(&mut path, ExportImageFormat::Png);
        match self.write_current_waveform_png(&path, &selections) {
            Ok(()) => tracing::info!("exported waveform image to {}", path.display()),
            Err(error) => {
                self.export_preview_error = Some(error.clone());
                self.last_error = Some(match self.language {
                    Language::Zh => format!("导出波形图片失败: {error}"),
                    Language::En => format!("Failed to export waveform image: {error}"),
                });
            }
        }
    }

    fn save_export_preview_svg(&mut self) {
        let selections = self.export_preview_plot_selections();
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter(self.tr("SVG 矢量图", "SVG vector image"), &["svg"])
            .set_file_name(self.default_waveform_image_name())
            .save_file()
        else {
            return;
        };
        ensure_export_extension(&mut path, ExportImageFormat::Svg);
        match self.write_current_waveform_svg(&path, &selections) {
            Ok(()) => tracing::info!("exported waveform SVG to {}", path.display()),
            Err(error) => {
                self.export_preview_error = Some(error.clone());
                self.last_error = Some(match self.language {
                    Language::Zh => format!("导出波形 SVG 失败: {error}"),
                    Language::En => format!("Failed to export waveform SVG: {error}"),
                });
            }
        }
    }

    fn save_export_preview_word(&mut self) {
        let selections = self.export_preview_plot_selections();
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("DOCX", &["docx"])
            .set_file_name(self.default_waveform_docx_name())
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("docx");
        }

        let range = match self.export_time_range() {
            Ok((start, end)) => format!("{start:.6}s - {end:.6}s"),
            Err(error) => {
                self.export_preview_error = Some(error);
                return;
            }
        };
        let figure = match self
            .build_word_report_figure(format!("图 1：当前波形 {range}"), &selections)
        {
            Ok(figure) => figure,
            Err(error) => {
                self.export_preview_error = Some(error.clone());
                self.last_error = Some(match self.language {
                    Language::Zh => format!("导出 DOCX 失败: {error}"),
                    Language::En => format!("Failed to export DOCX: {error}"),
                });
                return;
            }
        };
        let report = self.build_word_report(vec![figure], range);
        if let Err(error) = crate::word_export::write_word_report(&path, &report) {
            let message = error.to_string();
            self.export_preview_error = Some(message.clone());
            self.last_error = Some(match self.language {
                Language::Zh => format!("导出 DOCX 失败: {message}"),
                Language::En => format!("Failed to export DOCX: {message}"),
            });
        }
    }

    fn add_export_text_annotation_at(&mut self, position: [i32; 2]) {
        let before = self.export_preview_state();
        let scale = Self::effective_export_label_scale(self.export_label_scale);
        let mut annotation = Self::new_export_text_annotation(self.language, position, scale);
        annotation.color = Self::annotation_color(self.export_brush_color);
        self.export_text_annotations.push(annotation);
        self.push_export_preview_undo(before);
        self.mark_export_preview_dirty();
    }

    fn new_export_text_annotation(
        _language: Language,
        position: [i32; 2],
        scale: i32,
    ) -> ExportTextAnnotation {
        ExportTextAnnotation {
            text: String::new(),
            position: Self::canvas_point(position),
            color: Self::annotation_color(Color32::from_rgb(24, 36, 56)),
            scale: Self::effective_export_label_scale(scale),
        }
    }

    fn draw_export_text_annotations<C: WaveformCanvas>(&self, canvas: &mut C) {
        for annotation in &self.export_text_annotations {
            if annotation.text.trim().is_empty() {
                continue;
            }
            canvas.text_styled(
                annotation.position.x,
                annotation.position.y,
                &annotation.text,
                Self::export_model_color(annotation.color),
                annotation.scale,
                TextStyle::Outline,
            );
        }
    }

    fn draw_export_arrow_annotations<C: WaveformCanvas>(&self, canvas: &mut C) {
        for arrow in &self.export_arrow_annotations {
            canvas.arrow(
                arrow.start.x,
                arrow.start.y,
                arrow.end.x,
                arrow.end.y,
                Self::export_model_color(arrow.color),
                arrow
                    .head_size
                    .clamp(MIN_EXPORT_ARROW_SIZE, MAX_EXPORT_ARROW_SIZE),
                arrow.width.max(1),
                Self::annotation_stroke_style(arrow.line_style),
            );
        }
    }

    fn draw_export_rectangle_annotations<C: WaveformCanvas>(&self, canvas: &mut C) {
        for rectangle in &self.export_rectangle_annotations {
            if rectangle.rect.width().abs() < 2 || rectangle.rect.height().abs() < 2 {
                continue;
            }
            match rectangle.kind {
                ShapeKind::Rectangle => canvas.stroke_rect(
                    rectangle.rect.left,
                    rectangle.rect.top,
                    rectangle.rect.right,
                    rectangle.rect.bottom,
                    Self::export_model_color(rectangle.color),
                    rectangle.width.max(1),
                ),
                ShapeKind::Ellipse => canvas.stroke_ellipse(
                    rectangle.rect.left,
                    rectangle.rect.top,
                    rectangle.rect.right,
                    rectangle.rect.bottom,
                    Self::export_model_color(rectangle.color),
                    rectangle.width.max(1),
                ),
            }
        }
    }

    fn draw_export_ink_strokes<C: WaveformCanvas>(&self, canvas: &mut C) {
        for stroke in &self.export_ink_strokes {
            let color = Self::export_model_color(stroke.color);
            for pair in stroke.points.windows(2) {
                canvas.line(
                    pair[0].x,
                    pair[0].y,
                    pair[1].x,
                    pair[1].y,
                    color,
                    stroke.width,
                );
            }
        }
    }

    fn check_batch_export_cancel(cancel: &DataCancelToken) -> Result<(), String> {
        cancel.check().map_err(|error| error.to_string())
    }

    fn run_batch_waveform_png_export(&mut self) {
        let windows = self
            .batch_export_windows
            .iter()
            .enumerate()
            .filter(|(_, window)| window.enabled)
            .map(|(index, window)| {
                (
                    index + 1,
                    window.start.min(window.end),
                    window.start.max(window.end),
                )
            })
            .collect::<Vec<_>>();
        if windows.is_empty() {
            self.batch_export_last_summary = Some(
                self.tr(
                    "请至少启用一个时间窗口。",
                    "Enable at least one time window.",
                )
                .to_owned(),
            );
            return;
        }
        if windows
            .iter()
            .any(|(_, start, end)| !start.is_finite() || !end.is_finite() || end <= start)
        {
            self.batch_export_last_summary = Some(
                self.tr("时间窗口无效。", "One or more time windows are invalid.")
                    .to_owned(),
            );
            return;
        }

        let Some(output_dir) = rfd::FileDialog::new().pick_folder() else {
            return;
        };

        let saved_time_range_mode = self.export_time_range_mode;
        let saved_manual_start = self.export_manual_start;
        let saved_manual_end = self.export_manual_end;
        let saved_pane_scope = self.export_pane_scope;
        let saved_active_pane = self.active_scope_pane;
        let saved_label_overrides = self.export_label_overrides.clone();
        let saved_label_positions = self.export_label_positions.clone();
        let saved_label_anchor_x = self.export_label_anchor_x.clone();

        self.export_time_range_mode = ExportTimeRangeMode::Manual;
        self.export_label_overrides.clear();
        self.export_label_positions.clear();
        self.export_label_anchor_x.clear();

        let base_name = Self::sanitize_file_component(&self.batch_waveform_base_name());
        let source_pane_count = self.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS)
            * self.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS);
        let pane_jobs = self.batch_export_pane_jobs(source_pane_count);
        let dataset_jobs = self.batch_export_dataset_jobs();

        let mut exported = 0usize;
        let mut errors = Vec::new();
        let cancel = DataCancelToken::new();
        for (window_index, start, end) in windows {
            if let Err(error) = Self::check_batch_export_cancel(&cancel) {
                errors.push(error);
                break;
            }
            self.export_manual_start = start;
            self.export_manual_end = end;
            for (dataset_slug, selections) in &dataset_jobs {
                if let Err(error) = Self::check_batch_export_cancel(&cancel) {
                    errors.push(error);
                    break;
                }
                if Self::plot_selection_curve_count(selections) == 0 {
                    continue;
                }
                for (pane_slug, pane_scope, active_pane) in &pane_jobs {
                    if let Err(error) = Self::check_batch_export_cancel(&cancel) {
                        errors.push(error);
                        break;
                    }
                    self.export_pane_scope = *pane_scope;
                    self.active_scope_pane =
                        (*active_pane).min(source_pane_count.saturating_sub(1));
                    let file_name = format!(
                        "{}_waveform_w{:02}_{}_{}_{}_{}.png",
                        base_name,
                        window_index,
                        Self::time_slug(start),
                        Self::time_slug(end),
                        dataset_slug,
                        pane_slug
                    );
                    let path = Self::unique_export_path(&output_dir, file_name);
                    match self.write_current_waveform_png(&path, selections) {
                        Ok(()) => exported += 1,
                        Err(error) => errors.push(format!("{}: {error}", path.display())),
                    }
                }
            }
        }

        self.export_time_range_mode = saved_time_range_mode;
        self.export_manual_start = saved_manual_start;
        self.export_manual_end = saved_manual_end;
        self.export_pane_scope = saved_pane_scope;
        self.active_scope_pane = saved_active_pane;
        self.export_label_overrides = saved_label_overrides;
        self.export_label_positions = saved_label_positions;
        self.export_label_anchor_x = saved_label_anchor_x;

        self.batch_export_last_summary = if errors.is_empty() {
            Some(match self.language {
                Language::Zh => format!("已导出 {exported} 张 PNG。"),
                Language::En => format!("Exported {exported} PNG images."),
            })
        } else {
            Some(match self.language {
                Language::Zh => format!("已导出 {exported} 张 PNG，失败 {} 张。", errors.len()),
                Language::En => format!("Exported {exported} PNG images, {} failed.", errors.len()),
            })
        };
        if !errors.is_empty() {
            self.last_error = Some(match self.language {
                Language::Zh => format!("批量导出部分失败:\n{}", errors.join("\n")),
                Language::En => format!("Batch export partially failed:\n{}", errors.join("\n")),
            });
        }
    }

    fn enabled_batch_export_windows(&self) -> Vec<(usize, f64, f64)> {
        self.batch_export_windows
            .iter()
            .enumerate()
            .filter(|(_, window)| window.enabled)
            .map(|(index, window)| {
                (
                    index + 1,
                    window.start.min(window.end),
                    window.start.max(window.end),
                )
            })
            .collect()
    }

    fn run_batch_waveform_word_export(&mut self) {
        let windows = self.enabled_batch_export_windows();
        if windows.is_empty() {
            self.batch_export_last_summary = Some(
                self.tr(
                    "请至少启用一个时间窗口。",
                    "Enable at least one time window.",
                )
                .to_owned(),
            );
            return;
        }
        if windows
            .iter()
            .any(|(_, start, end)| !start.is_finite() || !end.is_finite() || end <= start)
        {
            self.batch_export_last_summary = Some(
                self.tr("时间窗口无效。", "One or more time windows are invalid.")
                    .to_owned(),
            );
            return;
        }

        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("DOCX", &["docx"])
            .set_file_name(self.default_waveform_docx_name())
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("docx");
        }

        let saved_time_range_mode = self.export_time_range_mode;
        let saved_manual_start = self.export_manual_start;
        let saved_manual_end = self.export_manual_end;
        let saved_pane_scope = self.export_pane_scope;
        let saved_active_pane = self.active_scope_pane;
        let saved_label_overrides = self.export_label_overrides.clone();
        let saved_label_positions = self.export_label_positions.clone();
        let saved_label_anchor_x = self.export_label_anchor_x.clone();

        self.export_time_range_mode = ExportTimeRangeMode::Manual;
        self.export_label_overrides.clear();
        self.export_label_positions.clear();
        self.export_label_anchor_x.clear();

        let source_pane_count = self.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS)
            * self.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS);
        let pane_jobs = self.batch_export_pane_jobs(source_pane_count);
        let dataset_jobs = self.batch_export_dataset_jobs();
        let mut figures = Vec::new();
        let mut build_error = None;
        let cancel = DataCancelToken::new();

        'build: for (window_index, start, end) in windows {
            if let Err(error) = Self::check_batch_export_cancel(&cancel) {
                build_error = Some(error);
                break;
            }
            self.export_manual_start = start;
            self.export_manual_end = end;
            for (dataset_slug, selections) in &dataset_jobs {
                if let Err(error) = Self::check_batch_export_cancel(&cancel) {
                    build_error = Some(error);
                    break 'build;
                }
                if Self::plot_selection_curve_count(selections) == 0 {
                    continue;
                }
                for (pane_slug, pane_scope, active_pane) in &pane_jobs {
                    if let Err(error) = Self::check_batch_export_cancel(&cancel) {
                        build_error = Some(error);
                        break 'build;
                    }
                    self.export_pane_scope = *pane_scope;
                    self.active_scope_pane =
                        (*active_pane).min(source_pane_count.saturating_sub(1));
                    let caption = format!(
                        "图 {}：窗口 #{window_index} {start:.6}s - {end:.6}s {dataset_slug} {pane_slug}",
                        figures.len() + 1
                    );
                    match self.build_word_report_figure(caption, selections) {
                        Ok(figure) => figures.push(figure),
                        Err(error) => {
                            build_error = Some(error);
                            break 'build;
                        }
                    }
                }
            }
        }

        self.export_time_range_mode = saved_time_range_mode;
        self.export_manual_start = saved_manual_start;
        self.export_manual_end = saved_manual_end;
        self.export_pane_scope = saved_pane_scope;
        self.active_scope_pane = saved_active_pane;
        self.export_label_overrides = saved_label_overrides;
        self.export_label_positions = saved_label_positions;
        self.export_label_anchor_x = saved_label_anchor_x;

        if let Some(error) = build_error {
            self.batch_export_last_summary = Some(match self.language {
                Language::Zh => format!("Word 导出失败: {error}"),
                Language::En => format!("Word export failed: {error}"),
            });
            return;
        }
        if figures.is_empty() {
            self.batch_export_last_summary = Some(
                self.tr("没有可导出的波形图。", "No figures to export.")
                    .to_owned(),
            );
            return;
        }

        let figure_count = figures.len();
        let report = self.build_word_report(figures, format!("{figure_count} 个窗口"));
        match crate::word_export::write_word_report(&path, &report) {
            Ok(()) => {
                self.batch_export_last_summary = Some(match self.language {
                    Language::Zh => format!("已导出 DOCX，共 {figure_count} 张图。"),
                    Language::En => format!("Exported DOCX with {figure_count} figures."),
                });
            }
            Err(error) => {
                self.batch_export_last_summary = Some(match self.language {
                    Language::Zh => format!("Word 导出失败: {error}"),
                    Language::En => format!("Word export failed: {error}"),
                });
            }
        }
    }

    fn batch_export_dataset_jobs(&self) -> Vec<(String, PlotSelections)> {
        match self.batch_export_dataset_mode {
            BatchExportDatasetMode::Combined => {
                vec![("combined".to_owned(), self.current_plot_selections())]
            }
            BatchExportDatasetMode::EachDataset => {
                let mut jobs = Vec::new();
                let imported_len = self.imported_datasets.len();
                jobs.push((
                    Self::sanitize_file_component(&self.dataset_short_label(0)),
                    PlotSelections {
                        primary: self.selected_channels(),
                        imported: vec![Vec::new(); imported_len],
                        derived: self.selected_derived_channels(),
                    },
                ));
                for dataset_index in 0..imported_len {
                    let mut imported = vec![Vec::new(); imported_len];
                    imported[dataset_index] = self.selected_imported_channels(dataset_index);
                    jobs.push((
                        Self::sanitize_file_component(&self.dataset_short_label(dataset_index + 1)),
                        PlotSelections {
                            primary: Vec::new(),
                            imported,
                            derived: Vec::new(),
                        },
                    ));
                }
                jobs
            }
        }
    }

    fn batch_export_pane_jobs(&self, pane_count: usize) -> Vec<(String, ExportPaneScope, usize)> {
        match self.batch_export_pane_mode {
            BatchExportPaneMode::Current => vec![(
                match self.export_pane_scope {
                    ExportPaneScope::All => "current_all".to_owned(),
                    ExportPaneScope::Active => {
                        format!("current_pane{:02}", self.active_scope_pane + 1)
                    }
                },
                self.export_pane_scope,
                self.active_scope_pane,
            )],
            BatchExportPaneMode::AllPanes => {
                vec![(
                    "all_panes".to_owned(),
                    ExportPaneScope::All,
                    self.active_scope_pane,
                )]
            }
            BatchExportPaneMode::EachPane => (0..pane_count)
                .map(|pane| {
                    (
                        format!("pane{:02}", pane + 1),
                        ExportPaneScope::Active,
                        pane,
                    )
                })
                .collect(),
        }
    }

    fn plot_selection_curve_count(selections: &PlotSelections) -> usize {
        selections.primary.len()
            + selections.derived.len()
            + selections.imported.iter().map(Vec::len).sum::<usize>()
    }

    fn batch_waveform_base_name(&self) -> String {
        self.loaded_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .or_else(|| self.meta().map(|meta| meta.source_name.as_str()))
            .unwrap_or("waveform")
            .to_owned()
    }

    fn time_slug(value: f64) -> String {
        format!("{value:.6}").replace('-', "m").replace('.', "p")
    }

    fn unique_export_path(output_dir: &Path, file_name: String) -> PathBuf {
        let mut path = output_dir.join(&file_name);
        if !path.exists() {
            return path;
        }
        let file_path = Path::new(&file_name);
        let stem = file_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("waveform");
        let extension = file_path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("png");
        for index in 2.. {
            path = output_dir.join(format!("{stem}_{index}.{extension}"));
            if !path.exists() {
                return path;
            }
        }
        path
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_export_pane<'a, C: WaveformCanvas>(
        &'a self,
        canvas: &mut C,
        plot: ClipRect,
        pane_index: usize,
        pane_count: usize,
        selections: &PlotSelections,
        y_bounds: (f64, f64),
        x_min: f64,
        x_max: f64,
        _title: &str,
        label_cursor: &mut usize,
        canvas_bounds: ClipRect,
        pending_label_panes: &mut Vec<PendingExportPaneLabels<'a>>,
    ) {
        let (y_min, y_max) = y_bounds;
        let palette = self.export_style_palette();
        canvas.fill_rect(
            plot.left,
            plot.top,
            plot.right,
            plot.bottom,
            palette.plot_bg,
        );
        canvas.stroke_rect(
            plot.left,
            plot.top,
            plot.right,
            plot.bottom,
            palette.border,
            2,
        );

        for i in 1..6 {
            let x = plot.left + (plot.right - plot.left) * i / 6;
            canvas.line_styled(
                x,
                plot.top,
                x,
                plot.bottom,
                palette.grid,
                1,
                StrokeStyle::Dashed,
            );
        }
        for i in 1..5 {
            let y = plot.top + (plot.bottom - plot.top) * i / 5;
            canvas.line_styled(
                plot.left,
                y,
                plot.right,
                y,
                palette.grid,
                1,
                StrokeStyle::Dashed,
            );
        }
        canvas.text(
            plot.left,
            plot.bottom + 18,
            &format!("{:.6}s", x_min),
            palette.axis_text,
            2,
        );
        let end_label = format!("{:.6}s", x_max);
        canvas.text(
            plot.right - Canvas::text_width(&end_label, 2),
            plot.bottom + 18,
            &end_label,
            palette.axis_text,
            2,
        );
        canvas.text(
            plot.left - 82,
            plot.top,
            &format!("{:.2}", y_max),
            palette.axis_text,
            2,
        );
        canvas.text(
            plot.left - 82,
            plot.bottom - 14,
            &format!("{:.2}", y_min),
            palette.axis_text,
            2,
        );

        let mut curves = Vec::<ExportCurve<'_>>::new();
        let width_scale = palette.line_width_scale;
        for (out_index, channel_index) in selections.primary.iter().enumerate() {
            if !self.channel_in_scope_pane(*channel_index, pane_index, pane_count) {
                continue;
            }
            let (points, low_points) = if let Some(summary) = &self.prepared_plot_summary {
                (
                    summary.points.get(out_index),
                    summary.envelope_low_points.get(out_index).map(Arc::as_ref),
                )
            } else {
                (self.prepared_plot_cache.points.get(out_index), None)
            };
            let Some(points) = points else {
                continue;
            };
            let default_label = self.channel_name(*channel_index);
            let label_index = *label_cursor;
            let label = self.export_label_for(*label_cursor, default_label);
            *label_cursor += 1;
            curves.push(ExportCurve {
                label_index,
                label,
                color: self.plot_channel_color(*channel_index, 0, pane_index, pane_count),
                width: (self.visible_line_width(*channel_index) * 2.2 * width_scale)
                    .round()
                    .max(3.0) as i32,
                points,
                low_points,
            });
        }

        for (dataset_index, dataset) in self.imported_datasets.iter().enumerate() {
            let compare_selected = selections
                .imported
                .get(dataset_index)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            for (out_index, channel_index) in compare_selected.iter().enumerate() {
                if !self.channel_in_scope_pane(*channel_index, pane_index, pane_count) {
                    continue;
                }
                let (points, low_points) = if let Some(summary) = &dataset.prepared_plot_summary {
                    (
                        summary.points.get(out_index),
                        summary.envelope_low_points.get(out_index).map(Arc::as_ref),
                    )
                } else {
                    (dataset.prepared_plot_cache.points.get(out_index), None)
                };
                let Some(points) = points else {
                    continue;
                };
                let default_label = format!(
                    "{}: {}",
                    self.dataset_label(dataset_index + 1),
                    self.channel_name(*channel_index)
                );
                let label_index = *label_cursor;
                let label = self.export_label_for(*label_cursor, default_label);
                *label_cursor += 1;
                curves.push(ExportCurve {
                    label_index,
                    label,
                    color: self.plot_channel_color(
                        *channel_index,
                        dataset_index + 1,
                        pane_index,
                        pane_count,
                    ),
                    width: (self.compare_line_width(*channel_index) * 2.0 * width_scale)
                        .round()
                        .max(3.0) as i32,
                    points,
                    low_points,
                });
            }
        }

        for (out_index, derived_index) in selections.derived.iter().enumerate() {
            if !self.derived_in_scope_pane(*derived_index, pane_index, pane_count) {
                continue;
            }
            let Some(points) = self.prepared_derived_curve_cache.points.get(out_index) else {
                continue;
            };
            let default_label = self.derived_channel_name(*derived_index);
            let label_index = *label_cursor;
            let label = self.export_label_for(*label_cursor, default_label);
            *label_cursor += 1;
            curves.push(ExportCurve {
                label_index,
                label,
                color: self.derived_channel_color(*derived_index),
                width: ((DEFAULT_CHANNEL_LINE_WIDTH + 0.2) * 2.0 * width_scale)
                    .round()
                    .max(3.0) as i32,
                points,
                low_points: None,
            });
        }

        for curve in &curves {
            let color = self.export_scope_color(curve.color);
            for points in [Some(curve.points), curve.low_points].into_iter().flatten() {
                for pair in points.windows(2) {
                    let Some((x0, y0)) =
                        Self::export_map_point(pair[0], plot, x_min, x_max, y_min, y_max)
                    else {
                        continue;
                    };
                    let Some((x1, y1)) =
                        Self::export_map_point(pair[1], plot, x_min, x_max, y_min, y_max)
                    else {
                        continue;
                    };
                    canvas.line_clipped(x0, y0, x1, y1, color, curve.width, plot);
                }
            }
        }

        if self.show_cursor_a {
            self.draw_export_cursor(canvas, plot, self.cursor_a, "X1", x_min, x_max);
            self.draw_export_cursor_markers(
                canvas,
                plot,
                self.cursor_a,
                &curves,
                x_min,
                x_max,
                y_min,
                y_max,
            );
        }
        if self.show_cursor_b {
            self.draw_export_cursor(canvas, plot, self.cursor_b, "X2", x_min, x_max);
            self.draw_export_cursor_markers(
                canvas,
                plot,
                self.cursor_b,
                &curves,
                x_min,
                x_max,
                y_min,
                y_max,
            );
        }
        self.draw_export_cursor_table(canvas, plot, &curves, x_min, x_max, y_min, y_max);
        let occupied_rects =
            self.export_cursor_obstacle_rects(plot, &curves, x_min, x_max, y_min, y_max);
        pending_label_panes.push(PendingExportPaneLabels {
            plot,
            canvas_bounds,
            curves,
            occupied_rects,
            x_min,
            x_max,
            y_min,
            y_max,
        });
    }

    fn draw_export_variable_label_annotations<C: WaveformCanvas>(
        &self,
        canvas: &mut C,
        pending_label_panes: &mut [PendingExportPaneLabels<'_>],
        label_layout: &mut Vec<ExportLabelPlacement>,
    ) {
        let label_scale = Self::effective_export_label_scale(self.export_label_scale);
        let label_height = Canvas::text_height(label_scale);
        let palette = self.export_style_palette();
        for pending in pending_label_panes {
            let plot = pending.plot;
            let canvas_bounds = pending.canvas_bounds;
            let x_min = pending.x_min;
            let x_max = pending.x_max;
            let y_min = pending.y_min;
            let y_max = pending.y_max;
            let curves = &pending.curves;
            let occupied_rects = &mut pending.occupied_rects;
            let label_step = label_height + 10;
            let max_labels = ((plot.bottom - plot.top - 10) / label_step).max(0) as usize;
            for (index, curve) in curves.iter().take(max_labels).enumerate() {
                let default_target_x = x_min + (x_max - x_min) * (0.62 + 0.10 * (index % 3) as f64);
                let target_x = self
                    .export_label_anchor_x
                    .get(curve.label_index)
                    .and_then(|x| *x)
                    .filter(|x| x.is_finite() && *x >= x_min && *x <= x_max)
                    .unwrap_or(default_target_x);
                let Some((target_px, target_py)) = self
                    .export_curve_target(curve.points, target_x, x_min, x_max)
                    .and_then(|point| {
                        Self::export_map_point(point, plot, x_min, x_max, y_min, y_max)
                    })
                else {
                    continue;
                };
                let max_chars = (30 / label_scale).max(12) as usize;
                let text = Self::truncate_export_label(&curve.label, max_chars);
                let text_w = Canvas::text_width(&text, label_scale);
                let (default_label_x, default_label_y, _) = Self::export_label_position(
                    plot,
                    target_px,
                    target_py,
                    text_w,
                    label_height,
                    index,
                );
                let (label_x, label_y) = self.export_label_canvas_position(
                    curve.label_index,
                    default_label_x,
                    default_label_y,
                    canvas_bounds,
                    plot,
                    text_w,
                    label_height,
                    target_px,
                    target_py,
                    index,
                    occupied_rects,
                    curves,
                    x_min,
                    x_max,
                    y_min,
                    y_max,
                );
                let (arrow_start_x, arrow_start_y) = Self::export_arrow_start_for_label(
                    label_x,
                    label_y,
                    text_w,
                    label_height,
                    target_px,
                );
                let color = self.export_annotation_color(curve.color);
                let label_rect = [
                    label_x - 4,
                    label_y - 3,
                    label_x + text_w + 4,
                    label_y + label_height + 4,
                ];
                canvas.fill_rect(
                    label_rect[0],
                    label_rect[1],
                    label_rect[2],
                    label_rect[3],
                    palette.label_bg,
                );
                canvas.text_styled(
                    label_x,
                    label_y,
                    &text,
                    color,
                    label_scale,
                    self.export_label_font_style.text_style(),
                );
                self.draw_export_annotation_arrow(
                    canvas,
                    arrow_start_x,
                    arrow_start_y,
                    target_px,
                    target_py,
                    color,
                );
                label_layout.push(ExportLabelPlacement {
                    label_index: curve.label_index,
                    label_rect,
                    anchor_rect: [
                        target_px - 11,
                        target_py - 11,
                        target_px + 11,
                        target_py + 11,
                    ],
                    anchor_point: [target_px, target_py],
                    plot_rect: plot,
                    drag_bounds: canvas_bounds,
                });
                occupied_rects.push(Self::inflate_rect(label_rect, 8));
            }
        }
    }

    fn draw_export_cursor<C: WaveformCanvas>(
        &self,
        canvas: &mut C,
        plot: ClipRect,
        cursor_x: f64,
        label: &str,
        x_min: f64,
        x_max: f64,
    ) {
        if cursor_x < x_min || cursor_x > x_max {
            return;
        }
        let x = plot.left
            + ((cursor_x - x_min) / (x_max - x_min) * (plot.right - plot.left) as f64).round()
                as i32;
        let color = Rgba::rgb(235, 42, 48);
        let mut y = plot.top;
        while y < plot.bottom {
            canvas.line(x, y, x, (y + 14).min(plot.bottom), color, 3);
            y += 22;
        }
        canvas.fill_rect(
            x - 9,
            plot.top + 4,
            x + 10,
            plot.top + 17,
            Rgba::rgba(255, 255, 255, 220),
        );
        canvas.text(x - 7, plot.top + 7, label, color, 1);
        self.draw_export_cursor_x_axis_label(canvas, plot, x, cursor_x, label, color);
    }

    fn draw_export_cursor_x_axis_label<C: WaveformCanvas>(
        &self,
        canvas: &mut C,
        plot: ClipRect,
        cursor_px: i32,
        cursor_x: f64,
        label: &str,
        color: Rgba,
    ) {
        let text = format!("{label} x={cursor_x:.6}s");
        let scale = 2;
        let text_w = Canvas::text_width(&text, scale);
        let text_h = Canvas::text_height(scale);
        let label_x = (cursor_px - text_w / 2).clamp(plot.left + 4, plot.right - text_w - 4);
        let label_y = plot.bottom + 34;
        let palette = self.export_style_palette();
        canvas.fill_rect(
            label_x - 6,
            label_y - 4,
            label_x + text_w + 6,
            label_y + text_h + 5,
            palette.cursor_label_bg,
        );
        canvas.stroke_rect(
            label_x - 6,
            label_y - 4,
            label_x + text_w + 6,
            label_y + text_h + 5,
            color,
            1,
        );
        canvas.text_styled(label_x, label_y, &text, color, scale, TextStyle::Bold);
    }

    fn draw_export_cursor_markers<C: WaveformCanvas>(
        &self,
        canvas: &mut C,
        plot: ClipRect,
        cursor_x: f64,
        curves: &[ExportCurve<'_>],
        x_min: f64,
        x_max: f64,
        y_min: f64,
        y_max: f64,
    ) {
        if cursor_x < x_min || cursor_x > x_max {
            return;
        }
        for curve in curves {
            let Some(y) = Self::interpolated_y(curve.points, cursor_x) else {
                continue;
            };
            let Some((px, py)) = Self::export_map_point(
                PlotPoint::new(cursor_x, y),
                plot,
                x_min,
                x_max,
                y_min,
                y_max,
            ) else {
                continue;
            };
            if px < plot.left || px > plot.right || py < plot.top || py > plot.bottom {
                continue;
            }
            let color = self.export_annotation_color(curve.color);
            canvas.fill_rect(
                px - 5,
                py - 5,
                px + 6,
                py + 6,
                Rgba::rgba(255, 255, 255, 235),
            );
            canvas.stroke_rect(px - 5, py - 5, px + 6, py + 6, color, 2);
        }
    }

    fn draw_export_cursor_table<C: WaveformCanvas>(
        &self,
        canvas: &mut C,
        plot: ClipRect,
        curves: &[ExportCurve<'_>],
        x_min: f64,
        x_max: f64,
        _y_min: f64,
        _y_max: f64,
    ) {
        if !self.export_cursor_table_enabled {
            return;
        }
        let use_x1 = self.show_cursor_a && self.cursor_a >= x_min && self.cursor_a <= x_max;
        let use_x2 = self.show_cursor_b && self.cursor_b >= x_min && self.cursor_b <= x_max;
        if !(use_x1 || use_x2) || curves.is_empty() {
            return;
        }

        let mut rows = Vec::new();
        for curve in curves {
            let y1 = use_x1
                .then(|| Self::interpolated_y(curve.points, self.cursor_a))
                .flatten();
            let y2 = use_x2
                .then(|| Self::interpolated_y(curve.points, self.cursor_b))
                .flatten();
            if y1.is_some() || y2.is_some() {
                rows.push((curve, y1, y2));
            }
        }
        if rows.is_empty() {
            return;
        }

        let scale = self.export_cursor_table_text_scale();
        let text_h = Canvas::text_height(scale);
        let row_h = text_h + 9;
        let title_h = text_h + 10;
        let table_h = title_h + row_h * (rows.len() as i32 + 1) + 8;
        let table_left = plot.left;
        let table_right = plot.right;
        let table_top = plot.bottom + 62;
        let table_bottom = table_top + table_h;
        let table_w = table_right - table_left;
        if table_w <= 120 {
            return;
        }

        let palette = self.export_style_palette();
        canvas.fill_rect(
            table_left,
            table_top,
            table_right,
            table_bottom,
            palette.cursor_label_bg,
        );
        canvas.stroke_rect(
            table_left,
            table_top,
            table_right,
            table_bottom,
            palette.border,
            1,
        );

        let delta_x = if use_x1 && use_x2 {
            Some((self.cursor_b - self.cursor_a).abs())
        } else {
            None
        };
        let title = match (use_x1, use_x2, delta_x) {
            (true, true, Some(dx)) => {
                format!(
                    "X1={:.6}s    X2={:.6}s    ΔX={dx:.6}s",
                    self.cursor_a, self.cursor_b
                )
            }
            (true, false, _) => format!("X1={:.6}s", self.cursor_a),
            (false, true, _) => format!("X2={:.6}s", self.cursor_b),
            _ => String::new(),
        };
        canvas.text_styled(
            table_left + 8,
            table_top + 5,
            &title,
            palette.axis_text,
            scale,
            TextStyle::Bold,
        );
        let title_bottom = table_top + title_h;
        canvas.line(
            table_left,
            title_bottom,
            table_right,
            title_bottom,
            palette.grid,
            1,
        );

        let name_w = if table_w <= 320 {
            table_w / 2
        } else {
            (table_w * 36 / 100).clamp(150, table_w / 2)
        };
        let remaining = table_w - name_w;
        let columns = if use_x1 && use_x2 { 3 } else { 1 };
        let value_w = (remaining / columns).max(60);
        let x_name = table_left;
        let x_y1 = table_left + name_w;
        let x_y2 = x_y1 + value_w;
        let x_delta = x_y2 + value_w;
        let header_y = title_bottom + 5;
        let header_color = palette.axis_text;
        canvas.text_styled(
            x_name + 8,
            header_y,
            self.tr("变量", "Variable"),
            header_color,
            scale,
            TextStyle::Bold,
        );
        if use_x1 {
            canvas.text_styled(
                x_y1 + 8,
                header_y,
                "Y@X1",
                header_color,
                scale,
                TextStyle::Bold,
            );
        }
        if use_x2 {
            let x = if use_x1 { x_y2 } else { x_y1 };
            canvas.text_styled(
                x + 8,
                header_y,
                "Y@X2",
                header_color,
                scale,
                TextStyle::Bold,
            );
        }
        if use_x1 && use_x2 {
            canvas.text_styled(
                x_delta + 8,
                header_y,
                "ΔY",
                header_color,
                scale,
                TextStyle::Bold,
            );
        }

        let header_bottom = title_bottom + row_h;
        canvas.line(
            table_left,
            header_bottom,
            table_right,
            header_bottom,
            palette.grid,
            1,
        );
        canvas.line(x_y1, title_bottom, x_y1, table_bottom, palette.grid, 1);
        if use_x1 && use_x2 {
            canvas.line(x_y2, title_bottom, x_y2, table_bottom, palette.grid, 1);
            canvas.line(
                x_delta,
                title_bottom,
                x_delta,
                table_bottom,
                palette.grid,
                1,
            );
        }

        for (row_index, (curve, y1, y2)) in rows.into_iter().enumerate() {
            let top = header_bottom + row_h * row_index as i32;
            let bottom = top + row_h;
            if row_index % 2 == 0 {
                canvas.fill_rect(
                    table_left + 1,
                    top,
                    table_right - 1,
                    bottom,
                    Rgba::rgba(245, 249, 255, 185),
                );
            }
            canvas.line(table_left, bottom, table_right, bottom, palette.grid, 1);
            let color = self.export_annotation_color(curve.color);
            canvas.fill_rect(x_name + 8, top + 7, x_name + 20, top + 19, color);
            let name = Self::truncate_export_label(&curve.label, 22);
            canvas.text_styled(
                x_name + 26,
                top + 5,
                &name,
                color,
                scale,
                self.export_label_font_style.text_style(),
            );

            if use_x1 {
                canvas.text(
                    x_y1 + 8,
                    top + 5,
                    &Self::format_export_cursor_value(y1),
                    palette.axis_text,
                    scale,
                );
            }
            if use_x2 {
                let x = if use_x1 { x_y2 } else { x_y1 };
                canvas.text(
                    x + 8,
                    top + 5,
                    &Self::format_export_cursor_value(y2),
                    palette.axis_text,
                    scale,
                );
            }
            if use_x1 && use_x2 {
                let delta = match (y1, y2) {
                    (Some(a), Some(b)) => Some(b - a),
                    _ => None,
                };
                canvas.text(
                    x_delta + 8,
                    top + 5,
                    &Self::format_export_cursor_value(delta),
                    palette.axis_text,
                    scale,
                );
            }
        }
    }

    fn export_curve_target(
        &self,
        points: &[PlotPoint],
        target_x: f64,
        x_min: f64,
        x_max: f64,
    ) -> Option<PlotPoint> {
        if let Some(y) = Self::interpolated_y(points, target_x) {
            return Some(PlotPoint::new(target_x, y));
        }
        points
            .iter()
            .rev()
            .find(|point| {
                point.x >= x_min && point.x <= x_max && point.y.is_finite() && point.x.is_finite()
            })
            .copied()
    }

    fn export_map_point(
        point: PlotPoint,
        plot: ClipRect,
        x_min: f64,
        x_max: f64,
        y_min: f64,
        y_max: f64,
    ) -> Option<(i32, i32)> {
        if !point.x.is_finite()
            || !point.y.is_finite()
            || x_max <= x_min
            || y_max <= y_min
            || !y_min.is_finite()
            || !y_max.is_finite()
        {
            return None;
        }
        let x = plot.left as f64
            + (point.x - x_min) / (x_max - x_min) * (plot.right - plot.left) as f64;
        let y = plot.bottom as f64
            - (point.y - y_min) / (y_max - y_min) * (plot.bottom - plot.top) as f64;
        Some((x.round() as i32, y.round() as i32))
    }

    fn export_label_position(
        plot: ClipRect,
        target_px: i32,
        target_py: i32,
        text_w: i32,
        text_h: i32,
        index: usize,
    ) -> (i32, i32, i32) {
        let prefer_right = target_px < plot.left + (plot.right - plot.left) * 2 / 3;
        let gap = 20;
        let y_offsets = [
            -(text_h + 12),
            10,
            text_h + 16,
            -(text_h * 2 + 22),
            text_h * 2 + 24,
        ];
        let label_y = (target_py + y_offsets[index % y_offsets.len()])
            .clamp(plot.top + 8, plot.bottom - text_h - 8);
        let label_x = if prefer_right {
            (target_px + gap).clamp(plot.left + 8, plot.right - text_w - 8)
        } else {
            (target_px - gap - text_w).clamp(plot.left + 8, plot.right - text_w - 8)
        };
        let arrow_start_x = if prefer_right {
            label_x - 5
        } else {
            label_x + text_w + 5
        };
        (label_x, label_y, arrow_start_x)
    }

    fn export_label_canvas_position(
        &self,
        label_index: usize,
        default_x: i32,
        default_y: i32,
        canvas_bounds: ClipRect,
        plot: ClipRect,
        text_w: i32,
        text_h: i32,
        target_px: i32,
        target_py: i32,
        label_order: usize,
        occupied_rects: &[[i32; 4]],
        curves: &[ExportCurve<'_>],
        x_min: f64,
        x_max: f64,
        y_min: f64,
        y_max: f64,
    ) -> (i32, i32) {
        if let Some([x, y]) = self
            .export_label_positions
            .get(label_index)
            .and_then(|position| *position)
        {
            return Self::clamp_export_label_position([x, y], canvas_bounds, text_w, text_h);
        }
        self.auto_export_label_position(
            plot,
            target_px,
            target_py,
            default_x,
            default_y,
            text_w,
            text_h,
            label_order,
            occupied_rects,
            curves,
            x_min,
            x_max,
            y_min,
            y_max,
        )
    }

    fn clamp_export_label_position(
        position: [i32; 2],
        bounds: ClipRect,
        text_w: i32,
        text_h: i32,
    ) -> (i32, i32) {
        let point = export_annotation::clamp_label_position(
            Self::canvas_point(position),
            CanvasRect {
                left: bounds.left,
                top: bounds.top,
                right: bounds.right,
                bottom: bounds.bottom,
            },
            text_w,
            text_h,
        );
        (point.x, point.y)
    }

    #[allow(clippy::too_many_arguments)]
    fn auto_export_label_position(
        &self,
        plot: ClipRect,
        target_px: i32,
        target_py: i32,
        default_x: i32,
        default_y: i32,
        text_w: i32,
        text_h: i32,
        label_order: usize,
        occupied_rects: &[[i32; 4]],
        curves: &[ExportCurve<'_>],
        x_min: f64,
        x_max: f64,
        y_min: f64,
        y_max: f64,
    ) -> (i32, i32) {
        let side_order = if target_px < plot.left + (plot.right - plot.left) * 2 / 3 {
            [1, -1]
        } else {
            [-1, 1]
        };
        let mut best = (default_x, default_y, f64::INFINITY);
        let gaps = [18, 30, 44, 62, 84];
        let y_offsets = [
            -(text_h + 12),
            8,
            text_h + 16,
            -(text_h * 2 + 24),
            text_h * 2 + 28,
            -(text_h * 3 + 36),
            text_h * 3 + 40,
        ];

        for side in side_order {
            for (gap_index, gap) in gaps.iter().enumerate() {
                for (offset_index, y_offset) in y_offsets.iter().enumerate() {
                    let x = if side > 0 {
                        target_px + gap
                    } else {
                        target_px - gap - text_w
                    }
                    .clamp(plot.left + 5, plot.right - text_w - 5);
                    let y = (target_py + y_offset).clamp(plot.top + 5, plot.bottom - text_h - 5);
                    let score = self.export_label_candidate_score(
                        [x - 4, y - 3, x + text_w + 4, y + text_h + 4],
                        target_px,
                        target_py,
                        occupied_rects,
                        curves,
                        plot,
                        x_min,
                        x_max,
                        y_min,
                        y_max,
                    ) + (gap_index as f64 * 7.0)
                        + (offset_index as f64 * 3.0)
                        + ((x - default_x).abs() + (y - default_y).abs()) as f64 * 0.03
                        + label_order as f64 * 0.2;
                    if score < best.2 {
                        best = (x, y, score);
                    }
                }
            }
        }
        (best.0, best.1)
    }

    fn export_arrow_start_for_label(
        label_x: i32,
        label_y: i32,
        text_w: i32,
        text_h: i32,
        target_px: i32,
    ) -> (i32, i32) {
        let x = if target_px < label_x {
            label_x - 5
        } else {
            label_x + text_w + 5
        };
        (x, label_y + text_h / 2)
    }

    #[allow(clippy::too_many_arguments)]
    fn export_label_candidate_score(
        &self,
        rect: [i32; 4],
        target_px: i32,
        target_py: i32,
        occupied_rects: &[[i32; 4]],
        curves: &[ExportCurve<'_>],
        plot: ClipRect,
        x_min: f64,
        x_max: f64,
        y_min: f64,
        y_max: f64,
    ) -> f64 {
        let center_x = (rect[0] + rect[2]) / 2;
        let center_y = (rect[1] + rect[3]) / 2;
        let mut score =
            (((center_x - target_px).pow(2) + (center_y - target_py).pow(2)) as f64).sqrt() * 0.08;
        let (arrow_start_x, arrow_start_y) = Self::export_arrow_start_for_label(
            rect[0] + 4,
            rect[1] + 3,
            rect[2] - rect[0] - 8,
            rect[3] - rect[1] - 7,
            target_px,
        );
        for occupied in occupied_rects {
            let overlap = Self::rect_overlap_area(rect, *occupied);
            if overlap > 0 {
                score += overlap as f64 * 20.0;
            }
            if Self::segment_hits_rect(
                arrow_start_x,
                arrow_start_y,
                target_px,
                target_py,
                *occupied,
            ) {
                score += 650.0;
            }
        }
        if Self::rect_contains_point(rect, target_px, target_py) {
            score += 1_200.0;
        }
        score += self.export_curve_coverage_penalty(
            rect,
            arrow_start_x,
            arrow_start_y,
            target_px,
            target_py,
            curves,
            plot,
            x_min,
            x_max,
            y_min,
            y_max,
        );
        score
    }

    #[allow(clippy::too_many_arguments)]
    fn export_curve_coverage_penalty(
        &self,
        rect: [i32; 4],
        arrow_start_x: i32,
        arrow_start_y: i32,
        target_px: i32,
        target_py: i32,
        curves: &[ExportCurve<'_>],
        plot: ClipRect,
        x_min: f64,
        x_max: f64,
        y_min: f64,
        y_max: f64,
    ) -> f64 {
        let mut penalty = 0.0;
        let expanded_rect = Self::inflate_rect(rect, 3);
        for curve in curves {
            let step = (curve.points.len() / 500).max(1);
            for point in curve.points.iter().step_by(step) {
                let Some((px, py)) =
                    Self::export_map_point(*point, plot, x_min, x_max, y_min, y_max)
                else {
                    continue;
                };
                if !Self::rect_contains_point(
                    [plot.left, plot.top, plot.right, plot.bottom],
                    px,
                    py,
                ) {
                    continue;
                }
                if Self::rect_contains_point(expanded_rect, px, py) {
                    penalty += 42.0;
                }
                let distance = Self::point_segment_distance_sq(
                    px as f64,
                    py as f64,
                    arrow_start_x as f64,
                    arrow_start_y as f64,
                    target_px as f64,
                    target_py as f64,
                );
                if distance < 36.0 {
                    penalty += 1.4;
                }
            }
        }
        penalty
    }

    fn export_cursor_obstacle_rects(
        &self,
        plot: ClipRect,
        curves: &[ExportCurve<'_>],
        x_min: f64,
        x_max: f64,
        y_min: f64,
        y_max: f64,
    ) -> Vec<[i32; 4]> {
        let mut rects = Vec::new();
        if self.show_cursor_a {
            self.collect_export_cursor_obstacles(
                &mut rects,
                plot,
                self.cursor_a,
                "X1",
                curves,
                x_min,
                x_max,
                y_min,
                y_max,
            );
        }
        if self.show_cursor_b {
            self.collect_export_cursor_obstacles(
                &mut rects,
                plot,
                self.cursor_b,
                "X2",
                curves,
                x_min,
                x_max,
                y_min,
                y_max,
            );
        }
        rects
    }

    fn collect_export_cursor_obstacles(
        &self,
        rects: &mut Vec<[i32; 4]>,
        plot: ClipRect,
        cursor_x: f64,
        cursor_label: &str,
        _curves: &[ExportCurve<'_>],
        x_min: f64,
        x_max: f64,
        _y_min: f64,
        _y_max: f64,
    ) {
        if cursor_x < x_min || cursor_x > x_max {
            return;
        }
        let cursor_px = plot.left
            + ((cursor_x - x_min) / (x_max - x_min) * (plot.right - plot.left) as f64).round()
                as i32;
        let x_axis_text = format!("{cursor_label} x={cursor_x:.6}s");
        let x_axis_scale = 2;
        let x_axis_w = Canvas::text_width(&x_axis_text, x_axis_scale);
        let x_axis_h = Canvas::text_height(x_axis_scale);
        let x_axis_x = (cursor_px - x_axis_w / 2).clamp(plot.left + 4, plot.right - x_axis_w - 4);
        let x_axis_y = plot.bottom + 34;
        rects.push(Self::inflate_rect(
            [
                x_axis_x - 6,
                x_axis_y - 4,
                x_axis_x + x_axis_w + 6,
                x_axis_y + x_axis_h + 5,
            ],
            4,
        ));
    }

    fn rect_overlap_area(a: [i32; 4], b: [i32; 4]) -> i32 {
        let width = (a[2].min(b[2]) - a[0].max(b[0])).max(0);
        let height = (a[3].min(b[3]) - a[1].max(b[1])).max(0);
        width * height
    }

    fn rect_contains_point(rect: [i32; 4], x: i32, y: i32) -> bool {
        x >= rect[0] && x <= rect[2] && y >= rect[1] && y <= rect[3]
    }

    fn inflate_rect(rect: [i32; 4], amount: i32) -> [i32; 4] {
        [
            rect[0] - amount,
            rect[1] - amount,
            rect[2] + amount,
            rect[3] + amount,
        ]
    }

    fn segment_hits_rect(x0: i32, y0: i32, x1: i32, y1: i32, rect: [i32; 4]) -> bool {
        if Self::rect_contains_point(rect, x0, y0) || Self::rect_contains_point(rect, x1, y1) {
            return true;
        }
        let steps = (((x1 - x0).abs().max((y1 - y0).abs())) / 6).clamp(1, 120);
        for step in 1..steps {
            let t = step as f64 / steps as f64;
            let x = x0 as f64 + (x1 - x0) as f64 * t;
            let y = y0 as f64 + (y1 - y0) as f64 * t;
            if Self::rect_contains_point(rect, x.round() as i32, y.round() as i32) {
                return true;
            }
        }
        false
    }

    fn point_segment_distance_sq(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
        let vx = bx - ax;
        let vy = by - ay;
        let wx = px - ax;
        let wy = py - ay;
        let len_sq = vx * vx + vy * vy;
        if len_sq <= f64::EPSILON {
            return (px - ax).powi(2) + (py - ay).powi(2);
        }
        let t = ((wx * vx + wy * vy) / len_sq).clamp(0.0, 1.0);
        let cx = ax + vx * t;
        let cy = ay + vy * t;
        (px - cx).powi(2) + (py - cy).powi(2)
    }

    fn stroke_near_point(stroke: &ExportInkStroke, point: CanvasPoint, radius: f64) -> bool {
        let radius_sq = radius * radius;
        if stroke.points.len() <= 1 {
            return stroke.points.iter().any(|p| {
                let dx = (p.x - point.x) as f64;
                let dy = (p.y - point.y) as f64;
                dx * dx + dy * dy <= radius_sq
            });
        }
        stroke.points.windows(2).any(|pair| {
            export_annotation::point_segment_distance_sq(point, pair[0], pair[1]) <= radius_sq
        })
    }

    fn export_color(color: Color32) -> Rgba {
        Rgba::rgba(color.r(), color.g(), color.b(), color.a())
    }

    fn export_model_color(color: AnnotationColor) -> Rgba {
        Rgba::rgba(color.r, color.g, color.b, color.a)
    }

    fn annotation_color(color: Color32) -> AnnotationColor {
        AnnotationColor {
            r: color.r(),
            g: color.g(),
            b: color.b(),
            a: color.a(),
        }
    }

    fn annotation_color32(color: AnnotationColor) -> Color32 {
        Color32::from_rgba_premultiplied(color.r, color.g, color.b, color.a)
    }

    fn canvas_point(position: [i32; 2]) -> CanvasPoint {
        CanvasPoint::new(position[0], position[1])
    }

    fn annotation_line_style(style: ExportArrowLineStyle) -> AnnotationLineStyle {
        match style.base_style() {
            ExportArrowLineStyle::Dashed => AnnotationLineStyle::Dashed,
            ExportArrowLineStyle::Dotted => AnnotationLineStyle::Dotted,
            ExportArrowLineStyle::Solid
            | ExportArrowLineStyle::Thick
            | ExportArrowLineStyle::Double => AnnotationLineStyle::Solid,
        }
    }

    fn annotation_stroke_style(style: AnnotationLineStyle) -> StrokeStyle {
        match style {
            AnnotationLineStyle::Solid => StrokeStyle::Solid,
            AnnotationLineStyle::Dashed => StrokeStyle::Dashed,
            AnnotationLineStyle::Dotted => StrokeStyle::Dotted,
        }
    }

    fn export_style_palette(&self) -> ExportStylePalette {
        ExportStylePreset::Screenshot.palette()
    }

    fn export_arrow_style_controls(&mut self, ui: &mut egui::Ui, id: &'static str) -> bool {
        let mut changed = false;
        let mut base_style = self.export_arrow_line_style.base_style();
        egui::ComboBox::from_id_source((id, "line"))
            .selected_text(base_style.base_label(self.language))
            .show_ui(ui, |ui| {
                for style in ExportArrowLineStyle::BASE {
                    changed |= ui
                        .selectable_value(&mut base_style, style, style.base_label(self.language))
                        .changed();
                }
            });
        if changed {
            self.export_arrow_line_style = base_style;
        }
        let mut thick = self.export_arrow_line_style == ExportArrowLineStyle::Thick;
        let mut double = self.export_arrow_line_style == ExportArrowLineStyle::Double;
        if ui
            .checkbox(&mut thick, self.tr("粗箭头", "Thick"))
            .changed()
        {
            self.export_arrow_line_style = if thick {
                ExportArrowLineStyle::Thick
            } else {
                base_style
            };
            changed = true;
        }
        if ui
            .checkbox(&mut double, self.tr("双线箭头", "Double"))
            .changed()
        {
            self.export_arrow_line_style = if double {
                ExportArrowLineStyle::Double
            } else {
                base_style
            };
            changed = true;
        }
        changed
    }

    fn export_dpi_value(&self) -> u32 {
        self.export_dpi_value.clamp(50, 2400)
    }

    fn export_resolution_for_dpi_value(dpi_value: u32) -> ExportResolution {
        if dpi_value <= 150 {
            ExportResolution::Standard
        } else {
            ExportResolution::Ultra
        }
    }

    fn sync_export_resolution_to_dpi(&mut self) {
        self.export_resolution = Self::export_resolution_for_dpi_value(self.export_dpi_value());
    }

    fn export_dpi_selected_text(&self) -> String {
        let value = self.export_dpi_value();
        if let Some(preset) = ExportDpi::ALL
            .iter()
            .copied()
            .find(|preset| preset.value() == value)
        {
            preset.label(self.language).to_owned()
        } else {
            format!("{value} DPI")
        }
    }

    fn export_dpi_controls(&mut self, ui: &mut egui::Ui, id: &'static str) -> bool {
        let mut changed = false;
        egui::ComboBox::from_id_source((id, "preset"))
            .selected_text(self.export_dpi_selected_text())
            .show_ui(ui, |ui| {
                for dpi in ExportDpi::ALL {
                    if ui
                        .selectable_label(
                            self.export_dpi_value() == dpi.value(),
                            dpi.label(self.language),
                        )
                        .clicked()
                    {
                        self.export_dpi = dpi;
                        self.export_dpi_value = dpi.value();
                        self.sync_export_resolution_to_dpi();
                        changed = true;
                    }
                }
            });
        changed
    }

    fn export_scope_color(&self, color: Color32) -> Rgba {
        let r = color.r() as f32;
        let g = color.g() as f32;
        let b = color.b() as f32;
        if self.export_style_preset == ExportStylePreset::PaperMono {
            let luminance = (0.2126 * r + 0.7152 * g + 0.0722 * b)
                .round()
                .clamp(0.0, 255.0) as u8;
            let gray = if luminance > 170 { 75 } else { 35 };
            return Rgba::rgb(gray, gray, gray);
        }
        if self.export_style_preset == ExportStylePreset::HighContrastPrint {
            let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            let factor = if luminance > 185.0 { 0.42 } else { 0.72 };
            return Rgba::rgb(
                (r * factor).round().clamp(0.0, 255.0) as u8,
                (g * factor).round().clamp(0.0, 255.0) as u8,
                (b * factor).round().clamp(0.0, 255.0) as u8,
            );
        }
        let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let factor = if luminance > 185.0 { 0.58 } else { 0.86 };
        Rgba::rgb(
            (r * factor).round().clamp(0.0, 255.0) as u8,
            (g * factor).round().clamp(0.0, 255.0) as u8,
            (b * factor).round().clamp(0.0, 255.0) as u8,
        )
    }

    fn export_annotation_color(&self, curve_color: Color32) -> Rgba {
        if matches!(
            self.export_style_preset,
            ExportStylePreset::PaperMono | ExportStylePreset::HighContrastPrint
        ) {
            return Rgba::rgb(0, 0, 0);
        }
        match self.export_arrow_color_style {
            ExportArrowColorStyle::Curve => self.export_scope_color(curve_color),
            ExportArrowColorStyle::Dark => Rgba::rgb(12, 21, 35),
            ExportArrowColorStyle::Red => Rgba::rgb(220, 20, 38),
            ExportArrowColorStyle::Blue => Rgba::rgb(18, 86, 210),
            ExportArrowColorStyle::Custom => Self::export_color(self.export_arrow_custom_color),
        }
    }

    fn export_arrow_width(&self) -> i32 {
        (self.export_arrow_size / 5.0).round().clamp(1.0, 6.0) as i32
    }

    fn manual_export_arrow_head_size(enabled: bool, arrow_size: f32) -> f32 {
        if enabled {
            arrow_size.clamp(MIN_EXPORT_ARROW_SIZE, MAX_EXPORT_ARROW_SIZE)
        } else {
            0.0
        }
    }

    fn draw_export_annotation_arrow<C: WaveformCanvas>(
        &self,
        canvas: &mut C,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: Rgba,
    ) {
        let style = self.export_arrow_line_style;
        let width = self.export_arrow_width() + style.width_extra();
        let head_size = self
            .export_arrow_size
            .clamp(MIN_EXPORT_ARROW_SIZE, MAX_EXPORT_ARROW_SIZE)
            * style.head_scale();
        if style == ExportArrowLineStyle::Double {
            let dx = (x1 - x0) as f32;
            let dy = (y1 - y0) as f32;
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            let ox = (-dy / len * 3.0).round() as i32;
            let oy = (dx / len * 3.0).round() as i32;
            canvas.arrow(
                x0 + ox,
                y0 + oy,
                x1 + ox,
                y1 + oy,
                color,
                head_size,
                width,
                StrokeStyle::Solid,
            );
            canvas.arrow(
                x0 - ox,
                y0 - oy,
                x1 - ox,
                y1 - oy,
                color,
                head_size,
                width,
                StrokeStyle::Solid,
            );
        } else {
            canvas.arrow(
                x0,
                y0,
                x1,
                y1,
                color,
                head_size,
                width,
                style.stroke_style(),
            );
        }
    }

    fn truncate_export_label(label: &str, max_chars: usize) -> String {
        let mut output = label.chars().take(max_chars).collect::<String>();
        if label.chars().count() > max_chars {
            output.push_str("...");
        }
        output
    }

    fn write_dataset_export(
        &self,
        source: Arc<dyn DataSource>,
        meta: &DatasetMeta,
        dataset_index: usize,
        channels: &[usize],
        path: &Path,
        format: DatasetExportFormat,
        start_time: f64,
        end_time: f64,
    ) -> Result<u64, String> {
        let start_time = start_time.max(meta.start_time);
        let end_time = end_time.min(meta.end_time);
        if !start_time.is_finite() || !end_time.is_finite() || end_time <= start_time {
            return Err(self
                .tr("Invalid export range.", "Invalid export range.")
                .to_owned());
        }
        if channels.is_empty() {
            return Err(self
                .tr(
                    "没有可导出的通道。",
                    "No channels are available for export.",
                )
                .to_owned());
        }
        match format {
            DatasetExportFormat::StandardCsv => self.write_dataset_delimited(
                source,
                meta,
                dataset_index,
                channels,
                path,
                b',',
                start_time,
                end_time,
            ),
            DatasetExportFormat::DataCsv => self.write_dataset_metadata_csv(
                source,
                meta,
                dataset_index,
                channels,
                path,
                start_time,
                end_time,
            ),
            DatasetExportFormat::Tsv => self.write_dataset_delimited(
                source,
                meta,
                dataset_index,
                channels,
                path,
                b'\t',
                start_time,
                end_time,
            ),
            DatasetExportFormat::Json => self.write_dataset_json(
                source,
                meta,
                dataset_index,
                channels,
                path,
                start_time,
                end_time,
            ),
        }
    }

    fn export_channel_names_for_channels(
        &self,
        meta: &DatasetMeta,
        dataset_index: usize,
        channels: &[usize],
    ) -> Vec<String> {
        channels
            .iter()
            .map(|channel_index| {
                let channel = meta
                    .channels
                    .iter()
                    .find(|channel| channel.index == *channel_index);
                let name = if dataset_index == 0 {
                    self.display_names
                        .get(*channel_index)
                        .filter(|name| !name.trim().is_empty())
                        .cloned()
                        .or_else(|| channel.map(|channel| channel.name.clone()))
                        .unwrap_or_else(|| format!("CH{}", *channel_index + 1))
                } else {
                    channel
                        .map(|channel| channel.name.clone())
                        .unwrap_or_else(|| format!("CH{}", *channel_index + 1))
                };
                if name.trim().is_empty() {
                    format!("CH{}", *channel_index + 1)
                } else {
                    name
                }
            })
            .collect()
    }

    fn estimated_export_sample_count(meta: &DatasetMeta, start_time: f64, end_time: f64) -> u64 {
        if end_time <= start_time || meta.nominal_sample_rate_hz <= 0.0 {
            return 0;
        }
        let count = ((end_time - start_time) * meta.nominal_sample_rate_hz).floor() as u64 + 1;
        count.min(meta.sample_count)
    }

    fn cursor_display_range(&self) -> Option<(f64, f64)> {
        if !(self.show_cursor_a && self.show_cursor_b) {
            return None;
        }
        let start = self.cursor_a.min(self.cursor_b);
        let end = self.cursor_a.max(self.cursor_b);
        (start.is_finite() && end.is_finite() && end > start).then_some((start, end))
    }

    fn hover_in_cursor_range(&self, hover_time: Option<f64>) -> bool {
        let Some(hover_time) = hover_time else {
            return false;
        };
        let Some((start, end)) = self.cursor_display_range() else {
            return false;
        };
        hover_time >= start && hover_time <= end
    }

    fn cursor_export_range_for_dataset(&self, dataset_index: usize) -> Option<(f64, f64)> {
        let (start, end) = self.cursor_display_range()?;
        let meta = self.dataset_meta_by_index(dataset_index)?;
        let offset = self.dataset_time_offset(dataset_index);
        let source_start = (start - offset).max(meta.start_time);
        let source_end = (end - offset).min(meta.end_time);
        (source_start.is_finite() && source_end.is_finite() && source_end > source_start)
            .then_some((source_start, source_end))
    }

    fn write_dataset_delimited(
        &self,
        source: Arc<dyn DataSource>,
        meta: &DatasetMeta,
        dataset_index: usize,
        channels: &[usize],
        path: &Path,
        delimiter: u8,
        start_time: f64,
        end_time: f64,
    ) -> Result<u64, String> {
        let mut writer = csv::WriterBuilder::new()
            .has_headers(false)
            .delimiter(delimiter)
            .from_path(path)
            .map_err(|error| error.to_string())?;

        let mut header = Vec::with_capacity(channels.len() + 1);
        header.push("time".to_owned());
        header.extend(self.export_channel_names_for_channels(meta, dataset_index, channels));
        writer
            .write_record(header)
            .map_err(|error| error.to_string())?;

        let sample_rate_hz = meta.nominal_sample_rate_hz.max(1.0);
        let chunk_duration =
            (EXPORT_CHUNK_SAMPLES as f64 / sample_rate_hz).max(1.0 / sample_rate_hz);
        let mut start_time = start_time;
        let mut last_written_time: Option<f64> = None;
        let mut rows = 0_u64;

        while start_time <= end_time {
            let range_end = (start_time + chunk_duration).min(end_time);
            let block = source
                .read_range(start_time, range_end, channels, EXPORT_CHUNK_SAMPLES + 2)
                .map_err(|error| error.to_string())?;

            for (row_index, time) in block.times.iter().enumerate() {
                if last_written_time.is_some_and(|last| *time <= last) {
                    continue;
                }
                let mut record = Vec::with_capacity(channels.len() + 1);
                record.push(format!("{time:.12}"));
                for values in &block.channels {
                    let value = values.get(row_index).copied().unwrap_or(f32::NAN);
                    if value.is_finite() {
                        record.push(value.to_string());
                    } else {
                        record.push(String::new());
                    }
                }
                writer
                    .write_record(record)
                    .map_err(|error| error.to_string())?;
                last_written_time = Some(*time);
                rows += 1;
            }

            if range_end >= end_time {
                break;
            }
            start_time = range_end;
        }

        writer.flush().map_err(|error| error.to_string())?;
        Ok(rows)
    }

    fn write_dataset_metadata_csv(
        &self,
        source: Arc<dyn DataSource>,
        meta: &DatasetMeta,
        dataset_index: usize,
        channels: &[usize],
        path: &Path,
        start_time: f64,
        end_time: f64,
    ) -> Result<u64, String> {
        let mut writer = csv::WriterBuilder::new()
            .has_headers(false)
            .from_path(path)
            .map_err(|error| error.to_string())?;

        let source_path = self
            .dataset_path_by_index(dataset_index)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| meta.source_name.clone());
        let sample_interval = if meta.nominal_sample_rate_hz > 0.0 {
            1.0 / meta.nominal_sample_rate_hz
        } else {
            0.0
        };
        writer
            .write_record(["file_path", source_path.as_str()])
            .map_err(|error| error.to_string())?;
        writer
            .write_record(["dt", &format!("{sample_interval:.12}")])
            .map_err(|error| error.to_string())?;
        writer
            .write_record([
                "Number_of_Point",
                &Self::estimated_export_sample_count(meta, start_time, end_time).to_string(),
            ])
            .map_err(|error| error.to_string())?;
        writer
            .write_record(["END"])
            .map_err(|error| error.to_string())?;
        writer
            .write_record(self.export_channel_names_for_channels(meta, dataset_index, channels))
            .map_err(|error| error.to_string())?;

        let sample_rate_hz = meta.nominal_sample_rate_hz.max(1.0);
        let chunk_duration =
            (EXPORT_CHUNK_SAMPLES as f64 / sample_rate_hz).max(1.0 / sample_rate_hz);
        let mut start_time = start_time;
        let mut last_written_time: Option<f64> = None;
        let mut rows = 0_u64;

        while start_time <= end_time {
            let range_end = (start_time + chunk_duration).min(end_time);
            let block = source
                .read_range(start_time, range_end, channels, EXPORT_CHUNK_SAMPLES + 2)
                .map_err(|error| error.to_string())?;

            for (row_index, time) in block.times.iter().enumerate() {
                if last_written_time.is_some_and(|last| *time <= last) {
                    continue;
                }
                let mut record = Vec::with_capacity(channels.len());
                for values in &block.channels {
                    let value = values.get(row_index).copied().unwrap_or(f32::NAN);
                    if value.is_finite() {
                        record.push(value.to_string());
                    } else {
                        record.push(String::new());
                    }
                }
                writer
                    .write_record(record)
                    .map_err(|error| error.to_string())?;
                last_written_time = Some(*time);
                rows += 1;
            }

            if range_end >= end_time {
                break;
            }
            start_time = range_end;
        }

        writer.flush().map_err(|error| error.to_string())?;
        Ok(rows)
    }

    fn write_dataset_json(
        &self,
        source: Arc<dyn DataSource>,
        meta: &DatasetMeta,
        dataset_index: usize,
        channels: &[usize],
        path: &Path,
        start_time: f64,
        end_time: f64,
    ) -> Result<u64, String> {
        let file = File::create(path).map_err(|error| error.to_string())?;
        let mut writer = BufWriter::new(file);
        let channel_names = self.export_channel_names_for_channels(meta, dataset_index, channels);

        write!(writer, "{{\n  \"source_name\": ").map_err(|error| error.to_string())?;
        serde_json::to_writer(&mut writer, &meta.source_name).map_err(|error| error.to_string())?;
        write!(
            writer,
            ",\n  \"sample_rate_hz\": {},\n  \"start_time\": {},\n  \"end_time\": {},\n  \"sample_count\": {},\n  \"channels\": [",
            meta.nominal_sample_rate_hz,
            start_time,
            end_time,
            Self::estimated_export_sample_count(meta, start_time, end_time)
        )
        .map_err(|error| error.to_string())?;

        for (index, channel_index) in channels.iter().enumerate() {
            let channel = meta
                .channels
                .iter()
                .find(|channel| channel.index == *channel_index);
            if index > 0 {
                write!(writer, ",").map_err(|error| error.to_string())?;
            }
            write!(writer, "\n    {{\"index\": {}, \"name\": ", channel_index)
                .map_err(|error| error.to_string())?;
            serde_json::to_writer(&mut writer, &channel_names[index])
                .map_err(|error| error.to_string())?;
            write!(writer, ", \"unit\": ").map_err(|error| error.to_string())?;
            let unit = channel.map(|channel| channel.unit.as_str()).unwrap_or("");
            serde_json::to_writer(&mut writer, unit).map_err(|error| error.to_string())?;
            write!(writer, "}}").map_err(|error| error.to_string())?;
        }
        write!(writer, "\n  ],\n  \"samples\": [").map_err(|error| error.to_string())?;

        let sample_rate_hz = meta.nominal_sample_rate_hz.max(1.0);
        let chunk_duration =
            (EXPORT_CHUNK_SAMPLES as f64 / sample_rate_hz).max(1.0 / sample_rate_hz);
        let mut start_time = start_time;
        let mut last_written_time: Option<f64> = None;
        let mut rows = 0_u64;

        while start_time <= end_time {
            let range_end = (start_time + chunk_duration).min(end_time);
            let block = source
                .read_range(start_time, range_end, channels, EXPORT_CHUNK_SAMPLES + 2)
                .map_err(|error| error.to_string())?;

            for (row_index, time) in block.times.iter().enumerate() {
                if last_written_time.is_some_and(|last| *time <= last) {
                    continue;
                }
                if rows > 0 {
                    write!(writer, ",").map_err(|error| error.to_string())?;
                }
                write!(writer, "\n    {{\"time\": {:.12}, \"values\": [", time)
                    .map_err(|error| error.to_string())?;
                for (value_index, values) in block.channels.iter().enumerate() {
                    if value_index > 0 {
                        write!(writer, ",").map_err(|error| error.to_string())?;
                    }
                    let value = values.get(row_index).copied().unwrap_or(f32::NAN);
                    if value.is_finite() {
                        write!(writer, "{value}").map_err(|error| error.to_string())?;
                    } else {
                        write!(writer, "null").map_err(|error| error.to_string())?;
                    }
                }
                write!(writer, "]}}").map_err(|error| error.to_string())?;
                last_written_time = Some(*time);
                rows += 1;
            }

            if range_end >= end_time {
                break;
            }
            start_time = range_end;
        }

        write!(writer, "\n  ]\n}}\n").map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())?;
        Ok(rows)
    }

    fn export_config(&mut self) {
        if self.display_names.is_empty() {
            self.last_error = Some(
                self.tr(
                    "请先打开波形文件，再导出变量名。",
                    "Open a waveform CSV before exporting names.",
                )
                .to_owned(),
            );
            return;
        }
        let section = ConfigSection::Names;
        let filter_name = section.label(self.language);
        let Some(path) = rfd::FileDialog::new()
            .add_filter(filter_name, &["json"])
            .set_file_name(section.default_file_name())
            .save_file()
        else {
            return;
        };
        match Self::serialize_config_file(section, &self.current_names_config()) {
            Ok(json) => {
                if let Err(error) = std::fs::write(&path, json) {
                    self.last_error = Some(match self.language {
                        Language::Zh => format!("导出变量名失败: {error}"),
                        Language::En => format!("Failed to export names: {error}"),
                    });
                } else {
                    self.remember_recent_config(section, &path);
                    self.last_error = Some(section.exported_message(self.language));
                }
            }
            Err(error) => {
                self.last_error = Some(match self.language {
                    Language::Zh => format!("序列化变量名失败: {error}"),
                    Language::En => format!("Failed to serialize names: {error}"),
                });
            }
        }
    }

    fn import_config(&mut self) {
        if self.display_names.is_empty() {
            self.last_error = Some(
                self.tr(
                    "请先打开波形文件，再导入变量名。",
                    "Open a waveform CSV before importing names.",
                )
                .to_owned(),
            );
            return;
        }
        let filter_name = ConfigSection::Names.label(self.language);
        let Some(path) = rfd::FileDialog::new()
            .add_filter(filter_name, &["json"])
            .pick_file()
        else {
            return;
        };
        self.import_config_from_path(path);
    }

    fn import_config_from_path(&mut self, path: PathBuf) {
        if self.display_names.is_empty() {
            self.last_error = Some(
                self.tr(
                    "请先打开波形文件，再导入变量名。",
                    "Open a waveform CSV before importing names.",
                )
                .to_owned(),
            );
            return;
        }
        match std::fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|text| Self::deserialize_config_file(ConfigSection::Names, &text))
        {
            Ok(config) => {
                self.apply_names_config(config);
                self.remember_recent_config(ConfigSection::Names, &path);
                self.last_error = Some(ConfigSection::Names.imported_message(self.language));
            }
            Err(error) => {
                self.last_error = Some(match self.language {
                    Language::Zh => format!("导入变量名失败: {error}"),
                    Language::En => format!("Failed to import names: {error}"),
                });
            }
        }
    }

    fn export_display_config(&mut self) {
        if self.display_names.is_empty() {
            self.last_error = Some(
                self.tr(
                    "请先打开波形文件，再导出显示配置。",
                    "Open a waveform file before exporting display settings.",
                )
                .to_owned(),
            );
            return;
        }
        self.export_json_config(
            ConfigSection::Display,
            &self.current_display_config(),
            self.tr("导出显示配置失败", "Failed to export display settings"),
        );
    }

    fn import_display_config(&mut self) {
        if self.display_names.is_empty() {
            self.last_error = Some(
                self.tr(
                    "请先打开波形文件，再导入显示配置。",
                    "Open a waveform file before importing display settings.",
                )
                .to_owned(),
            );
            return;
        }
        let filter_name = ConfigSection::Display.label(self.language);
        let Some(path) = rfd::FileDialog::new()
            .add_filter(filter_name, &["json"])
            .pick_file()
        else {
            return;
        };
        self.import_display_config_from_path(path);
    }

    fn import_display_config_from_path(&mut self, path: PathBuf) {
        if self.display_names.is_empty() {
            self.last_error = Some(
                self.tr(
                    "请先打开波形文件，再导入显示配置。",
                    "Open a waveform file before importing display settings.",
                )
                .to_owned(),
            );
            return;
        }
        self.import_json_config::<DisplayConfig, _>(
            ConfigSection::Display,
            &path,
            |app, config| app.apply_display_config(config),
            self.tr("导入显示配置失败", "Failed to import display settings"),
        );
    }

    fn export_shortcut_config(&mut self) {
        let shortcuts = self.shortcuts;
        let error_prefix = self.tr("导出快捷键配置失败", "Failed to export shortcut settings");
        self.export_json_config(ConfigSection::Shortcut, &shortcuts, error_prefix);
    }

    fn import_shortcut_config(&mut self) {
        let filter_name = ConfigSection::Shortcut.label(self.language);
        let Some(path) = rfd::FileDialog::new()
            .add_filter(filter_name, &["json"])
            .pick_file()
        else {
            return;
        };
        self.import_shortcut_config_from_path(path);
    }

    fn import_shortcut_config_from_path(&mut self, path: PathBuf) {
        self.import_json_config::<ShortcutConfig, _>(
            ConfigSection::Shortcut,
            &path,
            |app, config| app.shortcuts = config,
            self.tr("导入快捷键配置失败", "Failed to import shortcut settings"),
        );
    }

    fn export_dataset_config(&mut self) {
        if self.source.is_none() {
            self.last_error = Some(self.tr("请先导入数据。", "Import data first.").to_owned());
            return;
        }
        self.export_json_config(
            ConfigSection::Dataset,
            &self.current_dataset_config(),
            self.tr("导出数据组配置失败", "Failed to export dataset settings"),
        );
    }

    fn import_dataset_config(&mut self) {
        if self.source.is_none() {
            self.last_error = Some(self.tr("请先导入数据。", "Import data first.").to_owned());
            return;
        }
        let filter_name = ConfigSection::Dataset.label(self.language);
        let Some(path) = rfd::FileDialog::new()
            .add_filter(filter_name, &["json"])
            .pick_file()
        else {
            return;
        };
        self.import_dataset_config_from_path(path);
    }

    fn import_dataset_config_from_path(&mut self, path: PathBuf) {
        if self.source.is_none() {
            self.last_error = Some(self.tr("请先导入数据。", "Import data first.").to_owned());
            return;
        }
        self.import_json_config::<DatasetConfig, _>(
            ConfigSection::Dataset,
            &path,
            |app, config| app.apply_dataset_config(config),
            self.tr("导入数据组配置失败", "Failed to import dataset settings"),
        );
    }

    fn import_config_from_path_for_section(&mut self, section: ConfigSection, path: PathBuf) {
        match section {
            ConfigSection::Names => self.import_config_from_path(path),
            ConfigSection::Display => self.import_display_config_from_path(path),
            ConfigSection::Shortcut => self.import_shortcut_config_from_path(path),
            ConfigSection::Dataset => self.import_dataset_config_from_path(path),
        }
    }

    fn config_recent_menu(&mut self, ui: &mut egui::Ui, section: ConfigSection) {
        ui.separator();
        let title = if section == ConfigSection::Names {
            self.t(UiText::RecentNames)
        } else {
            self.t(UiText::RecentConfigs)
        };
        ui.strong(Self::icon_label("\u{E823}", title));
        let recent_configs = self.recent_configs(section).to_vec();
        if recent_configs.is_empty() {
            let empty = if section == ConfigSection::Names {
                self.t(UiText::NoRecentNames)
            } else {
                self.t(UiText::NoRecentConfigs)
            };
            ui.label(empty);
            return;
        }
        for path in recent_configs {
            let label = Self::recent_file_label(&path);
            let full_path = path.display().to_string();
            if path.exists() {
                if ui.button(label).on_hover_text(full_path).clicked() {
                    self.import_config_from_path_for_section(section, path);
                    ui.close_menu();
                }
            } else {
                ui.label(
                    RichText::new(format!("{} {}", label, self.t(UiText::MissingFile)))
                        .color(Color32::GRAY),
                )
                .on_hover_text(full_path);
            }
        }
        ui.separator();
        let clear = if section == ConfigSection::Names {
            self.t(UiText::ClearRecentNames)
        } else {
            self.t(UiText::ClearRecentConfigs)
        };
        if ui.button(Self::icon_label("\u{E74D}", clear)).clicked() {
            self.clear_recent_configs(section);
            ui.close_menu();
        }
    }

    fn export_json_config<T: Serialize>(
        &mut self,
        section: ConfigSection,
        config: &T,
        error_prefix: &str,
    ) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(section.label(self.language), &["json"])
            .set_file_name(section.default_file_name())
            .save_file()
        else {
            return;
        };
        match Self::serialize_config_file(section, config)
            .and_then(|json| std::fs::write(&path, json).map_err(|error| error.to_string()))
        {
            Ok(()) => {
                self.remember_recent_config(section, &path);
                self.last_error = Some(section.exported_message(self.language));
            }
            Err(error) => self.last_error = Some(format!("{error_prefix}: {error}")),
        }
    }

    fn import_json_config<T, F>(
        &mut self,
        section: ConfigSection,
        path: &Path,
        apply: F,
        error_prefix: &str,
    ) where
        T: DeserializeOwned,
        F: FnOnce(&mut Self, T),
    {
        match std::fs::read_to_string(path)
            .map_err(|error| error.to_string())
            .and_then(|text| Self::deserialize_config_file(section, &text))
        {
            Ok(config) => {
                apply(self, config);
                self.remember_recent_config(section, path);
                self.last_error = Some(section.imported_message(self.language));
            }
            Err(error) => self.last_error = Some(format!("{error_prefix}: {error}")),
        }
    }

    fn serialize_config_file<T: Serialize>(
        section: ConfigSection,
        config: &T,
    ) -> Result<String, String> {
        serde_json::to_string_pretty(&TaggedConfigRef {
            scope_config_type: section.type_name(),
            scope_config_version: 1,
            config,
        })
        .map_err(|error| error.to_string())
    }

    fn deserialize_config_file<T: DeserializeOwned>(
        section: ConfigSection,
        text: &str,
    ) -> Result<T, String> {
        let value =
            serde_json::from_str::<serde_json::Value>(text).map_err(|error| error.to_string())?;
        if value.get("scope_config_type").is_some() {
            let tagged = serde_json::from_value::<TaggedConfig<T>>(value)
                .map_err(|error| error.to_string())?;
            if tagged.scope_config_type == section.type_name() {
                Ok(tagged.config)
            } else {
                Err(section.mismatch_message())
            }
        } else if Self::legacy_config_matches_section(section, &value) {
            serde_json::from_value::<T>(value).map_err(|error| error.to_string())
        } else {
            Err(section.mismatch_message())
        }
    }

    fn legacy_config_matches_section(section: ConfigSection, value: &serde_json::Value) -> bool {
        let Some(object) = value.as_object() else {
            return false;
        };
        let has_any = |keys: &[&str]| keys.iter().any(|key| object.contains_key(*key));
        match section {
            ConfigSection::Names => has_any(&["display_names"]),
            ConfigSection::Display => has_any(&[
                "channel_colors",
                "line_widths",
                "line_patterns",
                "channel_scales",
                "channel_panes",
                "derived_visible",
                "scope_layout_rows",
                "wheel_zoom_sensitivity",
                "sample_rate_hz",
                "harmonic_base_hz",
                "export_image_format",
            ]),
            ConfigSection::Shortcut => has_any(&[
                "reset_view",
                "fit_cursors",
                "toggle_cursors",
                "select_all",
                "select_none",
                "toggle_channel_panel",
                "toggle_analysis_panel",
            ]),
            ConfigSection::Dataset => has_any(&[
                "primary_dataset_name",
                "primary_visible",
                "primary_line_pattern",
                "sync_time_axes",
                "imported",
            ]),
        }
    }

    fn selected_channels(&self) -> Vec<usize> {
        self.meta()
            .map(|meta| {
                meta.channels
                    .iter()
                    .filter_map(|channel| {
                        self.visible
                            .get(channel.index)
                            .copied()
                            .unwrap_or(false)
                            .then_some(channel.index)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn selected_imported_channels(&self, dataset_index: usize) -> Vec<usize> {
        let Some(compare_meta) = self.imported_meta(dataset_index) else {
            return Vec::new();
        };
        self.imported_datasets
            .get(dataset_index)
            .map(|dataset| {
                compare_meta
                    .channels
                    .iter()
                    .filter_map(|channel| {
                        dataset
                            .visible
                            .get(channel.index)
                            .copied()
                            .unwrap_or(false)
                            .then_some(channel.index)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn current_plot_selections(&self) -> PlotSelections {
        PlotSelections {
            primary: self.selected_channels(),
            imported: (0..self.imported_datasets.len())
                .map(|dataset_index| self.selected_imported_channels(dataset_index))
                .collect(),
            derived: self.selected_derived_channels(),
        }
    }

    fn pane_plot_selections(
        &self,
        selections: &PlotSelections,
        pane_count: usize,
    ) -> Vec<PanePlotSelections> {
        let pane_count = pane_count.max(1);
        let mut panes = vec![PanePlotSelections::default(); pane_count];
        for (out_index, channel_index) in selections.primary.iter().copied().enumerate() {
            let pane_index = self.channel_scope_pane(channel_index, pane_count);
            panes[pane_index].primary.push((out_index, channel_index));
        }
        for pane in &mut panes {
            pane.imported = vec![Vec::new(); selections.imported.len()];
        }
        for (dataset_index, channels) in selections.imported.iter().enumerate() {
            for (out_index, channel_index) in channels.iter().copied().enumerate() {
                let pane_index = self.channel_scope_pane(channel_index, pane_count);
                if let Some(dataset_channels) = panes[pane_index].imported.get_mut(dataset_index) {
                    dataset_channels.push((out_index, channel_index));
                }
            }
        }
        for (out_index, derived_index) in selections.derived.iter().copied().enumerate() {
            let pane_index = self.derived_scope_pane(derived_index, pane_count);
            panes[pane_index].derived.push((out_index, derived_index));
        }
        panes
    }

    fn plot_data_series_budget_count(&self, primary_channels: &[usize]) -> usize {
        let imported_count = (0..self.imported_datasets.len())
            .map(|dataset_index| self.selected_imported_channels(dataset_index).len())
            .sum::<usize>();
        primary_channels.len() + imported_count + self.selected_derived_channels().len()
    }

    fn plot_cache_key(
        &self,
        generation: u64,
        start: f64,
        end: f64,
        channels: &[usize],
        time_offset: f64,
        plot_pixel_width: f32,
        budget_series_count: usize,
    ) -> PlotCacheKey {
        PlotCacheKey {
            generation,
            start_bits: start.to_bits(),
            end_bits: end.to_bits(),
            channels: channels.to_vec(),
            scale_bits: channels
                .iter()
                .map(|channel| self.channel_scale(*channel).to_bits())
                .collect(),
            time_offset_bits: time_offset.to_bits(),
            plot_pixel_width: Self::plot_cache_pixel_width(plot_pixel_width) as u32,
            budget_series_count,
        }
    }

    fn dataset_channel_visible(&self, dataset_index: usize, channel_index: usize) -> bool {
        if dataset_index == 0 {
            self.visible.get(channel_index).copied().unwrap_or(false)
        } else {
            self.imported_datasets
                .get(dataset_index - 1)
                .and_then(|dataset| dataset.visible.get(channel_index))
                .copied()
                .unwrap_or(false)
        }
    }

    fn set_dataset_channel_visible(
        &mut self,
        dataset_index: usize,
        channel_index: usize,
        visible: bool,
    ) {
        let changed = if dataset_index == 0 {
            if let Some(current) = self.visible.get_mut(channel_index) {
                if *current != visible {
                    *current = visible;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else if let Some(dataset) = self.imported_datasets.get_mut(dataset_index - 1) {
            if let Some(current) = dataset.visible.get_mut(channel_index) {
                if *current != visible {
                    *current = visible;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        if !changed {
            return;
        }
        if visible {
            self.assign_channel_to_active_pane(channel_index);
        }
        if dataset_index == 0 {
            self.needs_plot_reload = true;
            self.measurement_cache = None;
        } else {
            self.needs_compare_plot_reload = true;
        }
        self.fft_results.clear();
        self.needs_fft_reload = true;
        self.fft_channel_user_selected = false;
    }

    fn fft_channel_options(&self) -> Vec<usize> {
        let dataset_index = self.selected_fft_dataset_index();
        self.analog_channel_options_for_dataset(dataset_index)
    }

    fn analog_channel_options_for_dataset(&self, dataset_index: usize) -> Vec<usize> {
        let Some(meta) = self.dataset_meta_by_index(dataset_index) else {
            return Vec::new();
        };
        let kind = self.dataset_kind_by_index(dataset_index);
        meta.channels
            .iter()
            .filter(|channel| !Self::channel_is_digital(kind, channel))
            .map(|channel| channel.index)
            .collect()
    }

    fn primary_time_sync_channel_options(&self) -> Vec<usize> {
        self.analog_channel_options_for_dataset(0)
    }

    fn default_sequence_channels_from_options(options: &[usize]) -> Option<[usize; 3]> {
        (options.len() >= 3).then(|| [options[0], options[1], options[2]])
    }

    fn preferred_sequence_channels(&self, options: &[usize]) -> Option<[usize; 3]> {
        let dataset_index = self.selected_fft_dataset_index();
        let lower_name = |channel_index: usize| {
            self.fft_channel_name(dataset_index, channel_index)
                .to_lowercase()
                .replace(['_', '.'], "")
        };
        let find_phase = |phase: &str| {
            options
                .iter()
                .copied()
                .find(|channel_index| lower_name(*channel_index).ends_with(phase))
        };
        if let (Some(a), Some(b), Some(c)) = (find_phase("ia"), find_phase("ib"), find_phase("ic"))
        {
            return Some([a, b, c]);
        }
        if let (Some(a), Some(b), Some(c)) = (find_phase("va"), find_phase("vb"), find_phase("vc"))
        {
            return Some([a, b, c]);
        }
        Self::default_sequence_channels_from_options(options)
    }

    fn normalized_channel_name(&self, dataset_index: usize, channel_index: usize) -> String {
        self.dataset_meta_by_index(dataset_index)
            .and_then(|meta| {
                meta.channels
                    .iter()
                    .find(|channel| channel.index == channel_index)
            })
            .map(|channel| channel.name.clone())
            .unwrap_or_else(|| self.fft_channel_name(dataset_index, channel_index))
            .to_ascii_lowercase()
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .collect()
    }

    fn related_three_phase_channels_from_anchor(
        &self,
        anchor: usize,
        options: &[usize],
    ) -> Option<[usize; 3]> {
        let dataset_index = self.selected_fft_dataset_index();
        self.related_three_phase_channels_from_anchor_in_dataset(dataset_index, anchor, options)
    }

    fn related_three_phase_channels_from_anchor_in_dataset(
        &self,
        dataset_index: usize,
        anchor: usize,
        options: &[usize],
    ) -> Option<[usize; 3]> {
        let anchor_name = self.normalized_channel_name(dataset_index, anchor);
        let phase = anchor_name.chars().last()?;
        if !matches!(phase, 'a' | 'b' | 'c') {
            return None;
        }
        let prefix = &anchor_name[..anchor_name.len().saturating_sub(1)];
        if prefix.len() < 2 {
            return None;
        }
        let find_phase = |phase: char| {
            let target = format!("{prefix}{phase}");
            options
                .iter()
                .copied()
                .find(|channel| self.normalized_channel_name(dataset_index, *channel) == target)
        };
        let channels = [find_phase('a')?, find_phase('b')?, find_phase('c')?];
        (!Self::triplet_has_duplicates(channels)).then_some(channels)
    }

    fn preferred_three_phase_channels(
        &self,
        options: &[usize],
        prefer_stvg: bool,
    ) -> Option<[usize; 3]> {
        let dataset_index = self.selected_fft_dataset_index();
        self.preferred_three_phase_channels_in_dataset(dataset_index, options, prefer_stvg)
    }

    fn preferred_three_phase_channels_in_dataset(
        &self,
        dataset_index: usize,
        options: &[usize],
        prefer_stvg: bool,
    ) -> Option<[usize; 3]> {
        let normalized_name =
            |channel_index: usize| self.normalized_channel_name(dataset_index, channel_index);
        let find_exact = |target: &str| {
            options
                .iter()
                .copied()
                .find(|channel_index| normalized_name(*channel_index) == target)
        };
        if prefer_stvg {
            if let (Some(a), Some(b), Some(c)) = (
                find_exact("stvg0ia"),
                find_exact("stvg0ib"),
                find_exact("stvg0ic"),
            ) {
                return Some([a, b, c]);
            }
        }
        let find_suffix = |phase: &str| {
            options
                .iter()
                .copied()
                .find(|channel_index| normalized_name(*channel_index).ends_with(phase))
        };
        if let (Some(a), Some(b), Some(c)) =
            (find_suffix("ia"), find_suffix("ib"), find_suffix("ic"))
        {
            return Some([a, b, c]);
        }
        if let (Some(a), Some(b), Some(c)) =
            (find_suffix("va"), find_suffix("vb"), find_suffix("vc"))
        {
            return Some([a, b, c]);
        }
        Self::default_sequence_channels_from_options(options)
    }

    fn preferred_time_sync_source_channels(&self, options: &[usize]) -> Option<[usize; 3]> {
        self.preferred_three_phase_channels_in_dataset(0, options, true)
    }

    fn preferred_pll_source_channels(&self, options: &[usize]) -> Option<[usize; 3]> {
        let dataset_index = self.selected_fft_dataset_index();
        let normalized_name =
            |channel_index: usize| self.normalized_channel_name(dataset_index, channel_index);
        let find_exact = |target: &str| {
            options
                .iter()
                .copied()
                .find(|channel_index| normalized_name(*channel_index) == target)
        };
        let exact_targets = match self.pll_sync_source {
            PllSyncSource::Voltage => ["stvg0ia", "stvg0ib", "stvg0ic"],
            PllSyncSource::Current => ["stig0ia", "stig0ib", "stig0ic"],
        };
        if let (Some(a), Some(b), Some(c)) = (
            find_exact(exact_targets[0]),
            find_exact(exact_targets[1]),
            find_exact(exact_targets[2]),
        ) {
            return Some([a, b, c]);
        }

        let find_suffix = |phase: &str| {
            options
                .iter()
                .copied()
                .find(|channel_index| normalized_name(*channel_index).ends_with(phase))
        };
        match self.pll_sync_source {
            PllSyncSource::Voltage => {
                if let (Some(a), Some(b), Some(c)) =
                    (find_suffix("va"), find_suffix("vb"), find_suffix("vc"))
                {
                    return Some([a, b, c]);
                }
            }
            PllSyncSource::Current => {
                if let (Some(a), Some(b), Some(c)) =
                    (find_suffix("ia"), find_suffix("ib"), find_suffix("ic"))
                {
                    return Some([a, b, c]);
                }
            }
        }
        Self::default_sequence_channels_from_options(options)
    }

    fn dataset_meta_by_index(&self, index: usize) -> Option<&DatasetMeta> {
        if index == 0 {
            self.meta()
        } else {
            self.imported_meta(index - 1)
        }
    }

    fn dataset_source_by_index(&self, index: usize) -> Option<Arc<dyn DataSource>> {
        if index == 0 {
            self.source.clone()
        } else {
            self.imported_datasets
                .get(index - 1)
                .map(|dataset| dataset.source.clone())
        }
    }

    fn dataset_path_by_index(&self, index: usize) -> Option<&Path> {
        if index == 0 {
            self.loaded_path.as_deref()
        } else {
            self.imported_datasets
                .get(index - 1)
                .map(|dataset| dataset.path.as_path())
        }
    }

    fn dataset_kind_by_index(&self, index: usize) -> Option<SourceKind> {
        if index == 0 {
            self.source_kind
        } else {
            self.imported_datasets
                .get(index - 1)
                .map(|dataset| dataset.kind)
        }
    }

    fn fft_channel_name(&self, dataset_index: usize, channel_index: usize) -> String {
        if dataset_index == 0 {
            self.channel_name(channel_index)
        } else {
            self.dataset_meta_by_index(dataset_index)
                .and_then(|meta| {
                    meta.channels
                        .iter()
                        .find(|channel| channel.index == channel_index)
                })
                .map(|channel| channel.name.clone())
                .unwrap_or_else(|| format!("CH{}", channel_index + 1))
        }
    }

    fn preferred_fft_channel(&self, fft_channels: &[usize]) -> Option<usize> {
        let dataset_index = self.selected_fft_dataset_index();
        let visible_channels = if dataset_index == 0 {
            self.selected_channels()
        } else {
            self.selected_imported_channels(dataset_index - 1)
        };
        visible_channels
            .into_iter()
            .find(|channel| fft_channels.contains(channel))
            .or_else(|| fft_channels.first().copied())
    }

    fn looks_like_digital_name(name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        [
            "logic", "sts", "status", "fault", "flag", "state", "onoff", "ready", "ok",
        ]
        .iter()
        .any(|pattern| lower.contains(pattern))
    }

    fn channel_is_digital(kind: Option<SourceKind>, channel: &ChannelMeta) -> bool {
        if channel.unit == CHANNEL_UNIT_DIGITAL {
            return true;
        }
        if channel.unit == CHANNEL_UNIT_ANALOG {
            return false;
        }
        match kind {
            Some(SourceKind::Cloud) => channel.index >= 30,
            _ => Self::looks_like_digital_name(&channel.name),
        }
    }

    fn dataset_time_offset(&self, dataset_index: usize) -> f64 {
        if !self.sync_time_axes || dataset_index == 0 {
            return 0.0;
        }
        self.imported_datasets
            .get(dataset_index - 1)
            .map(|dataset| dataset.time_offset)
            .filter(|offset| offset.is_finite())
            .unwrap_or(0.0)
    }

    fn sync_channel_key(name: &str) -> String {
        name.chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .map(|ch| ch.to_ascii_lowercase())
            .collect()
    }

    fn find_matching_sync_channel(meta: &DatasetMeta, target_name: &str) -> Option<usize> {
        let target = Self::sync_channel_key(target_name);
        if target.is_empty() {
            return None;
        }
        meta.channels
            .iter()
            .find(|channel| Self::sync_channel_key(&channel.name) == target)
            .map(|channel| channel.index)
            .or_else(|| {
                (target.len() >= 4)
                    .then(|| {
                        meta.channels
                            .iter()
                            .find(|channel| {
                                let key = Self::sync_channel_key(&channel.name);
                                key.contains(&target) || target.contains(&key)
                            })
                            .map(|channel| channel.index)
                    })
                    .flatten()
            })
    }

    fn sync_channel_pairs(
        primary: &DatasetMeta,
        other: &DatasetMeta,
        primary_channels: [usize; 3],
    ) -> Vec<(usize, usize)> {
        primary_channels
            .into_iter()
            .filter_map(|primary_channel| {
                let primary_name = primary
                    .channels
                    .iter()
                    .find(|channel| channel.index == primary_channel)
                    .map(|channel| channel.name.as_str())?;
                Some((
                    primary_channel,
                    Self::find_matching_sync_channel(other, primary_name)?,
                ))
            })
            .collect()
    }

    fn phase_at_frequency(times: &[f64], samples: &[f32], frequency_hz: f64) -> Option<(f64, f64)> {
        if times.len() < 16 || samples.len() < 16 || frequency_hz <= 0.0 {
            return None;
        }
        let count = times.len().min(samples.len());
        let finite_values = samples
            .iter()
            .take(count)
            .filter(|sample| sample.is_finite())
            .map(|sample| *sample as f64)
            .collect::<Vec<_>>();
        if finite_values.len() < 16 {
            return None;
        }
        let mean = finite_values.iter().sum::<f64>() / finite_values.len() as f64;
        let omega = std::f64::consts::TAU * frequency_hz;
        let mut re = 0.0;
        let mut im = 0.0;
        let mut used = 0usize;
        for (time, sample) in times.iter().zip(samples.iter()).take(count) {
            if !time.is_finite() || !sample.is_finite() {
                continue;
            }
            let centered = *sample as f64 - mean;
            let angle = omega * *time;
            re += centered * angle.cos();
            im -= centered * angle.sin();
            used += 1;
        }
        if used < 16 {
            return None;
        }
        let amplitude = (re * re + im * im).sqrt() / used as f64;
        if amplitude <= f64::EPSILON {
            return None;
        }
        Some((im.atan2(re), amplitude))
    }

    fn phase_sync_offset_for(
        primary: &dyn DataSource,
        other: &dyn DataSource,
        frequency_hz: f64,
        primary_channels: [usize; 3],
    ) -> DataResult<Option<f64>> {
        let primary_meta = primary.metadata();
        let other_meta = other.metadata();
        let pairs = Self::sync_channel_pairs(primary_meta, other_meta, primary_channels);
        if pairs.is_empty() {
            return Ok(None);
        }

        let min_cycles = 3.0 / frequency_hz.max(0.001);
        let span = primary_meta.duration().min(other_meta.duration()).max(0.0);
        if span < min_cycles {
            return Ok(None);
        }
        let primary_start = primary_meta.start_time;
        let other_start = other_meta.start_time;
        let primary_end = (primary_start + span).min(primary_meta.end_time);
        let other_end = (other_start + span).min(other_meta.end_time);
        if primary_end <= primary_start || other_end <= other_start {
            return Ok(None);
        }

        let mut sum_re = 0.0;
        let mut sum_im = 0.0;
        let mut used = 0usize;
        for (primary_channel, other_channel) in pairs {
            let primary_block = primary.read_range(
                primary_start,
                primary_end,
                &[primary_channel],
                MAX_TIME_SYNC_POINTS,
            )?;
            let other_block = other.read_range(
                other_start,
                other_end,
                &[other_channel],
                MAX_TIME_SYNC_POINTS,
            )?;
            let Some(primary_samples) = primary_block.channels.first() else {
                continue;
            };
            let Some(other_samples) = other_block.channels.first() else {
                continue;
            };
            let Some((primary_phase, primary_amp)) =
                Self::phase_at_frequency(&primary_block.times, primary_samples, frequency_hz)
            else {
                continue;
            };
            let Some((other_phase, other_amp)) =
                Self::phase_at_frequency(&other_block.times, other_samples, frequency_hz)
            else {
                continue;
            };
            let diff = other_phase - primary_phase;
            let weight = primary_amp.min(other_amp).max(1.0);
            sum_re += diff.cos() * weight;
            sum_im += diff.sin() * weight;
            used += 1;
        }

        if used == 0 || (sum_re.abs() <= f64::EPSILON && sum_im.abs() <= f64::EPSILON) {
            return Ok(None);
        }
        let phase_offset = sum_im.atan2(sum_re);
        Ok(Some(phase_offset / (std::f64::consts::TAU * frequency_hz)))
    }

    fn sync_time_axes_by_phase(&mut self) {
        let Some(primary) = self.source.clone() else {
            self.time_sync_status = self.tr("请先导入数据。", "Import data first.").to_owned();
            return;
        };
        if self.imported_datasets.is_empty() {
            self.time_sync_status = self
                .tr(
                    "没有可同步的附加数据组。",
                    "No extra dataset groups to sync.",
                )
                .to_owned();
            return;
        }

        let primary_channels = self.time_sync_source_channels;
        let frequency_hz = self.harmonic_base_hz.max(0.001);
        let mut synced = 0usize;
        let mut failed = 0usize;
        for dataset in &mut self.imported_datasets {
            match Self::phase_sync_offset_for(
                primary.as_ref(),
                dataset.source.as_ref(),
                frequency_hz,
                primary_channels,
            ) {
                Ok(Some(offset)) if offset.is_finite() => {
                    dataset.time_offset = offset;
                    dataset.plot_cache = SampleBlock::default();
                    dataset.plot_summary = None;
                    dataset.prepared_plot_cache = PreparedPlotSeries::default();
                    dataset.prepared_plot_summary = None;
                    synced += 1;
                }
                Ok(_) => {
                    dataset.time_offset = 0.0;
                    failed += 1;
                }
                Err(error) => {
                    dataset.time_offset = 0.0;
                    failed += 1;
                    self.last_error = Some(error.to_string());
                }
            }
        }
        self.sync_time_axes = synced > 0;
        self.needs_compare_plot_reload = true;
        self.fft_results.clear();
        self.needs_fft_reload = true;
        self.time_sync_status = match self.language {
            Language::Zh => format!("已同步 {synced} 个数据组，失败 {failed} 个。"),
            Language::En => format!("Synced {synced} group(s), failed {failed}."),
        };
    }

    fn clear_time_axis_sync(&mut self) {
        for dataset in &mut self.imported_datasets {
            dataset.time_offset = 0.0;
            dataset.plot_cache = SampleBlock::default();
            dataset.plot_summary = None;
            dataset.prepared_plot_cache = PreparedPlotSeries::default();
            dataset.prepared_plot_summary = None;
        }
        self.sync_time_axes = false;
        self.time_sync_status.clear();
        self.needs_compare_plot_reload = true;
        self.fft_results.clear();
        self.needs_fft_reload = true;
    }

    fn channel_name(&self, index: usize) -> String {
        self.display_names
            .get(index)
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .or_else(|| {
                self.meta()
                    .and_then(|meta| meta.channels.iter().find(|channel| channel.index == index))
                    .map(|channel| channel.name.clone())
            })
            .unwrap_or_else(|| format!("CH{}", index + 1))
    }

    fn compact_label(label: &str, max_chars: usize) -> String {
        let mut chars = label.chars();
        let mut output = chars.by_ref().take(max_chars).collect::<String>();
        if chars.next().is_some() {
            output.push_str("...");
        }
        output
    }

    fn channel_panel_display_name(name: &str, is_digital: bool, available_width: f32) -> String {
        if !is_digital || Self::estimated_label_width(name) <= available_width {
            return name.to_owned();
        }
        let Some((_, suffix)) = name.rsplit_once('.') else {
            return name.to_owned();
        };
        if suffix.trim().is_empty() {
            name.to_owned()
        } else {
            suffix.to_owned()
        }
    }

    fn channel_panel_display_text(
        name: &str,
        is_digital: bool,
        available_width: f32,
    ) -> Option<String> {
        if available_width < CHANNEL_NAME_HIDE_WIDTH {
            None
        } else {
            Some(Self::channel_panel_display_name(
                name,
                is_digital,
                available_width,
            ))
        }
    }

    fn channel_filter_width(available_width: f32, clear_visible: bool) -> (f32, bool) {
        let clear_width = if clear_visible && available_width >= 88.0 {
            46.0
        } else {
            0.0
        };
        ((available_width - clear_width).max(24.0), clear_width > 0.0)
    }

    fn sidebar_header_label(full_label: &str, short_label: &str, available_width: f32) -> String {
        if available_width < 120.0 {
            short_label.to_owned()
        } else {
            let max_chars = (available_width / CHANNEL_NAME_AVERAGE_CHAR_WIDTH)
                .floor()
                .max(8.0) as usize;
            Self::compact_label(full_label, max_chars)
        }
    }

    fn estimated_label_width(label: &str) -> f32 {
        label.chars().count() as f32 * CHANNEL_NAME_AVERAGE_CHAR_WIDTH
    }

    fn analysis_combo_width(available_width: f32) -> f32 {
        available_width.clamp(0.0, 420.0)
    }

    fn three_phase_selector_layout(available_width: f32) -> ThreePhaseSelectorLayout {
        if available_width < THREE_PHASE_SELECTOR_VERTICAL_WIDTH {
            ThreePhaseSelectorLayout::Vertical
        } else {
            ThreePhaseSelectorLayout::Horizontal
        }
    }

    fn analysis_input_shows_three_phase_selectors(channel_count: usize) -> bool {
        channel_count >= 3
    }

    fn analysis_input_three_phase_row_layout(
        channel_count: usize,
        available_width: f32,
    ) -> Option<ThreePhaseSelectorLayout> {
        Self::analysis_input_shows_three_phase_selectors(channel_count)
            .then(|| Self::three_phase_selector_layout(available_width))
    }

    fn dataset_channel_name(&self, dataset_index: usize, channel: &ChannelMeta) -> String {
        if dataset_index > 0 && self.dataset_kind_by_index(dataset_index) == Some(SourceKind::Dat) {
            if channel.name.trim().is_empty() {
                format!("CH{}", channel.index + 1)
            } else {
                channel.name.clone()
            }
        } else {
            self.channel_name(channel.index)
        }
    }

    fn draw_points_per_channel_with_mode(
        series_count: usize,
        software_performance_mode: bool,
    ) -> usize {
        if series_count == 0 {
            return 0;
        }
        let (total_budget, min_per_channel, max_per_channel) = if software_performance_mode {
            (
                SOFTWARE_MAX_TOTAL_DRAW_POINTS,
                SOFTWARE_MIN_DRAW_POINTS_PER_CHANNEL,
                SOFTWARE_MAX_DRAW_POINTS_PER_CHANNEL,
            )
        } else {
            (
                MAX_TOTAL_DRAW_POINTS,
                MIN_DRAW_POINTS_PER_CHANNEL,
                MAX_DRAW_POINTS_PER_CHANNEL,
            )
        };
        (total_budget / series_count).clamp(min_per_channel, max_per_channel)
    }

    #[cfg(test)]
    fn summary_bins_for_channels(channel_count: usize, plot_pixel_width: f32) -> usize {
        Self::summary_bins_for_channels_with_mode(channel_count, plot_pixel_width, false)
    }

    fn summary_bins_for_channels_with_mode(
        channel_count: usize,
        plot_pixel_width: f32,
        software_performance_mode: bool,
    ) -> usize {
        // Each summary bin is drawn as min+max. Use roughly one envelope bin per horizontal
        // screen pixel so dense ranges preserve spikes without drawing every sample. The envelope
        // is one plot item per channel, but still contains two line paths.
        let min_bins = if software_performance_mode { 64 } else { 96 };
        let envelope_item_count = channel_count.max(1);
        let max_budget_bins = (Self::draw_points_per_channel_with_mode(
            envelope_item_count,
            software_performance_mode,
        ) / 2)
            .max(min_bins);
        let pixel_bins = Self::plot_cache_pixel_width(plot_pixel_width)
            .clamp(min_bins as f32, max_budget_bins as f32) as usize;
        pixel_bins.max(1)
    }

    fn renderer_value_uses_software_performance_mode(value: Option<&str>) -> bool {
        let Some(value) = value else {
            return false;
        };
        let value = value.trim().to_ascii_lowercase();
        matches!(
            value.as_str(),
            "glow-software"
                | "opengl-software"
                | "software"
                | "cloud"
                | "virtual"
                | "wgpu-software"
                | "dx12-software"
                | "warp"
                | "mesa"
                | "llvmpipe"
                | "mesa-software"
        ) || value.contains("software")
    }

    fn software_rendering_performance_mode(&self) -> bool {
        let renderer = env::var("SCOPE_RENDERER").ok();
        Self::renderer_value_uses_software_performance_mode(renderer.as_deref())
            || env::var("LIBGL_ALWAYS_SOFTWARE")
                .map(|value| value == "1")
                .unwrap_or(false)
            || env::var("GALLIUM_DRIVER")
                .map(|value| {
                    let value = value.to_ascii_lowercase();
                    value.contains("llvmpipe") || value.contains("softpipe")
                })
                .unwrap_or(false)
            || env::var("MESA_LOADER_DRIVER_OVERRIDE")
                .map(|value| value.to_ascii_lowercase().contains("llvmpipe"))
                .unwrap_or(false)
    }

    fn plot_cache_pixel_width(plot_pixel_width: f32) -> f32 {
        let width = plot_pixel_width.max(80.0).ceil();
        (width / PLOT_CACHE_WIDTH_BUCKET).ceil() * PLOT_CACHE_WIDTH_BUCKET
    }

    fn plot_should_use_summary(estimated_samples: usize, channel_count: usize) -> bool {
        estimated_samples.saturating_mul(channel_count.max(1)) > MAX_RAW_PLOT_SOURCE_SAMPLES
    }

    fn lightweight_plot_points(points: &Arc<[PlotPoint]>) -> Arc<[PlotPoint]> {
        if points.len() <= LAYOUT_RESIZE_DRAW_POINTS_PER_CHANNEL {
            return Arc::clone(points);
        }

        let stride = points.len().div_ceil(LAYOUT_RESIZE_DRAW_POINTS_PER_CHANNEL);
        let mut reduced =
            Vec::with_capacity((points.len() / stride).saturating_add(2).min(points.len()));
        for index in (0..points.len()).step_by(stride) {
            reduced.push(points[index]);
        }
        if let Some(last) = points.last() {
            if reduced
                .last()
                .is_none_or(|point| point.x != last.x || point.y != last.y)
            {
                reduced.push(*last);
            }
        }
        Arc::from(reduced)
    }

    fn frame_plot_points(
        points: &Arc<[PlotPoint]>,
        lightweight_points: Option<&Arc<[PlotPoint]>>,
        lightweight: bool,
    ) -> Arc<[PlotPoint]> {
        if lightweight {
            lightweight_points
                .map(PreparedPlotSeries::shared_points)
                .unwrap_or_else(|| PreparedPlotSeries::shared_points(points))
        } else {
            PreparedPlotSeries::shared_points(points)
        }
    }

    fn plot_summary_envelope(plot_ui: &mut PlotUi, draw: SummaryEnvelopeDraw<'_>) {
        if let Some(low_points) = draw.low_points {
            let mut envelope = Envelope::new(
                Self::frame_plot_points(
                    draw.high_points,
                    draw.high_lightweight_points,
                    draw.lightweight,
                ),
                Self::frame_plot_points(low_points, draw.low_lightweight_points, draw.lightweight),
            )
            .color(draw.line_color)
            .style(draw.line_style)
            .width(draw.line_width);
            if let Some(legend_name) = draw.legend_name {
                envelope = envelope.name(legend_name);
            }
            plot_ui.envelope(envelope);
        } else {
            let mut line = Line::new(Self::frame_plot_points(
                draw.high_points,
                draw.high_lightweight_points,
                draw.lightweight,
            ))
            .color(draw.line_color)
            .style(draw.line_style)
            .width(draw.line_width);
            if let Some(legend_name) = draw.legend_name {
                line = line.name(legend_name);
            }
            plot_ui.line(line);
        }
    }

    fn scope_grid_color(ui: &egui::Ui) -> Color32 {
        if ui.visuals().dark_mode {
            Color32::from_rgba_unmultiplied(245, 248, 255, 34)
        } else {
            Color32::from_rgba_unmultiplied(72, 92, 120, 46)
        }
    }

    fn nice_scope_grid_step(span: f64, target_lines: usize) -> Option<f64> {
        if !span.is_finite() || span <= 0.0 {
            return None;
        }
        let raw = span / target_lines.max(1) as f64;
        let base = 10_f64.powf(raw.log10().floor());
        for factor in [1.0, 2.0, 5.0, 10.0] {
            let step = base * factor;
            if step >= raw && step.is_finite() && step > 0.0 {
                return Some(step);
            }
        }
        Some(base * 10.0)
    }

    fn scope_grid_values(min: f64, max: f64, target_lines: usize) -> Vec<f64> {
        let Some(step) = Self::nice_scope_grid_step(max - min, target_lines) else {
            return Vec::new();
        };
        let mut value = (min / step).ceil() * step;
        let mut values = Vec::new();
        while value <= max && values.len() < SCOPE_GRID_MAX_LINES_PER_AXIS {
            if value.is_finite() {
                values.push(value);
            }
            value += step;
        }
        values
    }

    fn draw_scope_dashed_grid(
        plot_ui: &mut PlotUi,
        x_min: f64,
        x_max: f64,
        y_min: f64,
        y_max: f64,
        color: Color32,
    ) {
        let style = LineStyle::Dashed { length: 5.0 };
        for x in Self::scope_grid_values(x_min, x_max, SCOPE_GRID_TARGET_X_LINES) {
            plot_ui.vline(VLine::new(x).color(color).width(1.0).style(style));
        }
        for y in Self::scope_grid_values(y_min, y_max, SCOPE_GRID_TARGET_Y_LINES) {
            plot_ui.hline(HLine::new(y).color(color).width(1.0).style(style));
        }
    }

    fn prepare_sample_series(
        &self,
        block: &SampleBlock,
        channels: &[usize],
        time_offset: f64,
    ) -> PreparedPlotSeries {
        let count = block.channels.len().min(channels.len());
        let mut points = Vec::with_capacity(count);
        let mut lightweight_points = Vec::with_capacity(count);
        let mut bounds = Vec::with_capacity(count);
        for (out_index, channel_index) in channels.iter().take(count).enumerate() {
            let Some(values) = block.channels.get(out_index) else {
                break;
            };
            let len = block.times.len().min(values.len());
            let mut series = Vec::with_capacity(len);
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            for i in 0..len {
                let x = block.times[i] + time_offset;
                let y = self.scaled_value(*channel_index, values[i]);
                if !x.is_finite() || !y.is_finite() {
                    continue;
                }
                min = min.min(y);
                max = max.max(y);
                series.push(PlotPoint::new(x, y));
            }
            let bounds_for_series = (min.is_finite() && max.is_finite()).then_some((min, max));
            let series: Arc<[PlotPoint]> = Arc::from(series);
            lightweight_points.push(Self::lightweight_plot_points(&series));
            points.push(series);
            bounds.push(bounds_for_series);
        }
        PreparedPlotSeries {
            points,
            lightweight_points,
            envelope_low_points: Vec::new(),
            envelope_low_lightweight_points: Vec::new(),
            bounds,
        }
    }

    fn prepare_summary_series(
        &self,
        summary: &RangeSummary,
        channels: &[usize],
        time_offset: f64,
    ) -> PreparedPlotSeries {
        let count = summary.min.len().min(summary.max.len()).min(channels.len());
        let mut points = Vec::with_capacity(count);
        let mut lightweight_points = Vec::with_capacity(count);
        let mut envelope_low_points = Vec::with_capacity(count);
        let mut envelope_low_lightweight_points = Vec::with_capacity(count);
        let mut bounds = Vec::with_capacity(count);
        let bin_count = summary.bin_start.len().min(summary.bin_end.len());
        for (out_index, channel_index) in channels.iter().take(count).enumerate() {
            let envelope = Self::summary_envelope_series(
                summary,
                out_index,
                time_offset,
                self.channel_scale(*channel_index) as f64,
                bin_count,
            );
            lightweight_points.push(Self::lightweight_plot_points(&envelope.high));
            envelope_low_lightweight_points.push(Self::lightweight_plot_points(&envelope.low));
            points.push(envelope.high);
            envelope_low_points.push(envelope.low);
            bounds.push(envelope.bounds);
        }
        PreparedPlotSeries {
            points,
            lightweight_points,
            envelope_low_points,
            envelope_low_lightweight_points,
            bounds,
        }
    }

    fn summary_envelope_series(
        summary: &RangeSummary,
        out_index: usize,
        time_offset: f64,
        scale: f64,
        bin_count: usize,
    ) -> SummaryEnvelopeSeries {
        let Some(mins) = summary.min.get(out_index) else {
            return SummaryEnvelopeSeries {
                high: Arc::<[PlotPoint]>::from(Vec::<PlotPoint>::new()),
                low: Arc::<[PlotPoint]>::from(Vec::<PlotPoint>::new()),
                bounds: None,
            };
        };
        let Some(maxes) = summary.max.get(out_index) else {
            return SummaryEnvelopeSeries {
                high: Arc::<[PlotPoint]>::from(Vec::<PlotPoint>::new()),
                low: Arc::<[PlotPoint]>::from(Vec::<PlotPoint>::new()),
                bounds: None,
            };
        };
        let len = bin_count.min(mins.len()).min(maxes.len());
        let mut high_series = Vec::with_capacity(len);
        let mut low_series = Vec::with_capacity(len);
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for ((&bin_start, &bin_end), (&raw_min, &raw_max)) in summary
            .bin_start
            .iter()
            .zip(&summary.bin_end)
            .zip(mins.iter().zip(maxes))
            .take(len)
        {
            let x0 = bin_start + time_offset;
            let x1 = bin_end + time_offset;
            let (scaled_min, scaled_max) = Self::scaled_min_max_with_scale(raw_min, raw_max, scale);
            if !x0.is_finite()
                || !x1.is_finite()
                || x1 < x0
                || !scaled_min.is_finite()
                || !scaled_max.is_finite()
            {
                continue;
            }
            min = min.min(scaled_min);
            max = max.max(scaled_max);
            let x = (x0 + x1) * 0.5;
            if x.is_finite() {
                high_series.push(PlotPoint::new(x, scaled_max));
                low_series.push(PlotPoint::new(x, scaled_min));
            }
        }
        let bounds_for_series = (min.is_finite() && max.is_finite()).then_some((min, max));
        SummaryEnvelopeSeries {
            high: Arc::from(high_series),
            low: Arc::from(low_series),
            bounds: bounds_for_series,
        }
    }

    fn prepare_derived_sample_series(
        &self,
        block: &SampleBlock,
        channels: &[usize],
        time_offset: f64,
    ) -> PreparedPlotSeries {
        let count = block.channels.len().min(channels.len());
        let mut points = Vec::with_capacity(count);
        let mut lightweight_points = Vec::with_capacity(count);
        let mut bounds = Vec::with_capacity(count);
        for derived_index in channels.iter().take(count) {
            let Some(values) = block.channels.get(*derived_index) else {
                break;
            };
            let len = block.times.len().min(values.len());
            let mut series = Vec::with_capacity(len);
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            for i in 0..len {
                let x = block.times[i] + time_offset;
                let y = values[i] as f64;
                if !x.is_finite() || !y.is_finite() {
                    continue;
                }
                min = min.min(y);
                max = max.max(y);
                series.push(PlotPoint::new(x, y));
            }
            let bounds_for_series = (min.is_finite() && max.is_finite()).then_some((min, max));
            let series: Arc<[PlotPoint]> = Arc::from(series);
            lightweight_points.push(Self::lightweight_plot_points(&series));
            points.push(series);
            bounds.push(bounds_for_series);
        }
        PreparedPlotSeries {
            points,
            lightweight_points,
            envelope_low_points: Vec::new(),
            envelope_low_lightweight_points: Vec::new(),
            bounds,
        }
    }

    #[allow(dead_code)]
    fn load_plot_data(
        source: Arc<dyn DataSource>,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        budget_series_count: usize,
        plot_pixel_width: f32,
    ) -> Result<Option<PlotJobData>, String> {
        Self::load_plot_data_with_cancel(
            source,
            start_time,
            end_time,
            channels,
            PlotLoadOptions {
                budget_series_count,
                plot_pixel_width,
                software_performance_mode: false,
                cancel: None,
            },
        )
    }

    fn load_plot_data_with_cancel(
        source: Arc<dyn DataSource>,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        options: PlotLoadOptions<'_>,
    ) -> Result<Option<PlotJobData>, String> {
        if channels.is_empty() || end_time <= start_time {
            return Ok(None);
        }
        if let Some(cancel) = options.cancel {
            cancel.check().map_err(|error| error.to_string())?;
        }
        let budget_series_count = options.budget_series_count.max(channels.len()).max(1);
        let max_points = Self::draw_points_per_channel_with_mode(
            budget_series_count,
            options.software_performance_mode,
        );
        let summary_bins = Self::summary_bins_for_channels_with_mode(
            budget_series_count,
            options.plot_pixel_width,
            options.software_performance_mode,
        );
        let estimated_points =
            ((end_time - start_time) * source.metadata().nominal_sample_rate_hz).max(0.0) as usize;
        if Self::plot_should_use_summary(estimated_points, channels.len()) {
            let summary = if let Some(cancel) = options.cancel {
                source.summarize_range_cancellable(
                    start_time,
                    end_time,
                    channels,
                    summary_bins,
                    cancel,
                )
            } else {
                source.summarize_range(start_time, end_time, channels, summary_bins)
            }
            .map_err(|error| error.to_string())?;
            Ok(Some(PlotJobData::Summary(summary)))
        } else {
            let block = if let Some(cancel) = options.cancel {
                source.read_range_cancellable(start_time, end_time, channels, max_points, cancel)
            } else {
                source.read_range(start_time, end_time, channels, max_points)
            }
            .map_err(|error| error.to_string())?;
            Ok(Some(PlotJobData::Samples(block)))
        }
    }

    #[cfg(test)]
    pub(crate) fn perf_load_plot_data(
        source: Arc<dyn DataSource>,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        budget_series_count: usize,
    ) -> Result<bool, String> {
        Self::load_plot_data(
            source,
            start_time,
            end_time,
            channels,
            budget_series_count,
            DEFAULT_PLOT_PIXEL_WIDTH,
        )
        .map(|data| data.is_some())
    }

    fn apply_plot_job_data(&mut self, data: Option<PlotJobData>, key: Option<PlotCacheKey>) {
        let selected = Self::plot_job_channels_for_prepare(key.as_ref(), &self.selected_channels());
        match data {
            Some(PlotJobData::Samples(block)) => {
                self.prepared_plot_cache = self.prepare_sample_series(&block, &selected, 0.0);
                self.plot_cache = block;
                self.plot_summary = None;
                self.prepared_plot_summary = None;
                self.plot_cache_key = key;
            }
            Some(PlotJobData::Summary(summary)) => {
                self.prepared_plot_summary =
                    Some(self.prepare_summary_series(&summary, &selected, 0.0));
                self.plot_cache = SampleBlock::default();
                self.prepared_plot_cache = PreparedPlotSeries::default();
                self.plot_summary = Some(summary);
                self.plot_cache_key = key;
            }
            None => {
                self.plot_cache = SampleBlock::default();
                self.plot_summary = None;
                self.prepared_plot_cache = PreparedPlotSeries::default();
                self.prepared_plot_summary = None;
                self.plot_cache_key = None;
            }
        }
        self.clear_pane_prepared_plot_cache();
    }

    fn plot_job_channels_for_prepare(
        key: Option<&PlotCacheKey>,
        current_channels: &[usize],
    ) -> Vec<usize> {
        key.map(|key| key.channels.clone())
            .unwrap_or_else(|| current_channels.to_vec())
    }

    fn triplet_has_duplicates(channels: [usize; 3]) -> bool {
        channels[0] == channels[1] || channels[0] == channels[2] || channels[1] == channels[2]
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    fn load_derived_data(
        source: Arc<dyn DataSource>,
        start_time: f64,
        end_time: f64,
        pll_channels: [usize; 3],
        dq_channels: [usize; 3],
        pll_scales: [f32; 3],
        dq_scales: [f32; 3],
        sample_rate_hz: f64,
        harmonic_base_hz: f64,
        skip_digital_by_samples: bool,
        max_points: usize,
    ) -> Result<SampleBlock, String> {
        Self::load_derived_data_with_cancel(
            source,
            start_time,
            end_time,
            &[0, 1, 2, 3],
            &[],
            pll_channels,
            dq_channels,
            pll_scales,
            dq_scales,
            &[],
            sample_rate_hz,
            harmonic_base_hz,
            skip_digital_by_samples,
            max_points,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn load_derived_data_with_cancel(
        source: Arc<dyn DataSource>,
        start_time: f64,
        end_time: f64,
        selected_outputs: &[usize],
        repid_curves: &[crate::repid_derived::RepidDerivedCurve],
        pll_channels: [usize; 3],
        dq_channels: [usize; 3],
        pll_scales: [f32; 3],
        dq_scales: [f32; 3],
        channel_scales: &[f32],
        sample_rate_hz: f64,
        harmonic_base_hz: f64,
        skip_digital_by_samples: bool,
        max_points: usize,
        cancel: Option<&DataCancelToken>,
    ) -> Result<SampleBlock, String> {
        if end_time <= start_time {
            return Ok(SampleBlock::default());
        }
        if let Some(cancel) = cancel {
            cancel.check().map_err(|error| error.to_string())?;
        }
        let output_count = Self::derived_output_count_for_repid_curves(repid_curves.len());
        let mut output = SampleBlock {
            times: Vec::new(),
            channels: vec![Vec::new(); output_count],
        };

        let selected_builtin = selected_outputs
            .iter()
            .any(|index| *index < DERIVED_CHANNEL_COUNT);
        if selected_builtin {
            if Self::triplet_has_duplicates(pll_channels)
                || Self::triplet_has_duplicates(dq_channels)
            {
                return Err("PLL/dq0 inputs must use distinct A/B/C channels.".to_owned());
            }
            let channels = [
                pll_channels[0],
                pll_channels[1],
                pll_channels[2],
                dq_channels[0],
                dq_channels[1],
                dq_channels[2],
            ];
            let block = if let Some(cancel) = cancel {
                source.read_range_cancellable(start_time, end_time, &channels, max_points, cancel)
            } else {
                source.read_range(start_time, end_time, &channels, max_points)
            }
            .map_err(|error| error.to_string())?;
            if let Some(cancel) = cancel {
                cancel.check().map_err(|error| error.to_string())?;
            }
            if block.channels.len() < 6 {
                return Err("PLL/dq0 needs six source channel reads.".to_owned());
            }
            let scale_samples = |values: &[f32], scale: f32| {
                if (scale - DEFAULT_CHANNEL_SCALE).abs() <= f32::EPSILON {
                    values.to_vec()
                } else {
                    values.iter().map(|value| *value * scale).collect()
                }
            };
            let pll_a = scale_samples(&block.channels[0], pll_scales[0]);
            let pll_b = scale_samples(&block.channels[1], pll_scales[1]);
            let pll_c = scale_samples(&block.channels[2], pll_scales[2]);
            let dq_a = scale_samples(&block.channels[3], dq_scales[0]);
            let dq_b = scale_samples(&block.channels[4], dq_scales[1]);
            let dq_c = scale_samples(&block.channels[5], dq_scales[2]);

            if skip_digital_by_samples
                && [&pll_a, &pll_b, &pll_c, &dq_a, &dq_b, &dq_c]
                    .iter()
                    .any(|samples| Self::samples_look_digital(samples))
            {
                return Err("PLL/dq0 only supports analog channels.".to_owned());
            }

            if let Some(cancel) = cancel {
                cancel.check().map_err(|error| error.to_string())?;
            }
            let theta = transforms::run_srf_pll(
                &pll_a,
                &pll_b,
                &pll_c,
                sample_rate_hz.max(1.0),
                harmonic_base_hz.max(0.001),
            )?;
            let dq0 = transforms::abc_to_dq0(&dq_a, &dq_b, &dq_c, &theta)?;
            if let Some(cancel) = cancel {
                cancel.check().map_err(|error| error.to_string())?;
            }
            let theta_deg = transforms::radians_to_wrapped_degrees(&theta);
            let len = block
                .times
                .len()
                .min(theta_deg.len())
                .min(dq0.d.len())
                .min(dq0.q.len())
                .min(dq0.zero.len());
            output.times = block.times.into_iter().take(len).collect();
            output.channels[0] = theta_deg.into_iter().take(len).collect();
            output.channels[1] = dq0.d.into_iter().take(len).collect();
            output.channels[2] = dq0.q.into_iter().take(len).collect();
            output.channels[3] = dq0.zero.into_iter().take(len).collect();
        }

        for derived_index in selected_outputs
            .iter()
            .copied()
            .filter(|index| *index >= DERIVED_CHANNEL_COUNT)
        {
            let Some(curve) = repid_curves.get(derived_index - DERIVED_CHANNEL_COUNT) else {
                continue;
            };
            let expression = curve.expression().map_err(|error| error.to_string())?;
            let source_channels = expression.input_channels();
            expression
                .validate_channel_count(source.metadata().channels.len())
                .map_err(|error| {
                    format!("Repid curve '{}' cannot be calculated: {error}", curve.name)
                })?;
            let block = if let Some(cancel) = cancel {
                source.read_range_cancellable(
                    start_time,
                    end_time,
                    source_channels,
                    max_points,
                    cancel,
                )
            } else {
                source.read_range(start_time, end_time, source_channels, max_points)
            }
            .map_err(|error| error.to_string())?;
            if block.channels.len() < source_channels.len() {
                return Err(format!(
                    "Repid curve '{}' needs source channels.",
                    curve.name
                ));
            }
            let scaled = block
                .channels
                .iter()
                .zip(source_channels.iter().copied())
                .map(|(values, channel_index)| {
                    let scale = channel_scales
                        .get(channel_index)
                        .copied()
                        .unwrap_or(DEFAULT_CHANNEL_SCALE);
                    if (scale - DEFAULT_CHANNEL_SCALE).abs() <= f32::EPSILON {
                        values.clone()
                    } else {
                        values.iter().map(|value| *value * scale).collect()
                    }
                })
                .collect::<Vec<Vec<f32>>>();
            let slices = scaled
                .iter()
                .map(|values| values.as_slice())
                .collect::<Vec<_>>();
            let mut values = expression.evaluate(&slices).map_err(|error| {
                format!("Repid curve '{}' cannot be calculated: {error}", curve.name)
            })?;
            if (curve.k - 1.0).abs() > f32::EPSILON || curve.b.abs() > f32::EPSILON {
                for value in &mut values {
                    *value = *value * curve.k + curve.b;
                }
            }
            if output.times.is_empty() {
                output.times = block.times;
            }
            if let Some(slot) = output.channels.get_mut(derived_index) {
                *slot = values;
            }
        }

        Ok(SampleBlock {
            times: output.times,
            channels: output.channels,
        })
    }

    fn poll_plot_worker(&mut self) {
        let Some(joined) = Self::take_finished_job(&mut self.plot_worker, "Plot worker panicked.")
        else {
            return;
        };
        self.plot_cancel = None;
        let Ok(result) = joined else {
            if !self.needs_plot_reload {
                self.last_error = Some("Plot worker panicked.".to_owned());
            }
            return;
        };
        if !self.result_matches_generation(result.generation, self.needs_plot_reload) {
            return;
        }
        match result.result {
            Ok(data) => self.apply_plot_job_data(data, Some(result.key)),
            Err(error) => self.last_error = Some(error),
        }
    }

    fn reload_plot_cache(&mut self, plot_pixel_width: f32) {
        self.poll_plot_worker();
        if !self.needs_plot_reload {
            return;
        }
        if self.plot_worker.is_some() {
            if let Some(token) = &self.plot_cancel {
                token.cancel();
            }
            return;
        }
        let Some(source) = self.source.clone() else {
            self.apply_plot_job_data(None, None);
            self.needs_plot_reload = false;
            return;
        };
        let channels = self.selected_channels();
        if channels.is_empty() {
            self.apply_plot_job_data(None, None);
            self.needs_plot_reload = false;
            return;
        }
        let generation = self.data_generation;
        let meta = source.metadata();
        let start = self.view_start.max(meta.start_time);
        let end = self.view_end.min(meta.end_time);
        if end <= start {
            self.apply_plot_job_data(None, None);
            self.needs_plot_reload = false;
            return;
        }
        let budget_series_count = self.plot_data_series_budget_count(&channels);
        let cache_plot_pixel_width = Self::plot_cache_pixel_width(plot_pixel_width);
        let software_performance_mode = self.software_rendering_performance_mode();
        let key = self.plot_cache_key(
            generation,
            start,
            end,
            &channels,
            0.0,
            cache_plot_pixel_width,
            budget_series_count,
        );
        if self.plot_cache_key.as_ref() == Some(&key) {
            self.needs_plot_reload = false;
            return;
        }
        self.needs_plot_reload = false;
        let result_key = key.clone();
        let cancel_token = JobCancelToken::new();
        let worker_cancel = cancel_token.clone();
        self.plot_cancel = Some(cancel_token);
        Self::spawn_job(&mut self.plot_worker, move || PlotJobResult {
            generation,
            key: result_key,
            result: Self::worker_result("Plot worker panicked.", || {
                let data_cancel = worker_cancel.data_token();
                if worker_cancel.is_cancelled() {
                    return Ok(None);
                }
                let result = Self::load_plot_data_with_cancel(
                    source,
                    start,
                    end,
                    &channels,
                    PlotLoadOptions {
                        budget_series_count,
                        plot_pixel_width: cache_plot_pixel_width,
                        software_performance_mode,
                        cancel: Some(&data_cancel),
                    },
                );
                if worker_cancel.is_cancelled() {
                    Ok(None)
                } else {
                    result
                }
            }),
        });
    }

    fn poll_compare_plot_worker(&mut self) {
        let Some(joined) = Self::take_finished_job(
            &mut self.compare_plot_worker,
            "Compare plot worker panicked.",
        ) else {
            return;
        };
        self.compare_plot_cancel = None;
        let Ok(result) = joined else {
            if !self.needs_compare_plot_reload {
                self.last_error = Some("Compare plot worker panicked.".to_owned());
            }
            return;
        };
        if !self.result_matches_generation(result.generation, self.needs_compare_plot_reload) {
            return;
        }
        let mut prepared_changed = false;
        for dataset_result in result.datasets {
            let dataset_index = dataset_result.index;
            match dataset_result.result {
                Ok(Some(PlotJobData::Samples(block))) => {
                    let selected = dataset_result.key.channels.clone();
                    let time_offset = self.dataset_time_offset(dataset_index + 1);
                    let prepared = self.prepare_sample_series(&block, &selected, time_offset);
                    if let Some(dataset) = self.imported_datasets.get_mut(dataset_index) {
                        dataset.plot_cache = block;
                        dataset.plot_summary = None;
                        dataset.prepared_plot_cache = prepared;
                        dataset.prepared_plot_summary = None;
                        dataset.plot_cache_key = Some(dataset_result.key);
                        prepared_changed = true;
                    }
                }
                Ok(Some(PlotJobData::Summary(summary))) => {
                    let selected = dataset_result.key.channels.clone();
                    let time_offset = self.dataset_time_offset(dataset_index + 1);
                    let prepared = self.prepare_summary_series(&summary, &selected, time_offset);
                    if let Some(dataset) = self.imported_datasets.get_mut(dataset_index) {
                        dataset.plot_cache = SampleBlock::default();
                        dataset.plot_summary = Some(summary);
                        dataset.prepared_plot_cache = PreparedPlotSeries::default();
                        dataset.prepared_plot_summary = Some(prepared);
                        dataset.plot_cache_key = Some(dataset_result.key);
                        prepared_changed = true;
                    }
                }
                Ok(None) => {
                    if let Some(dataset) = self.imported_datasets.get_mut(dataset_index) {
                        dataset.plot_cache = SampleBlock::default();
                        dataset.plot_summary = None;
                        dataset.prepared_plot_cache = PreparedPlotSeries::default();
                        dataset.prepared_plot_summary = None;
                        dataset.plot_cache_key = None;
                        prepared_changed = true;
                    }
                }
                Err(error) => self.last_error = Some(error),
            }
        }
        if prepared_changed {
            self.clear_pane_prepared_plot_cache();
        }
    }

    fn reload_compare_plot_cache(&mut self, plot_pixel_width: f32) {
        self.poll_compare_plot_worker();
        if !self.needs_compare_plot_reload {
            return;
        }
        if self.compare_plot_worker.is_some() {
            if let Some(token) = &self.compare_plot_cancel {
                token.cancel();
            }
            return;
        }
        let sync_time_axes = self.sync_time_axes;
        let primary_channels = self.selected_channels();
        let selected_inputs = self
            .imported_datasets
            .iter()
            .enumerate()
            .map(|(index, dataset)| {
                let offset = if sync_time_axes {
                    dataset.time_offset
                } else {
                    0.0
                };
                (
                    index,
                    dataset.source.clone(),
                    self.selected_imported_channels(index),
                    offset,
                )
            })
            .collect::<Vec<_>>();
        let budget_series_count = primary_channels.len()
            + selected_inputs
                .iter()
                .map(|(_, _, channels, _)| channels.len())
                .sum::<usize>();
        let generation = self.data_generation;
        let view_start = self.view_start;
        let view_end = self.view_end;
        let cache_plot_pixel_width = Self::plot_cache_pixel_width(plot_pixel_width);
        let software_performance_mode = self.software_rendering_performance_mode();
        let mut inputs = Vec::new();
        for (index, source, channels, offset) in selected_inputs {
            if channels.is_empty() {
                if let Some(dataset) = self.imported_datasets.get_mut(index) {
                    dataset.plot_cache = SampleBlock::default();
                    dataset.plot_summary = None;
                    dataset.prepared_plot_cache = PreparedPlotSeries::default();
                    dataset.prepared_plot_summary = None;
                    dataset.plot_cache_key = None;
                }
                self.clear_pane_prepared_plot_cache();
                continue;
            }
            let meta = source.metadata();
            let read_start = (view_start - offset).max(meta.start_time);
            let read_end = (view_end - offset).min(meta.end_time);
            if read_end <= read_start {
                if let Some(dataset) = self.imported_datasets.get_mut(index) {
                    dataset.plot_cache = SampleBlock::default();
                    dataset.plot_summary = None;
                    dataset.prepared_plot_cache = PreparedPlotSeries::default();
                    dataset.prepared_plot_summary = None;
                    dataset.plot_cache_key = None;
                }
                continue;
            }
            let key = self.plot_cache_key(
                generation,
                read_start,
                read_end,
                &channels,
                offset,
                cache_plot_pixel_width,
                budget_series_count,
            );
            if self
                .imported_datasets
                .get(index)
                .is_some_and(|dataset| dataset.plot_cache_key.as_ref() == Some(&key))
            {
                continue;
            }
            inputs.push(ComparePlotJobInput {
                index,
                source,
                channels,
                offset,
                key,
            });
        }
        if inputs.is_empty() {
            self.needs_compare_plot_reload = false;
            return;
        }
        let input_error_keys = inputs
            .iter()
            .map(|input| (input.index, input.key.clone()))
            .collect::<Vec<_>>();
        self.needs_compare_plot_reload = false;
        let cancel_token = JobCancelToken::new();
        let worker_cancel = cancel_token.clone();
        self.compare_plot_cancel = Some(cancel_token);
        Self::spawn_job(&mut self.compare_plot_worker, move || {
            let data_cancel = worker_cancel.data_token();
            let datasets = match panic::catch_unwind(AssertUnwindSafe(|| {
                let mut datasets = Vec::with_capacity(inputs.len());
                for input in inputs {
                    if worker_cancel.is_cancelled() {
                        break;
                    }
                    let ComparePlotJobInput {
                        index,
                        source,
                        channels,
                        offset,
                        key,
                    } = input;
                    let result = Self::worker_result("Compare plot worker panicked.", || {
                        if worker_cancel.is_cancelled() || channels.is_empty() {
                            return Ok(None);
                        }
                        let meta = source.metadata();
                        let read_start = (view_start - offset).max(meta.start_time);
                        let read_end = (view_end - offset).min(meta.end_time);
                        if read_end <= read_start {
                            return Ok(None);
                        }
                        let result = Self::load_plot_data_with_cancel(
                            source,
                            read_start,
                            read_end,
                            &channels,
                            PlotLoadOptions {
                                budget_series_count,
                                plot_pixel_width: cache_plot_pixel_width,
                                software_performance_mode,
                                cancel: Some(&data_cancel),
                            },
                        );
                        if worker_cancel.is_cancelled() {
                            Ok(None)
                        } else {
                            result
                        }
                    });
                    if worker_cancel.is_cancelled() {
                        break;
                    }
                    datasets.push(CompareDatasetJobResult { index, key, result });
                }
                datasets
            })) {
                Ok(datasets) => datasets,
                Err(payload) => {
                    let error =
                        Self::recover_worker_panic("Compare plot worker panicked.", payload);
                    input_error_keys
                        .into_iter()
                        .map(|(index, key)| CompareDatasetJobResult {
                            index,
                            key,
                            result: Err(error.clone()),
                        })
                        .collect()
                }
            };
            ComparePlotJobResult {
                generation,
                datasets,
            }
        });
    }

    fn dataset_read_request_for_range(
        &self,
        dataset_index: usize,
        start: f64,
        end: f64,
    ) -> Option<(Arc<dyn DataSource>, f64, f64, f64)> {
        if dataset_index == 0 {
            self.source.clone().map(|source| (source, start, end, 0.0))
        } else {
            let dataset = self.imported_datasets.get(dataset_index - 1)?;
            let offset = self.dataset_time_offset(dataset_index);
            let meta = dataset.source.metadata();
            let read_start = (start - offset).max(meta.start_time);
            let read_end = (end - offset).min(meta.end_time);
            Some((dataset.source.clone(), read_start, read_end, offset))
        }
    }

    fn derived_key_for_range(&self, start: f64, end: f64) -> DerivedJobKey {
        DerivedJobKey {
            generation: self.data_generation,
            dataset_index: self.selected_fft_dataset_index(),
            start,
            end,
            pll_channels: self.pll_source_channels,
            dq_channels: self.dq_source_channels,
            channels: self.selected_derived_channels(),
            repid_signatures: self.repid_derived_signatures(),
        }
    }

    fn repid_derived_signatures(&self) -> Vec<crate::repid_derived::RepidCurveSignature> {
        self.repid_derived_curves
            .iter()
            .map(|curve| curve.cache_signature())
            .collect()
    }

    fn derived_cache_matches(&self, key: &DerivedJobKey) -> bool {
        self.derived_curve_cache.as_ref().is_some_and(|cache| {
            cache.dataset_index == key.dataset_index
                && cache.start == key.start
                && cache.end == key.end
                && cache.pll_channels == key.pll_channels
                && cache.dq_channels == key.dq_channels
                && cache.channels == key.channels
                && cache.repid_signatures == key.repid_signatures
        })
    }

    fn poll_derived_curve_worker(&mut self, expected_key: &DerivedJobKey) {
        let Some(joined) =
            Self::take_finished_job(&mut self.derived_curve_worker, "PLL/dq0 worker panicked.")
        else {
            return;
        };
        self.derived_curve_worker_key = None;
        self.derived_curve_cancel = None;
        let Ok(result) = joined else {
            self.derived_curve_cache = Some(DerivedCurveCache {
                dataset_index: expected_key.dataset_index,
                start: expected_key.start,
                end: expected_key.end,
                pll_channels: expected_key.pll_channels,
                dq_channels: expected_key.dq_channels,
                channels: expected_key.channels.clone(),
                repid_signatures: expected_key.repid_signatures.clone(),
            });
            self.prepared_derived_curve_cache = PreparedPlotSeries::default();
            self.clear_pane_prepared_plot_cache();
            self.last_error = Some("PLL/dq0 worker panicked.".to_owned());
            return;
        };
        let result_key = DerivedJobKey {
            generation: result.generation,
            dataset_index: result.dataset_index,
            start: result.start,
            end: result.end,
            pll_channels: result.pll_channels,
            dq_channels: result.dq_channels,
            channels: result.channels.clone(),
            repid_signatures: result.repid_signatures.clone(),
        };
        if &result_key != expected_key {
            return;
        }
        let selected = self.selected_derived_channels();
        let time_offset = self.dataset_time_offset(result.dataset_index);
        self.prepared_derived_curve_cache = result
            .result
            .as_ref()
            .map(|block| self.prepare_derived_sample_series(block, &selected, time_offset))
            .unwrap_or_default();
        self.clear_pane_prepared_plot_cache();
        if let Err(error) = &result.result {
            if !selected.is_empty() {
                self.last_error = Some(error.clone());
            }
        }
        self.derived_curve_cache = Some(DerivedCurveCache {
            dataset_index: result.dataset_index,
            start: result.start,
            end: result.end,
            pll_channels: result.pll_channels,
            dq_channels: result.dq_channels,
            channels: result.channels,
            repid_signatures: result.repid_signatures,
        });
    }

    fn start_derived_curve_worker(&mut self, key: DerivedJobKey) {
        if self.derived_curve_worker_key.as_ref() == Some(&key) {
            return;
        }
        if let Some(token) = &self.derived_curve_cancel {
            token.cancel();
        }
        self.derived_curve_worker = None;
        self.derived_curve_worker_key = None;
        self.derived_curve_cancel = None;
        if self.selected_derived_channels().is_empty() {
            self.derived_curve_cache = None;
            self.prepared_derived_curve_cache = PreparedPlotSeries::default();
            self.clear_pane_prepared_plot_cache();
            self.needs_derived_reload = false;
            return;
        }
        let Some((source, read_start, read_end, _)) =
            self.dataset_read_request_for_range(key.dataset_index, key.start, key.end)
        else {
            return;
        };
        let pll_scales = key.pll_channels.map(|channel| self.channel_scale(channel));
        let dq_scales = key.dq_channels.map(|channel| self.channel_scale(channel));
        let channel_scales = self.channel_scales.clone();
        let repid_curves = self.repid_derived_curves.clone();
        let sample_rate_hz = self.sample_rate_hz.max(1.0);
        let harmonic_base_hz = self.harmonic_base_hz.max(0.001);
        let skip_digital_by_samples =
            self.dataset_kind_by_index(key.dataset_index) != Some(SourceKind::Cloud);
        let visible_primary = self.selected_channels();
        let max_points = Self::draw_points_per_channel_with_mode(
            self.plot_data_series_budget_count(&visible_primary).max(1),
            self.software_rendering_performance_mode(),
        );
        let generation = key.generation;
        self.derived_curve_worker_key = Some(key.clone());
        self.needs_derived_reload = false;
        let cancel_token = JobCancelToken::new();
        let worker_cancel = cancel_token.clone();
        self.derived_curve_cancel = Some(cancel_token);
        Self::spawn_job(&mut self.derived_curve_worker, move || {
            let result = Self::worker_result("PLL/dq0 worker panicked.", || {
                let data_cancel = worker_cancel.data_token();
                if worker_cancel.is_cancelled() {
                    return Ok(SampleBlock::default());
                }
                Self::load_derived_data_with_cancel(
                    source,
                    read_start,
                    read_end,
                    &key.channels,
                    &repid_curves,
                    key.pll_channels,
                    key.dq_channels,
                    pll_scales,
                    dq_scales,
                    &channel_scales,
                    sample_rate_hz,
                    harmonic_base_hz,
                    skip_digital_by_samples,
                    max_points,
                    Some(&data_cancel),
                )
            });
            DerivedCurveJobResult {
                generation,
                dataset_index: key.dataset_index,
                start: key.start,
                end: key.end,
                pll_channels: key.pll_channels,
                dq_channels: key.dq_channels,
                channels: key.channels,
                repid_signatures: key.repid_signatures,
                result,
            }
        });
    }

    fn reload_derived_curve_cache(&mut self) {
        let key = self.derived_key_for_range(self.view_start, self.view_end);
        self.poll_derived_curve_worker(&key);
        if self.selected_derived_channels().is_empty() {
            self.derived_curve_cache = None;
            self.prepared_derived_curve_cache = PreparedPlotSeries::default();
            self.clear_pane_prepared_plot_cache();
            self.needs_derived_reload = false;
            return;
        }
        if self.needs_derived_reload || !self.derived_cache_matches(&key) {
            self.start_derived_curve_worker(key);
        }
    }

    fn visible_time_span(&self) -> f64 {
        (self.view_end - self.view_start).max(f64::EPSILON)
    }

    fn clear_y_overrides(&mut self) {
        self.y_min = None;
        self.y_max = None;
        for bounds in &mut self.pane_y_bounds {
            *bounds = None;
        }
        self.pane_y_bounds_cache_key = None;
    }

    fn clear_pane_prepared_plot_cache(&mut self) {
        self.pane_prepared_plot_cache_key = None;
        self.pane_prepared_plot_cache = PanePreparedPlotCache::default();
        self.pane_y_bounds_cache_key = None;
    }

    fn auto_fit_y_axis(&mut self) {
        self.clear_y_overrides();
    }

    fn scope_pane_count(&self) -> usize {
        self.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS)
            * self.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS)
    }

    fn sync_pane_y_bounds_len(&mut self) {
        let pane_count = self.scope_pane_count();
        self.pane_y_bounds.truncate(pane_count);
        if self.pane_y_bounds.len() < pane_count {
            self.pane_y_bounds.resize(pane_count, None);
        }
        self.pane_y_bounds_cache.truncate(pane_count);
        if self.pane_y_bounds_cache.len() < pane_count {
            self.pane_y_bounds_cache.resize(pane_count, (-1.0, 1.0));
        }
        self.pane_prepared_plot_cache.panes.truncate(pane_count);
    }

    fn current_scope_pane(&self) -> usize {
        self.active_scope_pane
            .min(self.scope_pane_count().saturating_sub(1))
    }

    fn set_active_scope_pane(&mut self, pane_index: usize) {
        self.active_scope_pane = pane_index.min(self.scope_pane_count().saturating_sub(1));
    }

    fn assign_channel_to_active_pane(&mut self, channel_index: usize) {
        let active_pane = self.current_scope_pane();
        if let Some(pane) = self.channel_panes.get_mut(channel_index) {
            *pane = active_pane;
        }
    }

    fn channel_in_scope_pane(
        &self,
        channel_index: usize,
        pane_index: usize,
        pane_count: usize,
    ) -> bool {
        self.channel_scope_pane(channel_index, pane_count) == pane_index
    }

    fn channel_scope_pane(&self, channel_index: usize, pane_count: usize) -> usize {
        if pane_count <= 1 {
            0
        } else {
            self.channel_panes
                .get(channel_index)
                .copied()
                .unwrap_or(0)
                .min(pane_count.saturating_sub(1))
        }
    }

    fn defer_plot_reload(&mut self) {
        self.needs_plot_reload = true;
        self.needs_compare_plot_reload = true;
        self.plot_reload_deferred_until = Some(Instant::now() + PLOT_RELOAD_DEBOUNCE);
    }

    fn plot_reload_debounce_ready(&mut self, ctx: &egui::Context) -> bool {
        let Some(deadline) = self.plot_reload_deferred_until else {
            return true;
        };
        let now = Instant::now();
        if now < deadline {
            ctx.request_repaint_after(deadline.saturating_duration_since(now));
            false
        } else {
            self.plot_reload_deferred_until = None;
            true
        }
    }

    fn plot_reload_allowed(debounce_ready: bool, layout_resize_active: bool) -> bool {
        debounce_ready && !layout_resize_active
    }

    fn plot_interaction_debounce_active(&self) -> bool {
        self.plot_reload_deferred_until
            .is_some_and(|deadline| Instant::now() < deadline)
    }

    fn zoom(&mut self, center: f64, factor: f64) {
        let Some(meta) = self.meta() else {
            return;
        };
        let start_time = meta.start_time;
        let end_time = meta.end_time;
        let duration = meta.duration();
        let sample_rate = meta.nominal_sample_rate_hz;
        if duration <= 0.0 || sample_rate <= 0.0 {
            return;
        }
        let old_span = self.visible_time_span();
        let new_span = (old_span * factor).clamp(1.0 / sample_rate, duration);
        let ratio = ((center - self.view_start) / old_span).clamp(0.0, 1.0);
        let mut start = center - ratio * new_span;
        let mut end = start + new_span;
        if start < start_time {
            start = start_time;
            end = start + new_span;
        }
        if end > end_time {
            end = end_time;
            start = end - new_span;
        }
        self.view_start = start.max(start_time);
        self.view_end = end.min(end_time);
        self.defer_plot_reload();
    }

    fn zoom_y_with_bounds(
        &mut self,
        pane_index: usize,
        pane_count: usize,
        center: f64,
        factor: f64,
        current_min: f64,
        current_max: f64,
    ) {
        let old_span = (current_max - current_min).abs().max(f64::EPSILON);
        let new_span = (old_span * factor).max(f64::EPSILON);
        let ratio = ((center - current_min) / old_span).clamp(0.0, 1.0);
        let next_bounds = (center - ratio * new_span, center + (1.0 - ratio) * new_span);
        if pane_count <= 1 {
            self.y_min = Some(next_bounds.0);
            self.y_max = Some(next_bounds.1);
        } else if let Some(bounds) = self.pane_y_bounds.get_mut(pane_index) {
            *bounds = Some(next_bounds);
        }
        self.pane_y_bounds_cache_key = None;
    }

    fn add_bounds(accum: &mut (f64, f64), bounds: Option<(f64, f64)>) {
        let Some((series_min, series_max)) = bounds else {
            return;
        };
        let (min, max) = accum;
        *min = min.min(series_min);
        *max = max.max(series_max);
    }

    fn finalize_y_bounds(min: f64, max: f64) -> (f64, f64) {
        if !min.is_finite() || !max.is_finite() || max <= min {
            return (-1.0, 1.0);
        }
        let padding = ((max - min) * 0.08).max(f64::EPSILON);
        (min - padding, max + padding)
    }

    fn y_override_bits(bounds: Option<(f64, f64)>) -> Option<(u64, u64)> {
        bounds.map(|(min, max)| (min.to_bits(), max.to_bits()))
    }

    fn pane_selection_out_indices(items: &[(usize, usize)]) -> Vec<usize> {
        items.iter().map(|(out_index, _)| *out_index).collect()
    }

    fn pane_prepared_plot_cache_key(
        &self,
        pane_selections: &[PanePlotSelections],
        pane_count: usize,
    ) -> PanePreparedPlotCacheKey {
        let derived_key = (!self.selected_derived_channels().is_empty())
            .then(|| self.derived_key_for_range(self.view_start, self.view_end));
        PanePreparedPlotCacheKey {
            pane_count,
            selections: pane_selections.to_vec(),
            primary_plot_key: self.plot_cache_key.clone(),
            imported_plot_keys: self
                .imported_datasets
                .iter()
                .map(|dataset| dataset.plot_cache_key.clone())
                .collect(),
            derived_key,
        }
    }

    fn rebuild_pane_prepared_plot_cache(
        &self,
        pane_selections: &[PanePlotSelections],
        pane_count: usize,
    ) -> PanePreparedPlotCache {
        let mut panes = Vec::with_capacity(pane_count.max(1));
        for pane_index in 0..pane_count.max(1) {
            let empty_pane = PanePlotSelections::default();
            let pane = pane_selections.get(pane_index).unwrap_or(&empty_pane);
            let primary_indices = Self::pane_selection_out_indices(&pane.primary);
            let derived_indices = Self::pane_selection_out_indices(&pane.derived);
            let mut prepared_pane = PanePreparedPlotPane {
                primary_samples: self
                    .prepared_plot_cache
                    .subset_by_out_indices(&primary_indices),
                primary_summary: self
                    .prepared_plot_summary
                    .as_ref()
                    .map(|series| series.subset_by_out_indices(&primary_indices)),
                imported: Vec::with_capacity(self.imported_datasets.len()),
                derived: self
                    .prepared_derived_curve_cache
                    .subset_by_out_indices(&derived_indices),
            };

            for (dataset_index, dataset) in self.imported_datasets.iter().enumerate() {
                let imported_indices = pane
                    .imported
                    .get(dataset_index)
                    .map(|items| Self::pane_selection_out_indices(items))
                    .unwrap_or_default();
                prepared_pane.imported.push(PanePreparedImportedDataset {
                    samples: dataset
                        .prepared_plot_cache
                        .subset_by_out_indices(&imported_indices),
                    summary: dataset
                        .prepared_plot_summary
                        .as_ref()
                        .map(|series| series.subset_by_out_indices(&imported_indices)),
                });
            }
            panes.push(prepared_pane);
        }

        PanePreparedPlotCache { panes }
    }

    fn ensure_pane_prepared_plot_cache(
        &mut self,
        pane_selections: &[PanePlotSelections],
        pane_count: usize,
    ) {
        let key = self.pane_prepared_plot_cache_key(pane_selections, pane_count);
        if self.pane_prepared_plot_cache_key.as_ref() == Some(&key)
            && self.pane_prepared_plot_cache.panes.len() == pane_count.max(1)
        {
            return;
        }

        self.pane_prepared_plot_cache =
            self.rebuild_pane_prepared_plot_cache(pane_selections, pane_count);
        self.pane_prepared_plot_cache_key = Some(key);
        self.pane_y_bounds_cache_key = None;
    }

    fn pane_y_bounds_cache_key(
        &self,
        pane_selections: &[PanePlotSelections],
        pane_count: usize,
    ) -> PaneYBoundsCacheKey {
        let derived_key = (!self.selected_derived_channels().is_empty())
            .then(|| self.derived_key_for_range(self.view_start, self.view_end));
        PaneYBoundsCacheKey {
            pane_count,
            selections: pane_selections.to_vec(),
            primary_plot_key: self.plot_cache_key.clone(),
            imported_plot_keys: self
                .imported_datasets
                .iter()
                .map(|dataset| dataset.plot_cache_key.clone())
                .collect(),
            derived_key,
            global_y_override: match (self.y_min, self.y_max) {
                (Some(min), Some(max)) => Some((min.to_bits(), max.to_bits())),
                _ => None,
            },
            pane_y_overrides: self
                .pane_y_bounds
                .iter()
                .copied()
                .map(Self::y_override_bits)
                .collect(),
        }
    }

    fn current_y_bounds_for_panes_cached(
        &mut self,
        pane_selections: &[PanePlotSelections],
        pane_count: usize,
    ) -> Vec<(f64, f64)> {
        let key = self.pane_y_bounds_cache_key(pane_selections, pane_count);
        if self.pane_y_bounds_cache_key.as_ref() == Some(&key)
            && self.pane_y_bounds_cache.len() == pane_count.max(1)
        {
            return self.pane_y_bounds_cache.clone();
        }

        let bounds = self
            .current_y_bounds_for_prepared_panes(pane_count)
            .unwrap_or_else(|| self.current_y_bounds_for_panes(pane_selections, pane_count));
        self.pane_y_bounds_cache_key = Some(key);
        self.pane_y_bounds_cache = bounds.clone();
        bounds
    }

    fn accumulate_prepared_bounds(accum: &mut (f64, f64), prepared: &PreparedPlotSeries) {
        for (series_min, series_max) in prepared.bounds.iter().flatten().copied() {
            accum.0 = accum.0.min(series_min);
            accum.1 = accum.1.max(series_max);
        }
    }

    fn current_y_bounds_for_prepared_panes(&self, pane_count: usize) -> Option<Vec<(f64, f64)>> {
        let pane_count = pane_count.max(1);
        if self.pane_prepared_plot_cache.panes.len() != pane_count {
            return None;
        }
        if pane_count <= 1 {
            if let (Some(min), Some(max)) = (self.y_min, self.y_max) {
                if min.is_finite() && max.is_finite() && max > min {
                    return Some(vec![(min, max)]);
                }
            }
        }

        let mut bounds = Vec::with_capacity(pane_count);
        for pane in &self.pane_prepared_plot_cache.panes {
            let mut accum = (f64::INFINITY, f64::NEG_INFINITY);
            if let Some(summary) = &pane.primary_summary {
                Self::accumulate_prepared_bounds(&mut accum, summary);
            } else {
                Self::accumulate_prepared_bounds(&mut accum, &pane.primary_samples);
            }
            for imported in &pane.imported {
                if let Some(summary) = &imported.summary {
                    Self::accumulate_prepared_bounds(&mut accum, summary);
                } else {
                    Self::accumulate_prepared_bounds(&mut accum, &imported.samples);
                }
            }
            Self::accumulate_prepared_bounds(&mut accum, &pane.derived);
            bounds.push(Self::finalize_y_bounds(accum.0, accum.1));
        }

        if pane_count > 1 {
            for (pane_index, bound) in bounds.iter_mut().enumerate() {
                if let Some(Some((min, max))) = self.pane_y_bounds.get(pane_index) {
                    if min.is_finite() && max.is_finite() && max > min {
                        *bound = (*min, *max);
                    }
                }
            }
        }
        Some(bounds)
    }

    fn current_y_bounds_for_panes(
        &self,
        pane_selections: &[PanePlotSelections],
        pane_count: usize,
    ) -> Vec<(f64, f64)> {
        if pane_count <= 1 {
            if let (Some(min), Some(max)) = (self.y_min, self.y_max) {
                if min.is_finite() && max.is_finite() && max > min {
                    return vec![(min, max)];
                }
            }
        }

        let mut accum = vec![(f64::INFINITY, f64::NEG_INFINITY); pane_count.max(1)];
        if let Some(summary) = &self.prepared_plot_summary {
            for (pane_index, pane) in pane_selections.iter().enumerate() {
                for (out_index, _) in &pane.primary {
                    if let Some(accum) = accum.get_mut(pane_index) {
                        Self::add_bounds(accum, summary.bounds.get(*out_index).copied().flatten());
                    }
                }
            }
        } else {
            for (pane_index, pane) in pane_selections.iter().enumerate() {
                for (out_index, _) in &pane.primary {
                    if let Some(accum) = accum.get_mut(pane_index) {
                        Self::add_bounds(
                            accum,
                            self.prepared_plot_cache
                                .bounds
                                .get(*out_index)
                                .copied()
                                .flatten(),
                        );
                    }
                }
            }
        }

        for (dataset_index, dataset) in self.imported_datasets.iter().enumerate() {
            if let Some(summary) = &dataset.prepared_plot_summary {
                for (pane_index, pane) in pane_selections.iter().enumerate() {
                    let compare_selected = pane
                        .imported
                        .get(dataset_index)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    for (out_index, _) in compare_selected {
                        if let Some(accum) = accum.get_mut(pane_index) {
                            Self::add_bounds(
                                accum,
                                summary.bounds.get(*out_index).copied().flatten(),
                            );
                        }
                    }
                }
            } else {
                for (pane_index, pane) in pane_selections.iter().enumerate() {
                    let compare_selected = pane
                        .imported
                        .get(dataset_index)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    for (out_index, _) in compare_selected {
                        if let Some(accum) = accum.get_mut(pane_index) {
                            Self::add_bounds(
                                accum,
                                dataset
                                    .prepared_plot_cache
                                    .bounds
                                    .get(*out_index)
                                    .copied()
                                    .flatten(),
                            );
                        }
                    }
                }
            }
        }

        for (pane_index, pane) in pane_selections.iter().enumerate() {
            for (out_index, _) in &pane.derived {
                if let Some(accum) = accum.get_mut(pane_index) {
                    Self::add_bounds(
                        accum,
                        self.prepared_derived_curve_cache
                            .bounds
                            .get(*out_index)
                            .copied()
                            .flatten(),
                    );
                }
            }
        }

        let mut bounds = accum
            .into_iter()
            .map(|(min, max)| Self::finalize_y_bounds(min, max))
            .collect::<Vec<_>>();
        if pane_count > 1 {
            for (pane_index, bound) in bounds.iter_mut().enumerate() {
                if let Some(Some((min, max))) = self.pane_y_bounds.get(pane_index) {
                    if min.is_finite() && max.is_finite() && max > min {
                        *bound = (*min, *max);
                    }
                }
            }
        }
        bounds
    }

    fn pan(&mut self, delta_time: f64) {
        let Some(meta) = self.meta() else {
            return;
        };
        let start_time = meta.start_time;
        let end_time = meta.end_time;
        let span = self.visible_time_span();
        let mut start = self.view_start + delta_time;
        let mut end = self.view_end + delta_time;
        if start < start_time {
            start = start_time;
            end = start + span;
        }
        if end > end_time {
            end = end_time;
            start = end - span;
        }
        self.view_start = start.max(start_time);
        self.view_end = end.min(end_time);
        self.defer_plot_reload();
    }

    fn reset_view(&mut self) {
        if let Some(meta) = self.meta() {
            let start_time = meta.start_time;
            let end_time = meta.end_time;
            self.view_start = start_time;
            self.view_end = end_time;
            self.y_min = None;
            self.y_max = None;
            self.needs_plot_reload = true;
            self.needs_compare_plot_reload = true;
            self.needs_derived_reload = true;
        }
    }

    fn move_active_cursor(&mut self, time: f64) {
        let Some(meta) = self.meta() else {
            return;
        };
        let clamped = time.clamp(meta.start_time, meta.end_time);
        match self.active_cursor {
            CursorId::A => {
                self.cursor_a = clamped;
                self.show_cursor_a = true;
            }
            CursorId::B => {
                self.cursor_b = clamped;
                self.show_cursor_b = true;
            }
        }
        self.measurement_cache = None;
        self.derived_measurement_cache = None;
        self.needs_fft_reload = true;
    }

    fn set_cursor(&mut self, cursor: CursorId, time: f64) {
        let Some(meta) = self.meta() else {
            return;
        };
        let clamped = time.clamp(meta.start_time, meta.end_time);
        match cursor {
            CursorId::A => {
                self.cursor_a = clamped;
                self.show_cursor_a = true;
            }
            CursorId::B => {
                self.cursor_b = clamped;
                self.show_cursor_b = true;
            }
        }
        self.measurement_cache = None;
        self.derived_measurement_cache = None;
        self.needs_fft_reload = true;
    }

    fn cursor_label(cursor: CursorId) -> &'static str {
        match cursor {
            CursorId::A => "X1",
            CursorId::B => "X2",
        }
    }

    fn cursor_color(cursor: CursorId) -> Color32 {
        let _ = cursor;
        Color32::from_rgb(255, 40, 40)
    }

    fn zoom_to_range(&mut self, start: f64, end: f64) {
        let Some(meta) = self.meta() else {
            return;
        };
        let min_span = (1.0 / meta.nominal_sample_rate_hz.max(1.0)).max(f64::EPSILON);
        let range_start = start.min(end);
        let range_end = start.max(end);
        let mut start = range_start.clamp(meta.start_time, meta.end_time);
        let mut end = range_end.clamp(meta.start_time, meta.end_time);
        if end - start < min_span {
            let center = (start + end) * 0.5;
            start = (center - min_span * 0.5).clamp(meta.start_time, meta.end_time);
            end = start + min_span;
            if end > meta.end_time {
                end = meta.end_time;
                start = (end - min_span).max(meta.start_time);
            }
        }
        if end > start {
            self.view_start = start;
            self.view_end = end;
            self.needs_plot_reload = true;
            self.needs_compare_plot_reload = true;
        }
    }

    fn clamp_to_plot_rect(pos: egui::Pos2, rect: egui::Rect) -> egui::Pos2 {
        egui::pos2(
            pos.x.clamp(rect.left(), rect.right()),
            pos.y.clamp(rect.top(), rect.bottom()),
        )
    }

    fn run_fft(&mut self) {
        self.poll_fft_worker();
        if self.start_fft_worker() {
            return;
        }

        let dataset_index = self.selected_fft_dataset_index();
        self.fft_dataset_index = dataset_index;
        let Some(meta) = self.dataset_meta_by_index(dataset_index).cloned() else {
            return;
        };
        if meta.channels.is_empty() {
            self.fft_results.clear();
            self.needs_fft_reload = false;
            return;
        }

        let channels = self.fft_channel_options();
        if channels.is_empty() {
            self.fft_results.clear();
            self.needs_fft_reload = false;
            return;
        }
        if !channels.contains(&self.fft_channel) {
            self.fft_channel = self.preferred_fft_channel(&channels).unwrap_or(channels[0]);
            self.fft_channel_user_selected = false;
        }
        let fft_channel = self.fft_channel;

        let start = self.cursor_a.min(self.cursor_b);
        let end = self.cursor_a.max(self.cursor_b);
        let sample_rate_hz = self.sample_rate_hz.max(1.0);
        let harmonic_base_hz = self.harmonic_base_hz.max(0.001);
        let channel_name = self.fft_channel_name(dataset_index, fft_channel);
        let channel_scale = self.channel_scale(fft_channel);
        let skip_digital_by_samples =
            self.dataset_kind_by_index(dataset_index) != Some(SourceKind::Cloud);

        let read_result = if dataset_index == 0 {
            let Some(source) = &self.source else {
                return;
            };
            source.read_range(start, end, &[fft_channel], MAX_FFT_POINTS)
        } else {
            let Some(dataset) = self.imported_datasets.get(dataset_index - 1) else {
                return;
            };
            let offset = self.dataset_time_offset(dataset_index);
            let meta = dataset.source.metadata();
            let read_start = (start - offset).max(meta.start_time);
            let read_end = (end - offset).min(meta.end_time);
            if read_end <= read_start {
                self.fft_results.clear();
                self.needs_fft_reload = false;
                return;
            }
            dataset
                .source
                .read_range(read_start, read_end, &[fft_channel], MAX_FFT_POINTS)
        };
        let mut next_fft = Vec::new();
        let mut next_error = None;

        match read_result {
            Ok(block) => {
                if let Some(samples) = block.channels.first() {
                    if skip_digital_by_samples && Self::samples_look_digital(samples) {
                        next_error = Some(
                            self.tr(
                                "所选通道是数字量，已跳过 FFT。",
                                "Selected channel is digital, so FFT is skipped.",
                            )
                            .to_owned(),
                        );
                    } else {
                        let scaled_samples =
                            if (channel_scale - DEFAULT_CHANNEL_SCALE).abs() <= f32::EPSILON {
                                samples.to_vec()
                            } else {
                                samples
                                    .iter()
                                    .map(|sample| *sample * channel_scale)
                                    .collect()
                            };
                        if let Some(result) = fft::analyze(
                            channel_name,
                            &scaled_samples,
                            sample_rate_hz,
                            harmonic_base_hz,
                            10,
                        ) {
                            next_fft.push((fft_channel, result));
                        }
                    }
                }

                if next_fft.is_empty() && next_error.is_none() {
                    next_error = Some(
                        self.tr(
                            "FFT 需要光标区间内至少 16 个采样点。",
                            "FFT needs at least 16 samples in the cursor range.",
                        )
                        .to_owned(),
                    );
                }
            }
            Err(error) => next_error = Some(error.to_string()),
        }

        self.fft_results = next_fft;
        self.needs_fft_reload = false;
        if let Some(error) = next_error {
            if !Self::is_nonfatal_fft_message(&error) {
                self.last_error = Some(error);
            }
        } else if self
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("FFT"))
        {
            self.last_error = None;
        }
    }

    fn start_fft_worker(&mut self) -> bool {
        if !self.needs_fft_reload {
            return true;
        }
        if self.fft_worker.is_some() {
            if let Some(token) = &self.fft_cancel {
                token.cancel();
            }
            return true;
        }
        let dataset_index = self.selected_fft_dataset_index();
        self.fft_dataset_index = dataset_index;
        let Some(meta) = self.dataset_meta_by_index(dataset_index).cloned() else {
            return true;
        };
        if meta.channels.is_empty() {
            self.fft_results.clear();
            self.needs_fft_reload = false;
            return true;
        }

        let channels = self.fft_channel_options();
        if channels.is_empty() {
            self.fft_results.clear();
            self.needs_fft_reload = false;
            return true;
        }
        if !channels.contains(&self.fft_channel) {
            self.fft_channel = self.preferred_fft_channel(&channels).unwrap_or(channels[0]);
            self.fft_channel_user_selected = false;
        }
        let fft_channel = self.fft_channel;

        let start = self.cursor_a.min(self.cursor_b);
        let end = self.cursor_a.max(self.cursor_b);
        let sample_rate_hz = self.sample_rate_hz.max(1.0);
        let harmonic_base_hz = self.harmonic_base_hz.max(0.001);
        let channel_name = self.fft_channel_name(dataset_index, fft_channel);
        let channel_scale = self.channel_scale(fft_channel);
        let skip_digital_by_samples =
            self.dataset_kind_by_index(dataset_index) != Some(SourceKind::Cloud);
        let (source, read_start, read_end) = if dataset_index == 0 {
            let Some(source) = self.source.clone() else {
                return true;
            };
            (source, start, end)
        } else {
            let Some(dataset) = self.imported_datasets.get(dataset_index - 1) else {
                return true;
            };
            let offset = self.dataset_time_offset(dataset_index);
            let meta = dataset.source.metadata();
            let read_start = (start - offset).max(meta.start_time);
            let read_end = (end - offset).min(meta.end_time);
            if read_end <= read_start {
                self.fft_results.clear();
                self.needs_fft_reload = false;
                return true;
            }
            (dataset.source.clone(), read_start, read_end)
        };

        let generation = self.data_generation;
        self.needs_fft_reload = false;
        let cancel_token = JobCancelToken::new();
        let worker_cancel = cancel_token.clone();
        self.fft_cancel = Some(cancel_token);
        Self::spawn_job(&mut self.fft_worker, move || {
            let result = Self::worker_result("FFT worker panicked.", || {
                let data_cancel = worker_cancel.data_token();
                source
                    .read_range_cancellable(
                        read_start,
                        read_end,
                        &[fft_channel],
                        MAX_FFT_POINTS,
                        &data_cancel,
                    )
                    .map_err(|error| error.to_string())
                    .and_then(|block| {
                        if worker_cancel.is_cancelled() {
                            return Ok(Vec::new());
                        }
                        let Some(samples) = block.channels.first() else {
                            return Ok(Vec::new());
                        };
                        if skip_digital_by_samples && Self::samples_look_digital(samples) {
                            return Ok(Vec::new());
                        }
                        let scaled_samples =
                            if (channel_scale - DEFAULT_CHANNEL_SCALE).abs() <= f32::EPSILON {
                                samples.to_vec()
                            } else {
                                samples
                                    .iter()
                                    .map(|sample| *sample * channel_scale)
                                    .collect()
                            };
                        Ok(fft::analyze(
                            channel_name,
                            &scaled_samples,
                            sample_rate_hz,
                            harmonic_base_hz,
                            10,
                        )
                        .map(|result| vec![(fft_channel, result)])
                        .unwrap_or_default())
                    })
            });
            FftJobResult { generation, result }
        });
        true
    }

    fn poll_fft_worker(&mut self) {
        let Some(joined) = Self::take_finished_job(&mut self.fft_worker, "FFT worker panicked.")
        else {
            return;
        };
        self.fft_cancel = None;
        let Ok(result) = joined else {
            if !self.needs_fft_reload {
                self.last_error = Some("FFT worker panicked.".to_owned());
            }
            return;
        };
        if !self.result_matches_generation(result.generation, self.needs_fft_reload) {
            return;
        }
        match result.result {
            Ok(next_fft) => {
                self.fft_results = next_fft;
                if self
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.contains("FFT"))
                {
                    self.last_error = None;
                }
            }
            Err(error) => {
                self.fft_results.clear();
                if !Self::is_nonfatal_fft_message(&error) {
                    self.last_error = Some(error);
                }
            }
        }
    }

    fn is_nonfatal_fft_message(message: &str) -> bool {
        message.contains("FFT needs at least 16 samples")
            || message.contains("Selected channel is digital")
            || message.contains("已跳过 FFT")
            || message.contains("至少 16")
    }

    fn analysis_read_request(
        &self,
        dataset_index: usize,
    ) -> Option<(Arc<dyn DataSource>, f64, f64)> {
        let start = self.cursor_a.min(self.cursor_b);
        let end = self.cursor_a.max(self.cursor_b);
        if dataset_index == 0 {
            self.source.clone().map(|source| (source, start, end))
        } else {
            let dataset = self.imported_datasets.get(dataset_index - 1)?;
            let offset = self.dataset_time_offset(dataset_index);
            let meta = dataset.source.metadata();
            let read_start = (start - offset).max(meta.start_time);
            let read_end = (end - offset).min(meta.end_time);
            Some((dataset.source.clone(), read_start, read_end))
        }
    }

    #[allow(dead_code)]
    fn read_analysis_range(
        &self,
        dataset_index: usize,
        channels: &[usize],
        max_points: usize,
    ) -> Option<DataResult<SampleBlock>> {
        let (source, read_start, read_end) = self.analysis_read_request(dataset_index)?;
        if read_end <= read_start {
            return Some(Ok(SampleBlock::default()));
        }
        Some(source.read_range(read_start, read_end, channels, max_points))
    }

    #[allow(dead_code)]
    fn sequence_result(&self) -> Result<SequenceResult, String> {
        let dataset_index = self.selected_fft_dataset_index();
        let block = self
            .read_analysis_range(dataset_index, &self.sequence_channels, MAX_FFT_POINTS)
            .ok_or_else(|| self.t(UiText::NoDataLoaded).to_owned())?
            .map_err(|error| error.to_string())?;
        if block.channels.len() < 3 {
            return Err(self
                .tr(
                    "正负序分析需要三个通道。",
                    "Sequence analysis needs three channels.",
                )
                .to_owned());
        }
        let skip_digital_by_samples =
            self.dataset_kind_by_index(dataset_index) != Some(SourceKind::Cloud);
        let samples = self
            .sequence_channels
            .iter()
            .enumerate()
            .map(|(out_index, channel_index)| {
                block
                    .channels
                    .get(out_index)
                    .map(|values| self.scaled_samples(*channel_index, values))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        if skip_digital_by_samples
            && samples
                .iter()
                .any(|values| Self::samples_look_digital(values))
        {
            return Err(self
                .tr(
                    "正负序分析仅支持模拟量通道。",
                    "Sequence analysis only supports analog channels.",
                )
                .to_owned());
        }
        fft::sequence_components(
            &samples[0],
            &samples[1],
            &samples[2],
            self.sample_rate_hz.max(1.0),
            self.harmonic_base_hz.max(0.001),
        )
        .ok_or_else(|| {
            self.tr(
                "正负序分析需要光标区间内至少 16 个采样点。",
                "Sequence analysis needs at least 16 samples in the cursor range.",
            )
            .to_owned()
        })
    }

    fn default_channel_color(index: usize) -> Color32 {
        const COLORS: [Color32; 12] = [
            Color32::from_rgb(42, 157, 143),
            Color32::from_rgb(233, 196, 106),
            Color32::from_rgb(231, 111, 81),
            Color32::from_rgb(38, 70, 83),
            Color32::from_rgb(58, 134, 255),
            Color32::from_rgb(255, 0, 110),
            Color32::from_rgb(131, 56, 236),
            Color32::from_rgb(255, 190, 11),
            Color32::from_rgb(0, 168, 150),
            Color32::from_rgb(239, 71, 111),
            Color32::from_rgb(17, 138, 178),
            Color32::from_rgb(7, 59, 76),
        ];
        COLORS[index % COLORS.len()]
    }

    fn default_derived_color(index: usize) -> Color32 {
        const COLORS: [Color32; DERIVED_CHANNEL_COUNT] = [
            Color32::from_rgb(250, 132, 43),
            Color32::from_rgb(67, 170, 139),
            Color32::from_rgb(87, 117, 144),
            Color32::from_rgb(249, 199, 79),
        ];
        COLORS[index % COLORS.len()]
    }

    fn built_in_derived_channel_name(index: usize) -> &'static str {
        DERIVED_CHANNEL_NAMES
            .get(index)
            .copied()
            .unwrap_or("derived")
    }

    fn derived_output_count_for_repid_curves(repid_curve_count: usize) -> usize {
        DERIVED_CHANNEL_COUNT + repid_curve_count
    }

    fn derived_output_count(&self) -> usize {
        Self::derived_output_count_for_repid_curves(self.repid_derived_curves.len())
    }

    fn derived_channel_name_for_repid_curves(
        index: usize,
        repid_curves: &[crate::repid_derived::RepidDerivedCurve],
    ) -> String {
        if index < DERIVED_CHANNEL_COUNT {
            Self::built_in_derived_channel_name(index).to_owned()
        } else {
            repid_curves
                .get(index - DERIVED_CHANNEL_COUNT)
                .map(|curve| curve.name.as_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("Repid derived")
                .to_owned()
        }
    }

    fn derived_channel_name(&self, index: usize) -> String {
        Self::derived_channel_name_for_repid_curves(index, &self.repid_derived_curves)
    }

    fn repid_curve_input_text(curve: &crate::repid_derived::RepidDerivedCurve) -> String {
        match curve.expression() {
            Ok(expression) if expression.input_channels().is_empty() => "none".to_owned(),
            Ok(expression) => expression
                .input_channels()
                .iter()
                .map(|channel| format!("Ch{}", channel + 1))
                .collect::<Vec<_>>()
                .join(", "),
            Err(_) => "unavailable".to_owned(),
        }
    }

    fn repid_curve_diagnostic_text(
        &self,
        curve: &crate::repid_derived::RepidDerivedCurve,
    ) -> String {
        let channel_count = self
            .dataset_meta_by_index(self.selected_fft_dataset_index())
            .map(|meta| meta.channels.len());
        curve.diagnostic(channel_count)
    }

    fn derived_channel_color(&self, index: usize) -> Color32 {
        self.derived_colors
            .get(index)
            .copied()
            .unwrap_or_else(|| Self::default_derived_color(index))
    }

    fn derived_line_pattern(&self, index: usize) -> ChannelLinePattern {
        self.derived_line_patterns
            .get(index)
            .copied()
            .unwrap_or(ChannelLinePattern::Solid)
    }

    fn derived_in_scope_pane(
        &self,
        derived_index: usize,
        pane_index: usize,
        pane_count: usize,
    ) -> bool {
        self.derived_scope_pane(derived_index, pane_count) == pane_index
    }

    fn derived_scope_pane(&self, derived_index: usize, pane_count: usize) -> usize {
        if pane_count <= 1 {
            0
        } else {
            self.derived_panes
                .get(derived_index)
                .copied()
                .unwrap_or(0)
                .min(pane_count.saturating_sub(1))
        }
    }

    fn selected_derived_channels(&self) -> Vec<usize> {
        let output_count = self.derived_output_count();
        self.derived_visible
            .iter()
            .enumerate()
            .filter_map(|(index, visible)| (*visible && index < output_count).then_some(index))
            .collect()
    }

    fn channel_color(&self, index: usize) -> Color32 {
        self.channel_colors
            .get(index)
            .copied()
            .unwrap_or_else(|| Self::default_channel_color(index))
    }

    fn blend_color(base: Color32, accent: Color32, amount: f32) -> Color32 {
        let amount = amount.clamp(0.0, 1.0);
        let blend = |base: u8, accent: u8| -> u8 {
            (base as f32 * (1.0 - amount) + accent as f32 * amount)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Color32::from_rgba_premultiplied(
            blend(base.r(), accent.r()),
            blend(base.g(), accent.g()),
            blend(base.b(), accent.b()),
            base.a(),
        )
    }

    fn color_distance_sq(a: Color32, b: Color32) -> i32 {
        let dr = a.r() as i32 - b.r() as i32;
        let dg = a.g() as i32 - b.g() as i32;
        let db = a.b() as i32 - b.b() as i32;
        dr * dr + dg * dg + db * db
    }

    fn colors_are_close(a: Color32, b: Color32) -> bool {
        Self::color_distance_sq(a, b) < 10_000
    }

    fn dataset_variant_color(base: Color32, dataset_index: usize) -> Color32 {
        const ACCENTS: [Color32; 8] = [
            Color32::from_rgb(255, 107, 53),
            Color32::from_rgb(58, 134, 255),
            Color32::from_rgb(131, 56, 236),
            Color32::from_rgb(255, 190, 11),
            Color32::from_rgb(6, 214, 160),
            Color32::from_rgb(239, 71, 111),
            Color32::from_rgb(17, 138, 178),
            Color32::from_rgb(7, 59, 76),
        ];
        if dataset_index == 0 {
            base
        } else {
            let mut accent_index = (dataset_index - 1) % ACCENTS.len();
            for step in 0..ACCENTS.len() {
                let candidate = ACCENTS[(accent_index + step) % ACCENTS.len()];
                if Self::color_distance_sq(base, candidate) > 10_000 {
                    accent_index = (accent_index + step) % ACCENTS.len();
                    break;
                }
            }
            Self::blend_color(base, ACCENTS[accent_index], 0.78)
        }
    }

    fn plot_color_with_collision_rank(
        base: Color32,
        channel_index: usize,
        dataset_index: usize,
        collision_rank: usize,
    ) -> Color32 {
        if collision_rank == 0 {
            base
        } else {
            let variant_index = dataset_index
                .saturating_mul(17)
                .saturating_add(channel_index)
                .saturating_add(collision_rank);
            Self::dataset_variant_color(base, variant_index.max(1))
        }
    }

    fn dataset_channel_visible_in_pane(
        &self,
        dataset_index: usize,
        channel_index: usize,
        pane_index: usize,
        pane_count: usize,
    ) -> bool {
        self.channel_in_scope_pane(channel_index, pane_index, pane_count)
            && self.dataset_channel_visible(dataset_index, channel_index)
    }

    fn max_dataset_channel_len(&self, dataset_index: usize) -> usize {
        if dataset_index == 0 {
            self.visible.len()
        } else {
            self.imported_datasets
                .get(dataset_index - 1)
                .map(|dataset| dataset.visible.len())
                .unwrap_or(0)
        }
    }

    fn plot_color_collision_rank(
        &self,
        channel_index: usize,
        dataset_index: usize,
        pane_index: usize,
        pane_count: usize,
    ) -> usize {
        let base = self.channel_color(channel_index);
        let mut rank = 0;
        for candidate_dataset in 0..self.dataset_count() {
            for candidate_channel in 0..self.max_dataset_channel_len(candidate_dataset) {
                if candidate_dataset == dataset_index && candidate_channel == channel_index {
                    return rank;
                }
                if self.dataset_channel_visible_in_pane(
                    candidate_dataset,
                    candidate_channel,
                    pane_index,
                    pane_count,
                ) && Self::colors_are_close(self.channel_color(candidate_channel), base)
                {
                    rank += 1;
                }
            }
        }
        rank
    }

    fn pane_dataset_count_for_channel(
        &self,
        channel_index: usize,
        pane_index: usize,
        pane_count: usize,
    ) -> usize {
        if !self.channel_in_scope_pane(channel_index, pane_index, pane_count) {
            return 0;
        }
        let mut count = usize::from(self.visible.get(channel_index).copied().unwrap_or(false));
        for dataset in &self.imported_datasets {
            if dataset.visible.get(channel_index).copied().unwrap_or(false) {
                count += 1;
            }
        }
        count
    }

    fn pane_has_dataset_comparison(
        &self,
        selections: &PanePlotSelections,
        pane_index: usize,
        pane_count: usize,
    ) -> bool {
        selections.primary.iter().any(|(_, channel_index)| {
            self.pane_dataset_count_for_channel(*channel_index, pane_index, pane_count) > 1
        }) || selections.imported.iter().any(|channels| {
            channels.iter().any(|(_, channel_index)| {
                self.pane_dataset_count_for_channel(*channel_index, pane_index, pane_count) > 1
            })
        })
    }

    fn pane_visible_series_count(selections: &PanePlotSelections) -> usize {
        selections.primary.len()
            + selections.imported.iter().map(Vec::len).sum::<usize>()
            + selections.derived.len()
    }

    fn plot_legend_name(
        &self,
        dataset_index: usize,
        channel_name: &str,
        show_dataset_prefix: bool,
        suffix: Option<&str>,
    ) -> String {
        let dataset_label = show_dataset_prefix.then(|| self.dataset_short_label(dataset_index));
        Self::plot_legend_name_text(dataset_label.as_deref(), channel_name, suffix)
    }

    fn plot_legend_name_text(
        dataset_label: Option<&str>,
        channel_name: &str,
        suffix: Option<&str>,
    ) -> String {
        let base = if let Some(dataset_label) = dataset_label {
            format!("{dataset_label}: {channel_name}")
        } else {
            channel_name.to_owned()
        };
        match suffix {
            Some(suffix) if !suffix.is_empty() => format!("{base} {suffix}"),
            _ => base,
        }
    }

    fn plot_envelope_legend_name(
        &self,
        dataset_index: usize,
        channel_name: &str,
        show_dataset_prefix: bool,
        is_low_envelope: bool,
    ) -> Option<String> {
        let dataset_label = show_dataset_prefix.then(|| self.dataset_short_label(dataset_index));
        Self::plot_envelope_legend_name_text(
            dataset_label.as_deref(),
            channel_name,
            is_low_envelope,
        )
    }

    fn plot_envelope_legend_name_text(
        dataset_label: Option<&str>,
        channel_name: &str,
        is_low_envelope: bool,
    ) -> Option<String> {
        (!is_low_envelope).then(|| Self::plot_legend_name_text(dataset_label, channel_name, None))
    }

    fn plot_channel_color(
        &self,
        channel_index: usize,
        dataset_index: usize,
        pane_index: usize,
        pane_count: usize,
    ) -> Color32 {
        let base = self.channel_color(channel_index);
        let collision_rank =
            self.plot_color_collision_rank(channel_index, dataset_index, pane_index, pane_count);
        Self::plot_color_with_collision_rank(base, channel_index, dataset_index, collision_rank)
    }

    fn sidebar_channel_color(&self, channel_index: usize, dataset_index: usize) -> Color32 {
        let pane_count = self.scope_pane_count();
        let pane_index = self.channel_scope_pane(channel_index, pane_count);
        self.plot_channel_color(channel_index, dataset_index, pane_index, pane_count)
    }

    fn color_swatch(ui: &mut egui::Ui, color: Color32) -> egui::Response {
        let size = egui::vec2(48.0, ui.spacing().interact_size.y);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        if ui.is_rect_visible(rect) {
            let rounding = egui::Rounding::same(3.0);
            ui.painter().rect_filled(rect.shrink(2.0), rounding, color);
            ui.painter().rect_stroke(
                rect.shrink(2.0),
                rounding,
                Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
            );
        }
        response
    }

    fn channel_line_width(&self, index: usize) -> f32 {
        self.line_widths
            .get(index)
            .copied()
            .unwrap_or(DEFAULT_CHANNEL_LINE_WIDTH)
            .clamp(MIN_CHANNEL_LINE_WIDTH, MAX_CHANNEL_LINE_WIDTH)
    }

    fn channel_line_pattern(&self, index: usize) -> ChannelLinePattern {
        self.line_patterns
            .get(index)
            .copied()
            .unwrap_or(ChannelLinePattern::Solid)
    }

    fn dataset_line_pattern(&self, dataset_index: usize) -> ChannelLinePattern {
        if dataset_index == 0 {
            self.line_patterns
                .first()
                .copied()
                .unwrap_or(ChannelLinePattern::Solid)
        } else {
            self.imported_datasets
                .get(dataset_index - 1)
                .map(|dataset| dataset.line_pattern)
                .unwrap_or(ChannelLinePattern::Dashed)
        }
    }

    fn set_dataset_line_pattern(&mut self, dataset_index: usize, pattern: ChannelLinePattern) {
        if dataset_index == 0 {
            if self.line_patterns.iter().any(|current| *current != pattern) {
                self.line_patterns.fill(pattern);
            }
        } else if let Some(dataset) = self.imported_datasets.get_mut(dataset_index - 1) {
            if dataset.line_pattern != pattern {
                dataset.line_pattern = pattern;
            }
        }
    }

    fn sanitize_channel_scale(scale: f32) -> f32 {
        if scale.is_finite() {
            scale.clamp(MIN_CHANNEL_SCALE, MAX_CHANNEL_SCALE)
        } else {
            DEFAULT_CHANNEL_SCALE
        }
    }

    fn channel_scale(&self, index: usize) -> f32 {
        self.channel_scales
            .get(index)
            .copied()
            .map(Self::sanitize_channel_scale)
            .unwrap_or(DEFAULT_CHANNEL_SCALE)
    }

    fn scaled_value(&self, index: usize, value: f32) -> f64 {
        value as f64 * self.channel_scale(index) as f64
    }

    fn scaled_min_max(&self, index: usize, min: f32, max: f32) -> (f64, f64) {
        Self::scaled_min_max_with_scale(min, max, self.channel_scale(index) as f64)
    }

    fn scaled_min_max_with_scale(min: f32, max: f32, scale: f64) -> (f64, f64) {
        let a = min as f64 * scale;
        let b = max as f64 * scale;
        if a <= b {
            (a, b)
        } else {
            (b, a)
        }
    }

    #[allow(dead_code)]
    fn scaled_samples(&self, index: usize, samples: &[f32]) -> Vec<f32> {
        let scale = self.channel_scale(index);
        if (scale - DEFAULT_CHANNEL_SCALE).abs() <= f32::EPSILON {
            samples.to_vec()
        } else {
            samples.iter().map(|sample| *sample * scale).collect()
        }
    }

    fn samples_look_digital(samples: &[f32]) -> bool {
        let mut unique = Vec::<i32>::new();
        for &sample in samples.iter().filter(|sample| sample.is_finite()) {
            let rounded = sample.round();
            if (sample - rounded).abs() > 0.001 {
                return false;
            }
            let value = rounded as i32;
            if !(0..=16).contains(&value) {
                return false;
            }
            if !unique.contains(&value) {
                unique.push(value);
                if unique.len() > 8 {
                    return false;
                }
            }
        }
        !unique.is_empty()
    }

    fn set_channel_scale(&mut self, index: usize, scale: f32) {
        if let Some(current) = self.channel_scales.get_mut(index) {
            let next = Self::sanitize_channel_scale(scale);
            if (*current - next).abs() > f32::EPSILON {
                *current = next;
                self.y_min = None;
                self.y_max = None;
                self.measurement_cache = None;
                self.derived_curve_cache = None;
                self.derived_measurement_cache = None;
                self.fft_results.clear();
                self.needs_fft_reload = true;
                self.needs_plot_reload = true;
                self.needs_compare_plot_reload = true;
                self.needs_derived_reload = true;
            }
        }
    }

    fn multiply_channel_scale(&mut self, index: usize, factor: f32) {
        let current = self.channel_scale(index);
        self.set_channel_scale(index, current * factor);
    }

    fn visible_line_width(&self, index: usize) -> f32 {
        let base = self.channel_line_width(index);
        if self.hovered_channel == Some(index) {
            (base + 2.4).max(4.0)
        } else {
            base
        }
    }

    fn compare_line_width(&self, index: usize) -> f32 {
        let base = (self.channel_line_width(index) * 0.85).max(MIN_CHANNEL_LINE_WIDTH);
        if self.hovered_channel == Some(index) {
            (base + 1.8).max(3.2)
        } else {
            base
        }
    }

    // Compatibility helper for long explanatory text and transitional messages.
    // New short UI labels should be added to UiText and accessed through self.t(...).
    fn tr(&self, zh: &'static str, en: &'static str) -> &'static str {
        match self.language {
            Language::Zh => {
                if zh.is_empty() {
                    en
                } else {
                    zh
                }
            }
            Language::En => en,
        }
    }

    fn t(&self, text: UiText) -> &'static str {
        text.get(self.language)
    }

    fn icon_label(icon: &str, label: &str) -> String {
        let icon = match icon {
            "\u{E8E5}" => "+",
            "\u{EDE1}" => "^",
            "\u{E91B}" => "□",
            "\u{E823}" => "≡",
            "\u{E74D}" => "×",
            "\u{E80A}" => "▦",
            "\u{E890}" => "◎",
            "\u{E72C}" => "↺",
            "\u{E9A6}" => "↔",
            "\u{E9D2}" => "Y",
            "\u{E713}" => "⚙",
            "\u{E897}" => "?",
            "\u{E783}" => "!",
            _ => icon,
        };
        format!("{icon} {label}")
    }

    fn fixed_grid_label(
        ui: &mut egui::Ui,
        width: f32,
        text: impl Into<egui::WidgetText>,
        right_aligned: bool,
        truncate: bool,
    ) -> egui::Response {
        let mut label = egui::Label::new(text);
        if truncate {
            label = label.truncate(true);
        }
        let layout = if right_aligned {
            egui::Layout::right_to_left(egui::Align::Center)
        } else {
            egui::Layout::left_to_right(egui::Align::Center)
        };
        ui.allocate_ui_with_layout(
            egui::vec2(width, ui.spacing().interact_size.y),
            layout,
            |ui| ui.add(label),
        )
        .inner
    }

    fn error_banner(&mut self, ctx: &egui::Context) {
        let Some(error) = self.last_error.clone() else {
            return;
        };
        let mut dismiss = false;
        egui::TopBottomPanel::top("error_banner")
            .resizable(false)
            .show(ctx, |ui| {
                let fill = if self.theme_mode == ThemeMode::Dark {
                    Color32::from_rgb(92, 24, 28)
                } else {
                    Color32::from_rgb(255, 226, 226)
                };
                let stroke = Stroke::new(1.0, Color32::from_rgb(210, 56, 64));
                egui::Frame::none()
                    .fill(fill)
                    .stroke(stroke)
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(Self::icon_label("\u{E783}", self.t(UiText::Error)))
                                    .strong()
                                    .color(Color32::from_rgb(255, 80, 88)),
                            );
                            ui.separator();
                            ui.label(RichText::new(self.localized_error_message(&error)).strong());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button(self.t(UiText::Dismiss)).clicked() {
                                        dismiss = true;
                                    }
                                },
                            );
                        });
                    });
            });
        if dismiss {
            self.last_error = None;
        }
    }

    fn set_all_channels_visible(&mut self, visible: bool) {
        if self.visible.iter().any(|current| *current != visible) {
            self.visible.fill(visible);
            if visible {
                let active_pane = self.current_scope_pane();
                self.channel_panes.fill(active_pane);
            }
            self.needs_plot_reload = true;
            self.needs_compare_plot_reload = true;
            self.fft_results.clear();
            self.needs_fft_reload = true;
            self.measurement_cache = None;
            self.fft_channel_user_selected = false;
        }
    }

    fn set_channels_visible(&mut self, channels: &[usize], visible: bool) {
        let mut changed = false;
        let active_pane = self.current_scope_pane();
        for &channel in channels {
            if let Some(current) = self.visible.get_mut(channel) {
                if *current != visible {
                    *current = visible;
                    changed = true;
                }
                if visible {
                    if let Some(pane) = self.channel_panes.get_mut(channel) {
                        *pane = active_pane;
                    }
                }
            }
        }
        if changed {
            self.needs_plot_reload = true;
            self.needs_compare_plot_reload = true;
            self.fft_results.clear();
            self.needs_fft_reload = true;
            self.measurement_cache = None;
            self.fft_channel_user_selected = false;
        }
    }

    fn dataset_all_channels_visible(&self, dataset_index: usize) -> bool {
        let Some(meta) = self.dataset_meta_by_index(dataset_index) else {
            return false;
        };
        let mut has_channels = false;
        for channel in &meta.channels {
            has_channels = true;
            if !self.dataset_channel_visible(dataset_index, channel.index) {
                return false;
            }
        }
        has_channels
    }

    fn set_dataset_all_channels_visible(&mut self, dataset_index: usize, visible: bool) {
        let Some(meta) = self.dataset_meta_by_index(dataset_index) else {
            return;
        };
        let channels = meta
            .channels
            .iter()
            .map(|channel| channel.index)
            .collect::<Vec<_>>();
        if dataset_index == 0 {
            self.set_channels_visible(&channels, visible);
            return;
        }

        let mut changed = false;
        let active_pane = self.current_scope_pane();
        if let Some(dataset) = self.imported_datasets.get_mut(dataset_index - 1) {
            for channel in channels {
                if let Some(current) = dataset.visible.get_mut(channel) {
                    if *current != visible {
                        *current = visible;
                        changed = true;
                    }
                    if visible {
                        if let Some(pane) = self.channel_panes.get_mut(channel) {
                            *pane = active_pane;
                        }
                    }
                }
            }
        }

        if changed {
            self.needs_compare_plot_reload = true;
            self.measurement_cache = None;
            self.fft_results.clear();
            self.needs_fft_reload = true;
            self.fft_channel_user_selected = false;
        }
    }

    fn fit_to_cursors(&mut self) {
        self.zoom_to_range(self.cursor_a, self.cursor_b);
    }

    fn toggle_cursor_visibility(&mut self) {
        let show = !(self.show_cursor_a || self.show_cursor_b);
        self.show_cursor_a = show;
        self.show_cursor_b = show;
    }

    fn sidebar_visibility_after_shortcuts(
        channel_visible: bool,
        analysis_visible: bool,
        toggle_channel_panel: bool,
        toggle_analysis_panel: bool,
    ) -> (bool, bool, bool) {
        let mut channel_visible = channel_visible;
        let mut analysis_visible = analysis_visible;
        let mut handled = false;
        if toggle_channel_panel {
            channel_visible = !channel_visible;
            handled = true;
        }
        if toggle_analysis_panel {
            analysis_visible = !analysis_visible;
            handled = true;
        }
        (channel_visible, analysis_visible, handled)
    }

    fn export_history_shortcut_action(
        show_export_preview: bool,
        ctrl_z: bool,
        ctrl_y: bool,
        ctrl_down: bool,
        wants_keyboard_input: bool,
    ) -> Option<ExportHistoryAction> {
        if !show_export_preview || !ctrl_down || wants_keyboard_input {
            return None;
        }
        if ctrl_z {
            Some(ExportHistoryAction::Undo)
        } else if ctrl_y {
            Some(ExportHistoryAction::Redo)
        } else {
            None
        }
    }

    fn export_tool_shortcut(
        show_export_preview: bool,
        ctrl_down: bool,
        wants_keyboard_input: bool,
        key_1: bool,
        key_2: bool,
        key_3: bool,
        key_4: bool,
        key_5: bool,
        key_6: bool,
    ) -> Option<ExportPreviewTool> {
        if !show_export_preview || !ctrl_down || wants_keyboard_input {
            return None;
        }
        if key_1 {
            Some(ExportPreviewTool::Rectangle)
        } else if key_2 {
            Some(ExportPreviewTool::Arrow)
        } else if key_3 {
            Some(ExportPreviewTool::Text)
        } else if key_4 {
            Some(ExportPreviewTool::Brush)
        } else if key_5 {
            Some(ExportPreviewTool::Eraser)
        } else if key_6 {
            Some(ExportPreviewTool::Select)
        } else {
            None
        }
    }

    fn export_arrow_head_shortcut(
        show_export_preview: bool,
        tab_pressed: bool,
        wants_keyboard_input: bool,
    ) -> bool {
        show_export_preview && tab_pressed && !wants_keyboard_input
    }

    fn export_annotation_keyboard_action(
        show_export_preview: bool,
        wants_keyboard_input: bool,
        delete_pressed: bool,
        left_pressed: bool,
        right_pressed: bool,
        up_pressed: bool,
        down_pressed: bool,
        shift_down: bool,
    ) -> Option<ExportAnnotationKeyboardAction> {
        if !show_export_preview || wants_keyboard_input {
            return None;
        }
        if delete_pressed {
            return Some(ExportAnnotationKeyboardAction::Delete);
        }
        let step = if shift_down { 10 } else { 1 };
        let mut delta = [0, 0];
        if left_pressed {
            delta[0] -= step;
        }
        if right_pressed {
            delta[0] += step;
        }
        if up_pressed {
            delta[1] -= step;
        }
        if down_pressed {
            delta[1] += step;
        }
        (delta != [0, 0]).then_some(ExportAnnotationKeyboardAction::Nudge(delta))
    }

    fn clamp_canvas_point(point: CanvasPoint, canvas_size: [usize; 2]) -> CanvasPoint {
        let max_x = canvas_size[0].saturating_sub(1) as i32;
        let max_y = canvas_size[1].saturating_sub(1) as i32;
        CanvasPoint::new(point.x.clamp(0, max_x), point.y.clamp(0, max_y))
    }

    fn translate_point(
        point: CanvasPoint,
        delta: [i32; 2],
        canvas_size: [usize; 2],
    ) -> CanvasPoint {
        Self::clamp_canvas_point(
            CanvasPoint::new(point.x + delta[0], point.y + delta[1]),
            canvas_size,
        )
    }

    fn constrained_delta_for_points(
        points: impl Iterator<Item = CanvasPoint>,
        delta: [i32; 2],
        canvas_size: [usize; 2],
    ) -> [i32; 2] {
        let max_x = canvas_size[0].saturating_sub(1) as i32;
        let max_y = canvas_size[1].saturating_sub(1) as i32;
        let mut dx = delta[0];
        let mut dy = delta[1];
        for point in points {
            dx = dx.clamp(-point.x, max_x - point.x);
            dy = dy.clamp(-point.y, max_y - point.y);
        }
        [dx, dy]
    }

    #[allow(clippy::too_many_arguments)]
    fn nudge_export_annotation_selection(
        selection: ExportAnnotationSelection,
        delta: [i32; 2],
        canvas_size: [usize; 2],
        label_positions: &mut [Option<[i32; 2]>],
        text_annotations: &mut [ExportTextAnnotation],
        arrow_annotations: &mut [ExportArrowAnnotation],
        rectangle_annotations: &mut [ExportRectangleAnnotation],
        ink_strokes: &mut [ExportInkStroke],
    ) -> bool {
        if delta == [0, 0] {
            return false;
        }
        match selection {
            ExportAnnotationSelection::VariableLabel(index) => {
                let Some(Some(position)) = label_positions.get_mut(index) else {
                    return false;
                };
                let next = Self::translate_point(
                    CanvasPoint::new(position[0], position[1]),
                    delta,
                    canvas_size,
                );
                *position = [next.x, next.y];
                true
            }
            ExportAnnotationSelection::Text(index) => {
                let Some(annotation) = text_annotations.get_mut(index) else {
                    return false;
                };
                annotation.position =
                    Self::translate_point(annotation.position, delta, canvas_size);
                true
            }
            ExportAnnotationSelection::Arrow(index) => {
                let Some(annotation) = arrow_annotations.get_mut(index) else {
                    return false;
                };
                let delta = Self::constrained_delta_for_points(
                    [annotation.start, annotation.end].into_iter(),
                    delta,
                    canvas_size,
                );
                annotation.start =
                    CanvasPoint::new(annotation.start.x + delta[0], annotation.start.y + delta[1]);
                annotation.end =
                    CanvasPoint::new(annotation.end.x + delta[0], annotation.end.y + delta[1]);
                true
            }
            ExportAnnotationSelection::Rectangle(index) => {
                let Some(annotation) = rectangle_annotations.get_mut(index) else {
                    return false;
                };
                let top_left = CanvasPoint::new(annotation.rect.left, annotation.rect.top);
                let bottom_right = CanvasPoint::new(annotation.rect.right, annotation.rect.bottom);
                let delta = Self::constrained_delta_for_points(
                    [top_left, bottom_right].into_iter(),
                    delta,
                    canvas_size,
                );
                annotation.rect.left += delta[0];
                annotation.rect.right += delta[0];
                annotation.rect.top += delta[1];
                annotation.rect.bottom += delta[1];
                true
            }
            ExportAnnotationSelection::Ink(index) => {
                let Some(stroke) = ink_strokes.get_mut(index) else {
                    return false;
                };
                let delta = Self::constrained_delta_for_points(
                    stroke.points.iter().copied(),
                    delta,
                    canvas_size,
                );
                for point in &mut stroke.points {
                    point.x += delta[0];
                    point.y += delta[1];
                }
                true
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn delete_export_annotation_selection(
        selection: ExportAnnotationSelection,
        label_positions: &mut Vec<Option<[i32; 2]>>,
        text_annotations: &mut Vec<ExportTextAnnotation>,
        arrow_annotations: &mut Vec<ExportArrowAnnotation>,
        rectangle_annotations: &mut Vec<ExportRectangleAnnotation>,
        ink_strokes: &mut Vec<ExportInkStroke>,
    ) -> bool {
        match selection {
            ExportAnnotationSelection::VariableLabel(index) => {
                if index >= label_positions.len() {
                    return false;
                }
                label_positions.remove(index);
                true
            }
            ExportAnnotationSelection::Text(index) => {
                if index >= text_annotations.len() {
                    return false;
                }
                text_annotations.remove(index);
                true
            }
            ExportAnnotationSelection::Arrow(index) => {
                if index >= arrow_annotations.len() {
                    return false;
                }
                arrow_annotations.remove(index);
                true
            }
            ExportAnnotationSelection::Rectangle(index) => {
                if index >= rectangle_annotations.len() {
                    return false;
                }
                rectangle_annotations.remove(index);
                true
            }
            ExportAnnotationSelection::Ink(index) => {
                if index >= ink_strokes.len() {
                    return false;
                }
                ink_strokes.remove(index);
                true
            }
        }
    }

    fn apply_export_annotation_keyboard_action(
        &mut self,
        action: ExportAnnotationKeyboardAction,
    ) -> bool {
        let Some(selection) = self.export_annotation_selection else {
            return false;
        };
        let before = self.export_preview_state();
        let changed = match action {
            ExportAnnotationKeyboardAction::Delete => self.delete_selected_export_annotation(),
            ExportAnnotationKeyboardAction::Nudge(delta) => {
                self.nudge_selected_export_annotation(selection, delta)
            }
        };
        if changed {
            self.push_export_preview_undo(before);
            self.mark_export_preview_dirty();
        }
        changed
    }

    fn delete_selected_export_annotation(&mut self) -> bool {
        let Some(selection) = self.export_annotation_selection else {
            return false;
        };
        let changed = match selection {
            ExportAnnotationSelection::VariableLabel(index) => {
                let labels =
                    self.current_export_curve_labels(&self.export_preview_plot_selections());
                let Some(label) = labels.get(index) else {
                    return false;
                };
                let mut selections = self.export_preview_plot_selections();
                if Self::remove_export_curve_label_source(&mut selections, label.source) {
                    self.export_preview_selections = Some(selections);
                    let labels =
                        self.current_export_curve_labels(&self.export_preview_plot_selections());
                    self.sync_export_label_overrides(&labels);
                    true
                } else {
                    false
                }
            }
            ExportAnnotationSelection::Text(_)
            | ExportAnnotationSelection::Arrow(_)
            | ExportAnnotationSelection::Rectangle(_)
            | ExportAnnotationSelection::Ink(_) => Self::delete_export_annotation_selection(
                selection,
                &mut self.export_label_positions,
                &mut self.export_text_annotations,
                &mut self.export_arrow_annotations,
                &mut self.export_rectangle_annotations,
                &mut self.export_ink_strokes,
            ),
        };
        if changed {
            self.export_annotation_selection = None;
            self.export_preview_edit_label_index = None;
            self.export_preview_edit_text_index = None;
        }
        changed
    }

    fn nudge_selected_export_annotation(
        &mut self,
        selection: ExportAnnotationSelection,
        delta: [i32; 2],
    ) -> bool {
        if let ExportAnnotationSelection::VariableLabel(index) = selection {
            if self
                .export_label_positions
                .get(index)
                .and_then(|position| *position)
                .is_none()
            {
                if let Some(placement) = self
                    .export_preview_label_layout
                    .iter()
                    .find(|placement| placement.label_index == index)
                {
                    self.set_export_label_canvas_position(
                        index,
                        [placement.label_rect[0] + 4, placement.label_rect[1] + 3],
                    );
                }
            }
        }
        Self::nudge_export_annotation_selection(
            selection,
            delta,
            self.export_preview_size,
            &mut self.export_label_positions,
            &mut self.export_text_annotations,
            &mut self.export_arrow_annotations,
            &mut self.export_rectangle_annotations,
            &mut self.export_ink_strokes,
        )
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (
            reset_view,
            fit_cursors,
            toggle_cursors,
            select_all,
            select_none,
            toggle_channel_panel,
            toggle_analysis_panel,
            ctrl_z,
            ctrl_y,
            ctrl_down,
            key_1,
            key_2,
            key_3,
            key_4,
            key_5,
            key_6,
            key_tab,
            key_delete,
            key_left,
            key_right,
            key_up,
            key_down,
            shift_down,
        ) = ctx.input(|input| {
            (
                self.shortcuts.reset_view.pressed(input),
                self.shortcuts.fit_cursors.pressed(input),
                self.shortcuts.toggle_cursors.pressed(input),
                self.shortcuts.select_all.pressed(input),
                self.shortcuts.select_none.pressed(input),
                self.shortcuts.toggle_channel_panel.pressed(input),
                self.shortcuts.toggle_analysis_panel.pressed(input),
                input.key_pressed(egui::Key::Z),
                input.key_pressed(egui::Key::Y),
                input.modifiers.ctrl,
                input.key_pressed(egui::Key::Num1),
                input.key_pressed(egui::Key::Num2),
                input.key_pressed(egui::Key::Num3),
                input.key_pressed(egui::Key::Num4),
                input.key_pressed(egui::Key::Num5),
                input.key_pressed(egui::Key::Num6),
                input.key_pressed(egui::Key::Tab),
                input.key_pressed(egui::Key::Delete),
                input.key_pressed(egui::Key::ArrowLeft),
                input.key_pressed(egui::Key::ArrowRight),
                input.key_pressed(egui::Key::ArrowUp),
                input.key_pressed(egui::Key::ArrowDown),
                input.modifiers.shift,
            )
        });

        let (show_channel_panel, show_analysis_panel, sidebar_handled) =
            Self::sidebar_visibility_after_shortcuts(
                self.show_channel_panel,
                self.show_analysis_panel,
                toggle_channel_panel,
                toggle_analysis_panel,
            );
        self.show_channel_panel = show_channel_panel;
        self.show_analysis_panel = show_analysis_panel;
        if sidebar_handled {
            ctx.request_repaint();
        }

        let wants_keyboard_input = ctx.wants_keyboard_input();
        if let Some(action) = Self::export_history_shortcut_action(
            self.show_export_preview,
            ctrl_z,
            ctrl_y,
            ctrl_down,
            wants_keyboard_input,
        ) {
            match action {
                ExportHistoryAction::Undo => self.undo_export_preview_edit(),
                ExportHistoryAction::Redo => self.redo_export_preview_edit(),
            }
            ctx.request_repaint();
            return;
        }

        if let Some(tool) = Self::export_tool_shortcut(
            self.show_export_preview,
            ctrl_down,
            wants_keyboard_input,
            key_1,
            key_2,
            key_3,
            key_4,
            key_5,
            key_6,
        ) {
            self.export_preview_tool = tool;
            ctx.request_repaint();
            return;
        }

        if Self::export_arrow_head_shortcut(self.show_export_preview, key_tab, wants_keyboard_input)
        {
            let before = self.export_preview_state();
            self.export_manual_arrow_head_enabled = !self.export_manual_arrow_head_enabled;
            self.push_export_preview_undo(before);
            self.mark_export_preview_dirty();
            ctx.request_repaint();
            return;
        }

        if let Some(action) = Self::export_annotation_keyboard_action(
            self.show_export_preview,
            wants_keyboard_input,
            key_delete,
            key_left,
            key_right,
            key_up,
            key_down,
            shift_down,
        ) {
            if self.apply_export_annotation_keyboard_action(action) {
                ctx.request_repaint();
                return;
            }
        }

        if wants_keyboard_input {
            return;
        }

        let mut handled = false;
        if reset_view {
            self.reset_view();
            handled = true;
        }
        if fit_cursors {
            self.fit_to_cursors();
            handled = true;
        }
        if toggle_cursors {
            self.toggle_cursor_visibility();
            handled = true;
        }
        if select_all {
            self.set_all_channels_visible(true);
            handled = true;
        }
        if select_none {
            self.set_all_channels_visible(false);
            handled = true;
        }
        if handled {
            ctx.request_repaint();
        }
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        ctx.set_visuals(self.theme_mode.visuals());
    }

    fn localized_error_message(&self, message: &str) -> String {
        if self.language != Language::Zh {
            return message.to_owned();
        }
        if message.contains('\n') {
            return message
                .lines()
                .map(|line| self.localized_error_message(line))
                .collect::<Vec<_>>()
                .join("\n");
        }
        if message == "Data import is already running." {
            return "数据正在导入中，请稍后再试。".to_owned();
        }
        if message == "PLL/dq0 worker panicked." {
            return "PLL/dq0 计算任务异常退出。".to_owned();
        }
        if message == "PLL/dq0 only supports analog channels." {
            return "PLL/dq0 仅支持模拟量通道。".to_owned();
        }
        if message == "PLL/dq0 needs six source channel reads." {
            return "PLL/dq0 需要读取六路源通道。".to_owned();
        }
        match message {
            "Import data first." => "请先导入数据。".to_owned(),
            "Select at least one waveform data file." => "请选择至少一个波形数据文件。".to_owned(),
            "No data loaded." => "未加载数据。".to_owned(),
            "Import worker panicked." => "导入任务异常退出。".to_owned(),
            "Plot worker panicked." => "绘图任务异常退出。".to_owned(),
            "Compare plot worker panicked." => "对比绘图任务异常退出。".to_owned(),
            "FFT worker panicked." => "FFT 任务异常退出。".to_owned(),
            "Measurement worker panicked." => "测量任务异常退出。".to_owned(),
            "PLL/dq0 measurement worker panicked." => "PLL/dq0 测量任务异常退出。".to_owned(),
            "Sequence worker panicked." => "正负序任务异常退出。".to_owned(),
            "Selected channel is digital, so FFT is skipped." => {
                "所选通道是数字量，已跳过 FFT。".to_owned()
            }
            "PLL/dq0 inputs must use distinct A/B/C channels." => {
                "PLL/dq0 的 A/B/C 通道不能重复。".to_owned()
            }
            "Sequence analysis needs three channels." => "正负序分析需要三个通道。".to_owned(),
            "Sequence analysis only supports analog channels." => {
                "正负序分析仅支持模拟量通道。".to_owned()
            }
            "Sequence analysis needs at least 16 samples in the cursor range." => {
                "正负序分析需要光标区间内至少 16 个采样点。".to_owned()
            }
            "Could not distinguish analog and digital local CSV files." => {
                "无法区分本地 CSV 中的模拟量文件和数字量文件，请确认文件名或表头包含 ADATA/DDATA、analog/digital 等标识。".to_owned()
            }
            "Import a primary dataset first, or select multiple CSV files at once." => {
                "请先导入主数据，或一次选择多个 CSV 文件。".to_owned()
            }
            _ => self.localized_unknown_error_message(message),
        }
    }

    fn localized_unknown_error_message(&self, message: &str) -> String {
        if message.contains("Unsupported waveform file extension") {
            "不支持该波形文件格式，请选择 .csv 或 .dat 文件。".to_owned()
        } else if message.contains("Empty CSV") {
            "CSV 文件为空，缺少表头或数据。".to_owned()
        } else if message.contains("Could not distinguish analog and digital local CSV files") {
            format!(
                "无法区分本地 CSV 中的模拟量文件和数字量文件，请确认文件名或表头包含 ADATA/DDATA、analog/digital 等标识。详情：{message}"
            )
        } else if message.contains("No such file")
            || message.contains("cannot find")
            || message.contains("系统找不到")
        {
            format!("文件无法打开，请检查路径是否存在。详情：{message}")
        } else if message.contains("worker panicked") {
            format!("后台任务异常退出，软件已拦截并继续运行。详情：{message}")
        } else {
            format!("操作失败，请检查数据文件或参数。详情：{message}")
        }
    }

    fn scope_layout_menu(&mut self, ui: &mut egui::Ui) {
        ui.strong(self.t(UiText::ScopeLayout));
        ui.horizontal(|ui| {
            ui.label(self.t(UiText::Rows));
            ui.add(
                egui::Slider::new(&mut self.scope_layout_rows, 1..=MAX_SCOPE_LAYOUT_ROWS)
                    .show_value(true),
            );
        });
        ui.horizontal(|ui| {
            ui.label(self.t(UiText::Columns));
            ui.add(
                egui::Slider::new(&mut self.scope_layout_cols, 1..=MAX_SCOPE_LAYOUT_COLS)
                    .show_value(true),
            );
        });
        self.scope_layout_rows = self.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS);
        self.scope_layout_cols = self.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS);
        self.active_scope_pane = self
            .active_scope_pane
            .min(self.scope_pane_count().saturating_sub(1));

        ui.separator();
        ui.label(format!(
            "{}: {}",
            self.t(UiText::ActivePane),
            self.current_scope_pane() + 1
        ));
        ui.label(self.t(UiText::PaneSelectHint));
        ui.separator();
        ui.label(self.t(UiText::QuickSelect));
        egui::Grid::new("scope_layout_picker")
            .spacing([2.0, 2.0])
            .show(ui, |ui| {
                for row in 1..=MAX_SCOPE_LAYOUT_ROWS {
                    for col in 1..=MAX_SCOPE_LAYOUT_COLS {
                        let active = row <= self.scope_layout_rows && col <= self.scope_layout_cols;
                        let fill = if active {
                            Color32::from_rgb(25, 130, 220)
                        } else {
                            ui.visuals().widgets.inactive.bg_fill
                        };
                        let response = ui.add_sized(
                            [24.0, 22.0],
                            egui::Button::new("")
                                .fill(fill)
                                .stroke(Stroke::new(1.0, Color32::GRAY)),
                        );
                        if response.clicked() {
                            self.scope_layout_rows = row;
                            self.scope_layout_cols = col;
                            self.active_scope_pane = self
                                .active_scope_pane
                                .min(self.scope_pane_count().saturating_sub(1));
                        }
                    }
                    ui.end_row();
                }
            });
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} x {}",
                self.scope_layout_rows, self.scope_layout_cols
            ));
            if ui.button(self.t(UiText::Single)).clicked() {
                self.scope_layout_rows = 1;
                self.scope_layout_cols = 1;
                self.active_scope_pane = 0;
            }
        });
    }

    fn dataset_groups_ui(
        &mut self,
        ui: &mut egui::Ui,
        filter_terms: &[String],
        hovered_channel: &mut Option<usize>,
    ) {
        let delete_group_label = Self::icon_label("\u{E74D}", self.t(UiText::DeleteDataset));
        let mut delete_group = None;
        let primary_header = self.dataset_label(0);
        let primary_header_display = Self::sidebar_header_label(
            &primary_header,
            &self.dataset_short_label(0),
            ui.available_width(),
        );
        let primary_meta = self.meta().cloned();
        let primary_response = egui::CollapsingHeader::new(primary_header_display)
            .id_source(("dataset_group", 0usize))
            .default_open(true)
            .show(ui, |ui| {
                if let Some(meta) = &primary_meta {
                    self.channel_sections_ui(ui, 0, meta, filter_terms, hovered_channel);
                }
            });
        let mut delete_primary = false;
        primary_response
            .header_response
            .clone()
            .on_hover_text(primary_header);
        primary_response.header_response.context_menu(|ui| {
            self.dataset_context_menu(ui, 0, delete_group_label.as_str(), &mut delete_primary);
        });
        if delete_primary {
            delete_group = Some(0);
        }

        for index in 0..self.imported_datasets.len() {
            let header = self.dataset_label(index + 1);
            let header_display = Self::sidebar_header_label(
                &header,
                &self.dataset_short_label(index + 1),
                ui.available_width(),
            );
            let dataset_meta = self.imported_meta(index).cloned();
            let response = egui::CollapsingHeader::new(header_display)
                .id_source(("dataset_group", index + 1))
                .default_open(false)
                .show(ui, |ui| {
                    if let Some(meta) = &dataset_meta {
                        self.channel_sections_ui(
                            ui,
                            index + 1,
                            meta,
                            filter_terms,
                            hovered_channel,
                        );
                    }
                });
            let mut delete_this = false;
            response.header_response.clone().on_hover_text(header);
            response.header_response.context_menu(|ui| {
                self.dataset_context_menu(
                    ui,
                    index + 1,
                    delete_group_label.as_str(),
                    &mut delete_this,
                );
            });
            if delete_this {
                delete_group = Some(index + 1);
            }
        }

        if let Some(dataset_index) = delete_group {
            self.delete_dataset_group(dataset_index);
        }
    }

    fn dataset_context_menu(
        &mut self,
        ui: &mut egui::Ui,
        dataset_index: usize,
        delete_group_label: &str,
        delete_requested: &mut bool,
    ) {
        ui.strong(self.t(UiText::DatasetSettings));
        ui.horizontal(|ui| {
            ui.label(self.t(UiText::DatasetName));
            if dataset_index == 0 {
                ui.text_edit_singleline(&mut self.primary_dataset_name);
            } else if let Some(dataset) = self.imported_datasets.get_mut(dataset_index - 1) {
                ui.text_edit_singleline(&mut dataset.display_name);
            }
        });
        ui.separator();

        let mut all_visible = self.dataset_all_channels_visible(dataset_index);
        if ui
            .checkbox(&mut all_visible, self.t(UiText::SelectAllChannels))
            .changed()
        {
            self.set_dataset_all_channels_visible(dataset_index, all_visible);
        }
        ui.separator();

        ui.horizontal(|ui| {
            ui.label(self.t(UiText::LineStyle));
            let mut pattern = self.dataset_line_pattern(dataset_index);
            egui::ComboBox::from_id_source(("dataset_line_pattern", dataset_index))
                .selected_text(pattern.label(self.language))
                .show_ui(ui, |ui| {
                    for candidate in ChannelLinePattern::ALL {
                        ui.selectable_value(
                            &mut pattern,
                            candidate,
                            candidate.label(self.language),
                        );
                    }
                });
            self.set_dataset_line_pattern(dataset_index, pattern);
        });
        ui.separator();

        let mut selected_for_delete = self.dataset_selected_for_delete(dataset_index);
        if ui
            .checkbox(&mut selected_for_delete, self.t(UiText::MarkForDeletion))
            .changed()
        {
            self.set_dataset_selected_for_delete(dataset_index, selected_for_delete);
        }
        if self.any_dataset_selected_for_delete()
            && ui.button(self.t(UiText::DeleteSelectedDatasets)).clicked()
        {
            self.delete_selected_datasets();
            ui.close_menu();
        }
        if ui.button(delete_group_label).clicked() {
            *delete_requested = true;
            ui.close_menu();
        }
    }

    fn channel_sections_ui(
        &mut self,
        ui: &mut egui::Ui,
        dataset_index: usize,
        meta: &DatasetMeta,
        filter_terms: &[String],
        hovered_channel: &mut Option<usize>,
    ) {
        let source_label = self.t(UiText::Source);
        let mut entries = Vec::new();
        let source_kind = self.dataset_kind_by_index(dataset_index);

        for channel in &meta.channels {
            let has_visibility = if dataset_index == 0 {
                channel.index < self.visible.len()
            } else {
                self.imported_datasets
                    .get(dataset_index - 1)
                    .is_some_and(|dataset| channel.index < dataset.visible.len())
            };
            if !has_visibility {
                continue;
            }
            let display_name = self.dataset_channel_name(dataset_index, channel);
            let searchable = format!("{} {}", display_name, channel.name).to_lowercase();
            if !filter_terms.iter().all(|term| searchable.contains(term)) {
                continue;
            }
            entries.push((channel.clone(), display_name));
        }

        if source_kind != Some(SourceKind::Dat) {
            let mut analog_entries = Vec::new();
            let mut digital_entries = Vec::new();
            for (channel, display_name) in entries {
                if Self::channel_is_digital(source_kind, &channel) {
                    digital_entries.push((channel, display_name));
                } else {
                    analog_entries.push((channel, display_name));
                }
            }
            self.channel_section_ui(
                ui,
                dataset_index,
                self.t(UiText::Analog),
                true,
                &analog_entries,
                source_label,
                hovered_channel,
            );
            self.channel_section_ui(
                ui,
                dataset_index,
                self.t(UiText::Digital),
                false,
                &digital_entries,
                source_label,
                hovered_channel,
            );
            return;
        }

        if entries.is_empty() {
            ui.label(self.t(UiText::NoMatchingChannels));
            return;
        }
        for (channel, display_name) in &entries {
            if self.channel_row_ui(ui, dataset_index, channel, display_name, source_label) {
                *hovered_channel = Some(channel.index);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn channel_section_ui(
        &mut self,
        ui: &mut egui::Ui,
        dataset_index: usize,
        title: &str,
        default_open: bool,
        entries: &[(crate::data::ChannelMeta, String)],
        source_label: &str,
        hovered_channel: &mut Option<usize>,
    ) {
        let selected_count = entries
            .iter()
            .filter(|(channel, _)| self.dataset_channel_visible(dataset_index, channel.index))
            .count();
        let header = format!("{title} ({selected_count}/{})", entries.len());
        egui::CollapsingHeader::new(header)
            .id_source(("channel_kind", dataset_index, title))
            .default_open(default_open)
            .show(ui, |ui| {
                if entries.is_empty() {
                    ui.label(self.t(UiText::NoMatchingChannels));
                    return;
                }
                for (channel, display_name) in entries {
                    if self.channel_row_ui(ui, dataset_index, channel, display_name, source_label) {
                        *hovered_channel = Some(channel.index);
                    }
                }
            });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().button_padding.x = 6.0;
        ui.horizontal_wrapped(|ui| {
            self.workspace_selector(ui);
            ui.separator();
            if self.live.workspace_mode == scope_analyzer::live::state::WorkspaceMode::Live {
                self.live_toolbar(ui);
                return;
            }
            ui.menu_button(
                Self::icon_label("\u{E8E5}", self.t(UiText::ImportData)),
                |ui| {
                    if self.import_worker.is_some() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(self.t(UiText::ImportingData));
                        });
                        ui.separator();
                    }
                    if ui
                        .button(Self::icon_label(
                            "\u{E8E5}",
                            self.t(UiText::SelectDataFiles),
                        ))
                        .clicked()
                    {
                        let filter_name = self.t(UiText::WaveformCsv);
                        if let Some(paths) = rfd::FileDialog::new()
                            .add_filter(filter_name, &["csv", "dat", "scope"])
                            .pick_files()
                        {
                            self.import_data_files(paths);
                        }
                        ui.close_menu();
                    }

                    ui.separator();
                    ui.menu_button(
                        Self::icon_label("\u{E823}", self.t(UiText::RecentFiles)),
                        |ui| {
                            if self.recent_files.is_empty() {
                                ui.label(self.t(UiText::NoRecentFiles));
                            } else {
                                let recent_files = self.recent_files.clone();
                                for path in recent_files {
                                    let label = Self::recent_file_label(&path);
                                    let full_path = path.display().to_string();
                                    if path.exists() {
                                        if ui.button(label).on_hover_text(full_path).clicked() {
                                            self.import_data_files(vec![path.clone()]);
                                            ui.close_menu();
                                        }
                                    } else {
                                        ui.label(
                                            RichText::new(format!(
                                                "{} {}",
                                                label,
                                                self.t(UiText::MissingFile)
                                            ))
                                            .color(Color32::GRAY),
                                        )
                                        .on_hover_text(full_path);
                                    }
                                }
                                ui.separator();
                                if ui
                                    .button(Self::icon_label(
                                        "\u{E74D}",
                                        self.t(UiText::ClearRecentFiles),
                                    ))
                                    .clicked()
                                {
                                    self.clear_recent_files();
                                    ui.close_menu();
                                }
                            }
                        },
                    );
                },
            );
            if self.source.is_some() {
                let export_title = Self::icon_label("\u{EDE1}", self.t(UiText::Export));
                ui.menu_button(export_title, |ui| {
                    ui.menu_button(
                        Self::icon_label("\u{EDE1}", self.t(UiText::ExportData)),
                        |ui| {
                            let dataset_indices =
                                (0..=self.imported_datasets.len()).collect::<Vec<_>>();
                            self.export_data_menu(ui, &dataset_indices);
                        },
                    );
                    if ui
                        .button(Self::icon_label(
                            "\u{E91B}",
                            self.t(UiText::ExportWaveformImage),
                        ))
                        .clicked()
                    {
                        self.export_waveform_png();
                        ui.close_menu();
                    }
                    if ui
                        .button(Self::icon_label("\u{E8B5}", self.t(UiText::BatchExport)))
                        .clicked()
                    {
                        self.open_batch_waveform_export();
                        ui.close_menu();
                    }
                });
            }
            ui.menu_button(Self::icon_label("\u{E80A}", self.t(UiText::Layout)), |ui| {
                self.scope_layout_menu(ui)
            });
            ui.menu_button(Self::icon_label("\u{E890}", self.t(UiText::View)), |ui| {
                if ui
                    .button(Self::icon_label("\u{E72C}", self.t(UiText::ResetView)))
                    .clicked()
                {
                    self.reset_view();
                    ui.close_menu();
                }
                if ui
                    .button(Self::icon_label("\u{E9A6}", self.t(UiText::FitCursors)))
                    .clicked()
                {
                    self.fit_to_cursors();
                    ui.close_menu();
                }
                if ui
                    .button(Self::icon_label("\u{E9D2}", self.t(UiText::AutoY)))
                    .clicked()
                {
                    self.auto_fit_y_axis();
                    ui.close_menu();
                }
            });
            ui.menu_button(Self::icon_label("\u{E713}", self.t(UiText::Config)), |ui| {
                ui.menu_button(self.t(UiText::NameConfig), |ui| {
                    if ui
                        .button(Self::icon_label("\u{E8B5}", self.t(UiText::ImportNames)))
                        .clicked()
                    {
                        self.import_config();
                        ui.close_menu();
                    }
                    if ui
                        .button(Self::icon_label("\u{EDE1}", self.t(UiText::ExportNames)))
                        .clicked()
                    {
                        self.export_config();
                        ui.close_menu();
                    }
                    self.config_recent_menu(ui, ConfigSection::Names);
                });
                ui.menu_button(self.t(UiText::DisplayConfig), |ui| {
                    if ui
                        .button(Self::icon_label("\u{E8B5}", self.t(UiText::ImportAction)))
                        .clicked()
                    {
                        self.import_display_config();
                        ui.close_menu();
                    }
                    if ui
                        .button(Self::icon_label("\u{EDE1}", self.t(UiText::ExportAction)))
                        .clicked()
                    {
                        self.export_display_config();
                        ui.close_menu();
                    }
                    self.config_recent_menu(ui, ConfigSection::Display);
                });
                ui.menu_button(self.t(UiText::ShortcutConfig), |ui| {
                    if ui
                        .button(Self::icon_label("\u{E8B5}", self.t(UiText::ImportAction)))
                        .clicked()
                    {
                        self.import_shortcut_config();
                        ui.close_menu();
                    }
                    if ui
                        .button(Self::icon_label("\u{EDE1}", self.t(UiText::ExportAction)))
                        .clicked()
                    {
                        self.export_shortcut_config();
                        ui.close_menu();
                    }
                    self.config_recent_menu(ui, ConfigSection::Shortcut);
                });
                ui.menu_button(self.t(UiText::DatasetConfig), |ui| {
                    if ui
                        .button(Self::icon_label("\u{E8B5}", self.t(UiText::ImportAction)))
                        .clicked()
                    {
                        self.import_dataset_config();
                        ui.close_menu();
                    }
                    if ui
                        .button(Self::icon_label("\u{EDE1}", self.t(UiText::ExportAction)))
                        .clicked()
                    {
                        self.export_dataset_config();
                        ui.close_menu();
                    }
                    self.config_recent_menu(ui, ConfigSection::Dataset);
                });
            });
            if ui
                .button(Self::icon_label("\u{E713}", self.t(UiText::Options)))
                .clicked()
            {
                self.show_options = true;
            }
            ui.menu_button(Self::icon_label("\u{E897}", self.t(UiText::Help)), |ui| {
                if ui.button(self.t(UiText::Help)).clicked() {
                    self.show_help = true;
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(self.t(UiText::CopyDiagnostics)).clicked() {
                    self.copy_diagnostics_to_clipboard(ui.ctx());
                    ui.close_menu();
                }
                if ui.button(self.t(UiText::OpenLogDirectory)).clicked() {
                    self.open_log_directory();
                    ui.close_menu();
                }
            });
            if self.import_worker.is_some() {
                ui.separator();
                ui.spinner();
                ui.label(self.t(UiText::ImportingData));
            } else if let Some(status) = &self.import_status {
                ui.separator();
                ui.label(status);
            }
            if let Some(meta) = self.meta() {
                ui.separator();
                if self.language == Language::Zh {
                    let imported_status = if self.imported_datasets.is_empty() {
                        String::new()
                    } else {
                        format!(" | 附加 {} 组", self.imported_datasets.len())
                    };
                    ui.label(format!(
                        "主数据: {} | {} 采样点 | {:.3}s | 数据 {:.1} Hz | FFT Fs {:.1} Hz{}",
                        meta.source_name,
                        meta.sample_count,
                        meta.duration(),
                        meta.nominal_sample_rate_hz,
                        self.sample_rate_hz,
                        imported_status
                    ));
                } else {
                    let imported_status = if self.imported_datasets.is_empty() {
                        String::new()
                    } else {
                        format!(" | {} extra", self.imported_datasets.len())
                    };
                    ui.label(format!(
                        "Primary: {} | {} samples | {:.3}s | data {:.1} Hz | FFT Fs {:.1} Hz{}",
                        meta.source_name,
                        meta.sample_count,
                        meta.duration(),
                        meta.nominal_sample_rate_hz,
                        self.sample_rate_hz,
                        imported_status
                    ));
                }
            }
        });
    }

    fn help_window(&mut self, ctx: &egui::Context) {
        let title = self.t(UiText::Help);
        let language = self.language;
        let copy_label = self.t(UiText::CopyDiagnostics);
        let open_log_label = self.t(UiText::OpenLogDirectory);
        let diagnostics_label = self.t(UiText::Diagnostics);
        let mut copy_diagnostics = false;
        let mut open_log_directory = false;
        egui::Window::new(title)
            .open(&mut self.show_help)
            .default_width(720.0)
            .default_height(620.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.strong(diagnostics_label);
                    if ui.button(copy_label).clicked() {
                        copy_diagnostics = true;
                    }
                    if ui.button(open_log_label).clicked() {
                        open_log_directory = true;
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if language == Language::Zh {
                        ui.heading("示波器分析器");
                        ui.label("离线波形分析工具，支持通道勾选、示波器式缩放、光标测量、FFT 和 THD 分析。");
                        ui.label("使用“导入数据”菜单选择一个或多个 CSV 或 DAT 数据文件。第一个文件作为主数据，后续文件作为附加数据叠加显示，Content CSV 会自动识别。");

                        ui.separator();
                        ui.heading("支持的数据格式");
                        ui.label("可一次导入多个数据文件。CSV 会根据表头自动识别；DAT 会按二进制文件头和采样帧解码。");
                        ui.label("主数据决定通道列表、显示名、颜色、线宽、测量和 FFT；附加数据按相同通道序号叠加显示。");
                        ui.strong("云端 Content CSV");
                        ui.label("第一行是 Content，后续每行是十六进制报文。每条报文解析为两个采样点，每个采样点包含 30 个模拟量通道和 30 个数字/状态通道。");
                        ui.label("云端 Content CSV 没有显式时间列，软件使用“选项”里的 FFT Fs 生成时间轴，默认 1000 Hz。");
                        ui.add_space(6.0);
                        ui.strong("本地/数值 CSV");
                        ui.label("第一列是秒级时间，后续列为通道值，最多读取 128 个数值通道。文件打开时只建立索引，绘图按当前视窗读取原始采样或 min/max 摘要。");
                        ui.monospace("time,CH1,CH2,CH3\n0.000000,0.0,0.1,0.2\n0.000010,0.1,0.2,0.3");
                        ui.add_space(6.0);
                        ui.strong("本地 ADATA/DDATA CSV");
                        ui.label("文件名包含 ADATA 和 DDATA 的本地 CSV 会自动识别并配对，可单个选择其中一个文件，也可批量选择多个文件。配对优先使用文件名时间戳，其次使用名称关系和修改时间。");
                        ui.label("ADATA 作为模拟量通道，DDATA 作为数字量通道。两类文件第一列序号会自动剔除；DDATA 去掉序号列后的前三个 bit 通道会合并为一个数字量通道：bit0 + 2*bit1 + 4*bit2。");
                        ui.label("该规则只作用于这种本地 ADATA/DDATA 格式，不影响云端 Content CSV、标准数值 CSV 或 DAT 文件。变量名会尽量与云端 CSV 保持一致。");
                        ui.add_space(6.0);
                        ui.strong("二进制 DAT");
                        ui.label("DAT 文件从二进制文件头读取采样率和通道名，每帧按小端 int16 通道值解码。");
                        ui.label("大文件不会一次性全部载入内存。缩小时绘制最小/最大包络，放大后读取原始采样点。");

                        ui.separator();
                        ui.heading("波形操作");
                        ui.label("导入数据：选择一个或多个波形文件，第一个作为主数据，后续作为附加数据。导出数据可选择全部导出或只导出光标内区间，并保存为标准 CSV、DATA CSV、TSV 或 JSON。");
                        ui.label("最近文件：成功导入的文件会自动加入列表，可在顶部“导入数据”菜单中重新打开或清空。");
                        ui.label("布局：设置示波器行数和列数。点击某个子窗口后，再在左侧勾选变量，变量会放入当前子窗口。");
                        ui.label("选项：设置 FFT Fs、谐波基准频率、滚轮缩放灵敏度、界面语言、主题和快捷键。");
                        ui.label("鼠标滚轮：以鼠标位置为中心缩放纵轴幅值范围。Ctrl + 滚轮/触摸板滚动：缩放横轴时间范围。");
                        ui.label("左侧变量栏：按数据组、模拟量/数字量和变量名组织。右键数据组可全选/全不选并配置线型；双击变量名可编辑显示名；右键变量名可配置变比。");
                        ui.label("变量名导入/导出：只保存和恢复显示名，不覆盖通道可见性、颜色、线宽、倍率、FFT 设置、语言、主题或快捷键。");
                        ui.label("侧栏：左右侧栏都可以拖动调整宽度。窗口变窄时变量名会自动缩短或隐藏，右侧分析选择项会按空间自动横向或纵向排列。");
                        ui.label("默认快捷键：Ctrl+B 显示/隐藏左侧变量栏，Ctrl+Alt+B 显示/隐藏右侧分析栏。快捷键在输入框获得焦点时仍可生效。");
                        ui.label("左键点击波形：移动最近的光标。左键拖拽：框选时间范围并放大。右键点击：打开光标菜单。右键拖拽：平移当前视图。");
                        ui.label("放置光标 X1/X2：显示红色虚线预览，左键确认，Esc 取消。隐藏/显示光标只切换可见状态，不改变位置和测量结果。");
                        ui.label("适配光标：缩放到 X1/X2 两个光标之间。快捷键可在选项中配置。");
                        ui.label("测量：右侧表格显示 X1-X2 区间内已选通道的 Y1、Y2、dY、最大值和最小值，结果使用通道变比后的值。");

                        ui.separator();
                        ui.heading("波形图片与 DOCX 导出");
                        ui.label("导出波形图片 PNG：从顶部菜单打开当前示波器视图的导出标注台，不会直接保存文件。预览中保留当前波形、光标、X 轴坐标、变量标注和可选的光标数据表。");
                        ui.label("导出标注台提供选择、文字、箭头、矩形/椭圆、画笔、橡皮、撤销和恢复。变量名箭头会自动吸附曲线，拖动变量名时箭头会跟随变化。");
                        ui.label("文字工具会进入放置模式，点击预览图选择文字位置后再编辑内容；箭头工具可自由标注故障点、现象和说明；画笔可手写标注；橡皮可擦除画笔笔迹。");
                        ui.label("撤销/恢复：拖动变量名、移动锚点、改变量名、改箭头样式、添加文字、添加箭头、添加矩形/椭圆、画笔和橡皮操作都可撤销或恢复。");
                        ui.label("箭头与标注：可在预览窗口设置箭头大小、实线/虚线/点线/粗箭头/双线箭头、标注颜色、变量名字号和字体。变量名和文字标注字号支持 1-8 档。变量箭头尖默认吸附并指向曲线。");
                        ui.label("导出设置：在选项中可设置分辨率、DPI、导出子窗口范围、时间范围和是否显示光标数据表；DPI 支持预设，也可手动输入数值。导出风格固定为示波器截图风格。");
                        ui.label("批量导出波形 / DOCX：在选项中打开批量导出窗口，可按当前视图、X1-X2 区间或手动时间窗口批量保存 PNG，或把多张已标注波形图写入同一个 DOCX。");
                        ui.label("DOCX 使用内置简洁模板，光标数据表可在导出设置中选择显示或隐藏。第一版不导入外部模板。");

                        ui.separator();
                        ui.heading("FFT 和 THD");
                        ui.label("右侧分析入口统一选择数据组、分析通道和三相 A/B/C 通道；测量、FFT 和正负序都使用同一个入口和 X1-X2 区间。");
                        ui.label("谐波分析前会去除直流均值并使用 Hann 窗，按目标谐波频率做相量投影和窗增益补偿。");
                        ui.label("谐波基准频率可在选项中设置，默认 50 Hz。谐波表显示 0 次直流量以及 1-10 次谐波的幅值、相位、相对基波比例和 THD。");
                        ui.label("THD 为 2 次及以上谐波平方和开根号后除以 1 次谐波幅值。");

                        ui.separator();
                        ui.heading("启动、诊断和云桌面");
                        ui.label("默认启动器会依次尝试多个渲染器，并把启动过程写入程序目录下的 ScopeAnalyzer-startup.log；如果程序目录不可写，会写入系统临时目录。");
                        ui.label("打包版本会优先使用随程序携带的 ANGLE DLL，以提高虚拟显卡和云桌面的 OpenGL 兼容性。若普通渲染器都失败，会最后尝试隔离在 mesa 目录下的 Mesa/llvmpipe 软件 OpenGL。");
                        ui.label("打包目录只保留默认启动脚本和 Start-ScopeAnalyzer-Mesa.bat。云桌面建议先用默认启动；若仍无法启动，再使用 Mesa/llvmpipe 兜底。");
                        ui.label("Help 菜单中的“复制诊断信息”会复制版本、渲染器环境、日志路径、当前数据和最近错误；“打开日志目录”可直接查看启动日志和崩溃日志。");
                    } else {
                        ui.heading("Scope Analyzer");
                        ui.label("Windows offline waveform analyzer with channel selection, oscilloscope-style zooming, cursor measurement, FFT, and THD analysis.");
                        ui.label("Use the Import Data menu to select one or more CSV or DAT data files. The first file becomes the primary dataset, and later files are overlaid as extra datasets. Content CSV files are detected automatically.");

                        ui.separator();
                        ui.heading("Supported Data Formats");
                        ui.label("Use the Import Data menu to load data. You can select multiple data files at once. CSV files are detected from the header; DAT files are decoded from their binary header and sample frames.");
                        ui.label("The primary dataset controls the channel list, display names, colors, line widths, measurements, and FFT. Extra datasets are overlaid as dashed lines by matching channel index.");
                        ui.strong("Cloud Content CSV");
                        ui.label("The first row is Content. Each following row is a hexadecimal record. Each record is decoded into two samples. Each sample contains 30 analog channels plus 30 digital/status channels.");
                        ui.label("Cloud Content CSV has no explicit time column, so FFT Fs in Options is used to generate the time axis. The default is 1000 Hz.");
                        ui.add_space(6.0);
                        ui.strong("Local / Numeric CSV");
                        ui.label("The first column is time in seconds. Remaining columns are channel values. Up to 128 numeric channels are loaded. The file is indexed in blocks and the plot reads only the current view or min/max summaries.");
                        ui.monospace("time,CH1,CH2,CH3\n0.000000,0.0,0.1,0.2\n0.000010,0.1,0.2,0.3");
                        ui.add_space(6.0);
                        ui.strong("Local ADATA/DDATA CSV");
                        ui.label("Local CSV files whose names contain ADATA and DDATA are detected and paired automatically. You can select one side of a pair or select many files in one batch. Pairing prefers filename timestamps, then name relationships and file modification time.");
                        ui.label("ADATA is treated as analog channels and DDATA as digital channels. The first sequence column is removed from both files. After the sequence column is removed, the first three DDATA bit channels are merged as bit0 + 2*bit1 + 4*bit2.");
                        ui.label("This rule only applies to the local ADATA/DDATA format and does not affect cloud Content CSV, standard numeric CSV, or DAT files. Variable names are kept aligned with the cloud CSV names where possible.");
                        ui.add_space(6.0);
                        ui.strong("Binary DAT");
                        ui.label("DAT files are read from their binary header. The header supplies the sample rate and channel names; each frame is decoded as little-endian int16 channel values.");
                        ui.label("Large files are not loaded fully into memory. Zoomed-out views draw min/max envelopes; zoomed-in views read raw samples.");

                        ui.separator();
                        ui.heading("Waveform Controls");
                        ui.label("Import Data opens one or more waveform files. The first file becomes the primary dataset; later files are added as extra datasets. Export Data can save either the full time range or only the X1-X2 cursor range.");
                        ui.label("Layout sets the scope pane rows and columns. Click a pane, then select channels in the sidebar to add them to that pane.");
                        ui.label("Options controls FFT Fs, harmonic base frequency, wheel zoom sensitivity, language, theme, and shortcuts.");
                        ui.label("Mouse wheel zooms the vertical axis around the pointer. Ctrl + wheel or touchpad scroll zooms the time axis.");
                        ui.label("The left channel list is organized by dataset, analog/digital type, and channel name. Right-click datasets or channels for related settings.");
                        ui.label("Name import/export only stores display names; it does not overwrite visibility, color, line width, scale, FFT, language, theme, or shortcut settings.");
                        ui.label("Sidebars can be resized by dragging. When the window is narrow, channel names shorten or hide automatically, and analysis selectors switch between horizontal and vertical layouts based on available space.");
                        ui.label("Default sidebar shortcuts follow VS Code style: Ctrl+B toggles the left channel sidebar, and Ctrl+Alt+B toggles the right analysis sidebar. These shortcuts still work while text inputs are focused.");
                        ui.label("Left-click the plot to move the nearest cursor. Drag with the left button to zoom a time range. Right-click for the cursor menu; right-drag pans the current view.");

                        ui.separator();
                        ui.heading("Waveform Image and Document Export");
                        ui.label("Export Waveform PNG opens an annotation workspace for the current scope view instead of saving immediately. The preview keeps waveform traces, cursors, X-axis cursor labels, variable annotations, and the optional cursor data table.");
                        ui.label("The export workspace provides select, text, arrow, rectangle, brush, eraser, undo, and redo tools. Variable arrows stay attached to curves and update as labels move.");
                        ui.label("Text enters placement mode; Arrow creates free notes such as fault points and observations; Brush draws freehand marks; Eraser removes brush strokes.");
                        ui.label("Undo and redo cover label moves, anchor moves, label edits, arrow style changes, text notes, manual arrows, rectangles, brush strokes, and eraser actions.");
                        ui.label("Arrows and labels can be configured in the preview: arrow size, solid/dashed/dotted/thick/double arrows, annotation color, variable label size, and label font. Variable labels and text notes support size levels 1-8. Arrow tips stay snapped to the curve by default.");
                        ui.label("Export settings in Options control resolution, DPI, scope pane range, time range, and the cursor data table. DPI can use presets or a custom typed value. The image style is fixed to the oscilloscope screenshot style.");
                        ui.label("Batch Export Waveform / DOCX can save PNGs for the current view, X1-X2, or manual time windows, or write multiple annotated waveform images into one DOCX.");
                        ui.label("DOCX export uses the built-in simple template. Cursor data tables can be shown or hidden from export settings. External templates are not imported in this version.");

                        ui.separator();
                        ui.heading("FFT and THD");
                        ui.label("The Analysis section selects the dataset, analysis channel, and A/B/C channels; Measurements, FFT, and Sequence use that shared input and the X1-X2 range.");
                        ui.label("Harmonic analysis removes the DC mean and applies a Hann window before calculating harmonic phasors.");
                        ui.label("The harmonic table shows the DC component plus the 1st through 10th harmonic amplitude, phase, ratio to fundamental, and THD.");

                        ui.separator();
                        ui.heading("Startup, Diagnostics, and Cloud Desktops");
                        ui.label("The default launcher tries several renderers in order and writes startup details to ScopeAnalyzer-startup.log beside the executable, or to the system temp directory if the install directory is not writable.");
                        ui.label("Packaged builds prefer bundled ANGLE DLLs to improve OpenGL compatibility on virtual display adapters and cloud desktops. If the normal renderers all fail, the launcher finally tries the isolated Mesa/llvmpipe software OpenGL helper in the mesa directory.");
                        ui.label("Packaged folders keep only the default launcher and Start-ScopeAnalyzer-Mesa.bat. Use the default launcher first on cloud desktops; use Mesa/llvmpipe only as the manual fallback.");
                        ui.label("Copy Diagnostics in the Help menu copies the version, renderer environment, log paths, active data, and last error. Open Log Directory opens the folder containing startup and crash logs.");
                    }
                });
            });
        if copy_diagnostics {
            self.copy_diagnostics_to_clipboard(ctx);
        }
        if open_log_directory {
            self.open_log_directory();
        }
    }

    fn options_window(&mut self, ctx: &egui::Context) {
        let title = self.t(UiText::Options);
        let mut open = self.show_options;
        egui::Window::new(title)
            .open(&mut open)
            .default_width(420.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading(self.t(UiText::Interaction));
                ui.horizontal(|ui| {
                    ui.label(self.t(UiText::UiLanguage));
                    egui::ComboBox::from_id_source("language_select")
                        .selected_text(self.language.label())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.language, Language::Zh, "中文");
                            ui.selectable_value(&mut self.language, Language::En, "English");
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(self.t(UiText::Theme));
                    let previous_theme = self.theme_mode;
                    egui::ComboBox::from_id_source("theme_select")
                        .selected_text(self.theme_mode.label(self.language))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.theme_mode,
                                ThemeMode::Light,
                                ThemeMode::Light.label(self.language),
                            );
                            ui.selectable_value(
                                &mut self.theme_mode,
                                ThemeMode::Dark,
                                ThemeMode::Dark.label(self.language),
                            );
                        });
                    if self.theme_mode != previous_theme {
                        self.apply_theme(ui.ctx());
                    }
                });
                ui.separator();
                ui.heading(self.t(UiText::ImageExportLabels));
                let mut batch_export_checked = self.show_batch_export;
                let batch_export_label = self.t(UiText::BatchExportWaveformDocx);
                if ui
                    .checkbox(&mut batch_export_checked, batch_export_label)
                    .changed()
                {
                    if batch_export_checked {
                        self.open_batch_waveform_export();
                    } else {
                        self.show_batch_export = false;
                    }
                }
                ui.horizontal(|ui| {
                    ui.label(self.t(UiText::ExportQuality));
                    let _ = self.export_dpi_controls(ui, "export_dpi");
                });
                ui.horizontal(|ui| {
                    ui.label(self.t(UiText::ImageFormat));
                    egui::ComboBox::from_id_source("export_image_format")
                        .selected_text(self.export_image_format.label())
                        .show_ui(ui, |ui| {
                            for format in ExportImageFormat::ALL {
                                ui.selectable_value(
                                    &mut self.export_image_format,
                                    format,
                                    format.label(),
                                );
                            }
                        });
                });
                let annotations_label = self.t(UiText::ShowAnnotations);
                if ui
                    .checkbox(&mut self.export_annotations_visible, annotations_label)
                    .changed()
                    && !self.export_annotations_visible
                {
                    self.clear_export_annotation_interaction_state();
                    self.mark_export_preview_dirty();
                }
                let cursor_table_label = self.t(UiText::CursorDataTable);
                let auto_label_text = self.t(UiText::AutoVariableLabels);
                ui.add_enabled_ui(self.export_annotations_visible, |ui| {
                    ui.checkbox(
                        &mut self.export_auto_variable_labels_enabled,
                        auto_label_text,
                    );
                });
                ui.checkbox(&mut self.export_cursor_table_enabled, cursor_table_label);
                ui.horizontal(|ui| {
                    ui.label(self.t(UiText::Pane));
                    egui::ComboBox::from_id_source("export_pane_scope")
                        .selected_text(self.export_pane_scope.label(self.language))
                        .show_ui(ui, |ui| {
                            for scope in ExportPaneScope::ALL {
                                ui.selectable_value(
                                    &mut self.export_pane_scope,
                                    scope,
                                    scope.label(self.language),
                                );
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(self.t(UiText::TimeRange));
                    egui::ComboBox::from_id_source("export_time_range_mode")
                        .selected_text(self.export_time_range_mode.label(self.language))
                        .show_ui(ui, |ui| {
                            for mode in ExportTimeRangeMode::ALL {
                                ui.selectable_value(
                                    &mut self.export_time_range_mode,
                                    mode,
                                    mode.label(self.language),
                                );
                            }
                        });
                });
                if self.export_time_range_mode == ExportTimeRangeMode::Manual {
                    ui.horizontal(|ui| {
                        ui.label(self.t(UiText::ManualRange));
                        ui.add(
                            egui::DragValue::new(&mut self.export_manual_start)
                                .speed(0.0001)
                                .prefix("X0 "),
                        );
                        ui.add(
                            egui::DragValue::new(&mut self.export_manual_end)
                                .speed(0.0001)
                                .prefix("X1 "),
                        );
                    });
                }
                let arrow_size_label = self.t(UiText::ArrowSize);
                ui.add(
                    egui::Slider::new(
                        &mut self.export_arrow_size,
                        MIN_EXPORT_ARROW_SIZE..=MAX_EXPORT_ARROW_SIZE,
                    )
                    .text(arrow_size_label),
                );
                self.export_arrow_size = self
                    .export_arrow_size
                    .clamp(MIN_EXPORT_ARROW_SIZE, MAX_EXPORT_ARROW_SIZE);
                ui.horizontal(|ui| {
                    ui.label(self.t(UiText::ArrowLabelColor));
                    egui::ComboBox::from_id_source("export_arrow_color_style")
                        .selected_text(self.export_arrow_color_style.label(self.language))
                        .show_ui(ui, |ui| {
                            for style in ExportArrowColorStyle::ALL {
                                ui.selectable_value(
                                    &mut self.export_arrow_color_style,
                                    style,
                                    style.label(self.language),
                                );
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(self.t(UiText::ArrowLineStyle));
                    let _ = self.export_arrow_style_controls(ui, "export_arrow_line_style");
                });
                if self.export_arrow_color_style == ExportArrowColorStyle::Custom {
                    ui.horizontal(|ui| {
                        ui.label(self.t(UiText::CustomColor));
                        egui::color_picker::color_edit_button_srgba(
                            ui,
                            &mut self.export_arrow_custom_color,
                            egui::color_picker::Alpha::Opaque,
                        );
                    });
                }
                let label_size_label = self.t(UiText::VariableNameSize);
                ui.add(
                    egui::Slider::new(
                        &mut self.export_label_scale,
                        MIN_EXPORT_LABEL_SCALE..=MAX_EXPORT_LABEL_SCALE,
                    )
                    .text(label_size_label),
                );
                self.export_label_scale = self
                    .export_label_scale
                    .clamp(MIN_EXPORT_LABEL_SCALE, MAX_EXPORT_LABEL_SCALE);
                ui.horizontal(|ui| {
                    ui.label(self.t(UiText::VariableNameFont));
                    egui::ComboBox::from_id_source("export_label_font_style")
                        .selected_text(self.export_label_font_style.label(self.language))
                        .show_ui(ui, |ui| {
                            for style in ExportLabelFontStyle::ALL {
                                ui.selectable_value(
                                    &mut self.export_label_font_style,
                                    style,
                                    style.label(self.language),
                                );
                            }
                        });
                });
                if ui.button(self.t(UiText::ResetExportLabelStyle)).clicked() {
                    self.export_style_preset = ExportStylePreset::Screenshot;
                    self.export_arrow_size = DEFAULT_EXPORT_ARROW_SIZE;
                    self.export_arrow_color_style = ExportArrowColorStyle::Curve;
                    self.export_arrow_line_style = ExportArrowLineStyle::Solid;
                    self.export_manual_arrow_head_enabled = true;
                    self.export_arrow_custom_color = Color32::from_rgb(20, 96, 180);
                    self.export_label_scale = DEFAULT_EXPORT_LABEL_SCALE;
                    self.export_label_font_style = ExportLabelFontStyle::Regular;
                    self.export_resolution = DEFAULT_EXPORT_RESOLUTION;
                    self.export_dpi = ExportDpi::Dpi300;
                    self.export_dpi_value = 300;
                    self.export_image_format = ExportImageFormat::Png;
                    self.export_annotations_visible = true;
                    self.export_auto_variable_labels_enabled = false;
                    self.export_cursor_table_enabled = true;
                    self.export_pane_scope = ExportPaneScope::All;
                    self.export_time_range_mode = ExportTimeRangeMode::View;
                    self.export_manual_start = self.view_start;
                    self.export_manual_end = self.view_end;
                }
                ui.separator();
                let old_sample_rate = self.sample_rate_hz;
                let sample_rate_prefix = self.t(UiText::FftSampleRatePrefix);
                ui.add(
                    egui::DragValue::new(&mut self.sample_rate_hz)
                        .speed(10.0)
                        .clamp_range(1.0..=10_000_000.0)
                        .suffix(" Hz")
                        .prefix(sample_rate_prefix),
                );
                if (self.sample_rate_hz - old_sample_rate).abs() > f64::EPSILON {
                    self.sample_rate_hz = self.sample_rate_hz.clamp(1.0, 10_000_000.0);
                    self.derived_curve_cache = None;
                    self.derived_measurement_cache = None;
                    self.needs_derived_reload = true;
                    self.reload_cloud_with_current_sample_rate();
                }
                ui.label(self.t(UiText::FftSampleRateHint));
                let old_harmonic_base = self.harmonic_base_hz;
                let harmonic_base_prefix = self.t(UiText::HarmonicBasePrefix);
                ui.add(
                    egui::DragValue::new(&mut self.harmonic_base_hz)
                        .speed(1.0)
                        .clamp_range(0.001..=10_000_000.0)
                        .suffix(" Hz")
                        .prefix(harmonic_base_prefix),
                );
                if (self.harmonic_base_hz - old_harmonic_base).abs() > f64::EPSILON {
                    self.harmonic_base_hz = self.harmonic_base_hz.clamp(0.001, 10_000_000.0);
                    self.needs_fft_reload = true;
                    self.derived_curve_cache = None;
                    self.derived_measurement_cache = None;
                    self.needs_derived_reload = true;
                }
                ui.label(self.t(UiText::HarmonicBaseHint));
                ui.separator();
                ui.heading(self.t(UiText::PllDq0Title));
                let channel_options = self.fft_channel_options();
                if channel_options.len() >= 3 {
                    if !Self::valid_three_phase_selection(
                        self.pll_source_channels,
                        &channel_options,
                    ) {
                        self.pll_source_channels = self
                            .preferred_pll_source_channels(&channel_options)
                            .or_else(|| {
                                Self::default_sequence_channels_from_options(&channel_options)
                            })
                            .unwrap_or([0, 1, 2]);
                    }
                    let options = channel_options
                        .iter()
                        .map(|channel| {
                            (
                                *channel,
                                self.fft_channel_name(self.fft_dataset_index, *channel),
                                self.related_three_phase_channels_from_anchor(
                                    *channel,
                                    &channel_options,
                                ),
                            )
                        })
                        .collect::<Vec<_>>();
                    ui.label(self.t(UiText::PllSyncSource));
                    if Self::three_phase_channel_selectors_ui(
                        ui,
                        "options_pll_source_channel",
                        self.fft_dataset_index,
                        &options,
                        &mut self.pll_source_channels,
                    ) {
                        self.derived_curve_cache = None;
                        self.prepared_derived_curve_cache = PreparedPlotSeries::default();
                        self.derived_measurement_cache = None;
                        self.needs_derived_reload = true;
                    }
                } else {
                    ui.label(self.t(UiText::PllNeedsThreeAnalogChannels));
                }
                ui.label(self.t(UiText::PllUsesAnalysisDataset));
                ui.separator();
                ui.heading(self.t(UiText::TimeAxisSync));
                let previous_sync = self.sync_time_axes;
                let sync_axes_label = self.t(UiText::AlignDatasetTimeAxes);
                ui.checkbox(&mut self.sync_time_axes, sync_axes_label);
                if self.sync_time_axes != previous_sync {
                    self.needs_compare_plot_reload = true;
                    self.needs_derived_reload = true;
                    self.fft_results.clear();
                    self.needs_fft_reload = true;
                }
                let time_sync_options = self.primary_time_sync_channel_options();
                if time_sync_options.len() >= 3 {
                    if !Self::valid_three_phase_selection(
                        self.time_sync_source_channels,
                        &time_sync_options,
                    ) || (!self.time_sync_source_channels_user_selected
                        && self
                            .preferred_time_sync_source_channels(&time_sync_options)
                            .is_some())
                    {
                        self.time_sync_source_channels = self
                            .preferred_time_sync_source_channels(&time_sync_options)
                            .or_else(|| {
                                Self::default_sequence_channels_from_options(&time_sync_options)
                            })
                            .unwrap_or([0, 1, 2]);
                        self.time_sync_source_channels_user_selected = false;
                    }
                    let options = time_sync_options
                        .iter()
                        .map(|channel| {
                            (
                                *channel,
                                self.fft_channel_name(0, *channel),
                                self.related_three_phase_channels_from_anchor_in_dataset(
                                    0,
                                    *channel,
                                    &time_sync_options,
                                ),
                            )
                        })
                        .collect::<Vec<_>>();
                    ui.label(self.t(UiText::TimeSyncSource));
                    if Self::three_phase_channel_selectors_ui(
                        ui,
                        "time_sync_source_channel",
                        0,
                        &options,
                        &mut self.time_sync_source_channels,
                    ) {
                        self.time_sync_source_channels_user_selected = true;
                        self.time_sync_status.clear();
                    }
                } else {
                    ui.label(self.t(UiText::TimeSyncNeedsMainThreeAnalogChannels));
                }
                ui.horizontal(|ui| {
                    if ui.button(self.t(UiText::SyncByPhase)).clicked() {
                        self.sync_time_axes_by_phase();
                    }
                    if ui.button(self.t(UiText::ClearSync)).clicked() {
                        self.clear_time_axis_sync();
                    }
                });
                ui.label(self.t(UiText::TimeSyncPhaseHint));
                if !self.time_sync_status.is_empty() {
                    ui.label(&self.time_sync_status);
                }
                for (index, dataset) in self.imported_datasets.iter().enumerate() {
                    ui.label(format!(
                        "{}: {:+.6}s",
                        self.dataset_label(index + 1),
                        dataset.time_offset
                    ));
                }
                ui.separator();
                let zoom_label = self.t(UiText::WheelZoomSensitivity);
                ui.add(
                    egui::Slider::new(
                        &mut self.wheel_zoom_sensitivity,
                        MIN_WHEEL_ZOOM_SENSITIVITY..=MAX_WHEEL_ZOOM_SENSITIVITY,
                    )
                    .text(zoom_label)
                    .logarithmic(false),
                );
                if self.language == Language::Zh {
                    ui.label(format!(
                        "当前: 每格滚轮 {:.1}%",
                        self.wheel_zoom_sensitivity * 100.0
                    ));
                } else {
                    ui.label(format!(
                        "Current: {:.1}% per wheel step",
                        self.wheel_zoom_sensitivity * 100.0
                    ));
                }
                if ui.button(self.t(UiText::ResetSensitivity)).clicked() {
                    self.wheel_zoom_sensitivity = DEFAULT_WHEEL_ZOOM_SENSITIVITY;
                }
                ui.separator();
                ui.heading(self.t(UiText::Shortcuts));
                let reset_view_label = self.t(UiText::ResetView);
                let fit_cursors_label = self.t(UiText::FitCursors);
                let toggle_cursors_label = self.t(UiText::ToggleCursors);
                let select_all_label = self.t(UiText::SelectAllChannels);
                let select_none_label = self.t(UiText::DeselectAllChannels);
                let toggle_channel_panel_label = self.t(UiText::ToggleLeftSidebar).to_owned();
                let toggle_analysis_panel_label = self.t(UiText::ToggleRightSidebar).to_owned();
                Self::shortcut_binding_ui(
                    ui,
                    "shortcut_reset_view",
                    reset_view_label,
                    &mut self.shortcuts.reset_view,
                );
                Self::shortcut_binding_ui(
                    ui,
                    "shortcut_fit_cursors",
                    fit_cursors_label,
                    &mut self.shortcuts.fit_cursors,
                );
                Self::shortcut_binding_ui(
                    ui,
                    "shortcut_toggle_cursors",
                    toggle_cursors_label,
                    &mut self.shortcuts.toggle_cursors,
                );
                Self::shortcut_binding_ui(
                    ui,
                    "shortcut_select_all",
                    select_all_label,
                    &mut self.shortcuts.select_all,
                );
                Self::shortcut_binding_ui(
                    ui,
                    "shortcut_select_none",
                    select_none_label,
                    &mut self.shortcuts.select_none,
                );
                Self::shortcut_binding_ui(
                    ui,
                    "shortcut_toggle_channel_panel",
                    &toggle_channel_panel_label,
                    &mut self.shortcuts.toggle_channel_panel,
                );
                Self::shortcut_binding_ui(
                    ui,
                    "shortcut_toggle_analysis_panel",
                    &toggle_analysis_panel_label,
                    &mut self.shortcuts.toggle_analysis_panel,
                );
                if ui.button(self.t(UiText::ResetShortcuts)).clicked() {
                    self.shortcuts = ShortcutConfig::default();
                }
                ui.separator();
                ui.label(self.t(UiText::MouseWheelZoomY));
                ui.label(self.t(UiText::CtrlWheelZoomX));
            });
        self.show_options = open;
    }
    fn shortcut_binding_ui(
        ui: &mut egui::Ui,
        id: &'static str,
        label: &str,
        binding: &mut ShortcutBinding,
    ) {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.checkbox(&mut binding.ctrl, "Ctrl");
            ui.checkbox(&mut binding.alt, "Alt");
            egui::ComboBox::from_id_source(id)
                .selected_text(binding.key.label())
                .show_ui(ui, |ui| {
                    for key in ShortcutKey::ALL {
                        ui.selectable_value(&mut binding.key, key, key.label());
                    }
                });
            ui.label(binding.label());
        });
    }

    fn wheel_zoom_factor(&self, scroll_delta: f32) -> f64 {
        if scroll_delta > 0.0 {
            1.0 - self.wheel_zoom_sensitivity
        } else {
            1.0 + self.wheel_zoom_sensitivity
        }
    }

    fn ctrl_zoom_factor(&self, zoom_delta: f32) -> f64 {
        (zoom_delta as f64).powf(-self.wheel_zoom_sensitivity * 4.0)
    }

    fn observe_layout_panel_widths(&mut self, channel_width: f32, analysis_width: f32) {
        let channel_changed = self
            .last_channel_panel_width
            .is_some_and(|last| (last - channel_width).abs() > 0.5);
        let analysis_changed = self
            .last_analysis_panel_width
            .is_some_and(|last| (last - analysis_width).abs() > 0.5);
        self.last_channel_panel_width = Some(channel_width);
        self.last_analysis_panel_width = Some(analysis_width);
        if channel_changed || analysis_changed {
            self.layout_resize_active_until = Some(Instant::now() + LAYOUT_RESIZE_ACTIVE_GRACE);
        }
    }

    fn sidebar_width_ranges(window_width: f32) -> SidebarWidthRanges {
        Self::sidebar_width_ranges_for_visibility(window_width, true, true)
    }

    fn sidebar_width_ranges_for_visibility(
        window_width: f32,
        channel_visible: bool,
        analysis_visible: bool,
    ) -> SidebarWidthRanges {
        let window_width = window_width.max(0.0);
        let channel_min = CHANNEL_PANEL_MIN_WIDTH;
        let analysis_min = ANALYSIS_PANEL_MIN_WIDTH;
        let minimum_sidebar_total = if channel_visible { channel_min } else { 0.0 }
            + if analysis_visible { analysis_min } else { 0.0 };
        let sidebar_budget = (window_width - MIN_CENTRAL_PANEL_WIDTH).max(minimum_sidebar_total);

        let channel_cap = if channel_visible {
            (window_width * MAX_CHANNEL_PANEL_FRACTION)
                .max(CHANNEL_PANEL_MAX_WIDTH)
                .max(channel_min)
        } else {
            channel_min
        };
        let analysis_cap = if analysis_visible {
            (window_width * MAX_ANALYSIS_PANEL_FRACTION)
                .max(ANALYSIS_PANEL_MAX_WIDTH)
                .max(analysis_min)
        } else {
            analysis_min
        };

        let (channel_max, analysis_max) = if channel_cap + analysis_cap <= sidebar_budget {
            (channel_cap, analysis_cap)
        } else {
            let extra_budget = (sidebar_budget - minimum_sidebar_total).max(0.0);
            let channel_extra = if channel_visible {
                (channel_cap - channel_min).max(0.0)
            } else {
                0.0
            };
            let analysis_extra = if analysis_visible {
                (analysis_cap - analysis_min).max(0.0)
            } else {
                0.0
            };
            let extra_total = channel_extra + analysis_extra;
            if extra_total <= f32::EPSILON {
                (channel_min, analysis_min)
            } else {
                (
                    channel_min + extra_budget * channel_extra / extra_total,
                    analysis_min + extra_budget * analysis_extra / extra_total,
                )
            }
        };

        SidebarWidthRanges {
            channel: channel_min..=channel_max.max(channel_min),
            analysis: analysis_min..=analysis_max.max(analysis_min),
        }
    }

    fn layout_resize_active(&self) -> bool {
        self.layout_resize_active_until
            .is_some_and(|until| Instant::now() < until)
    }

    fn has_zoom_delta(zoom_delta: f32) -> bool {
        (zoom_delta - 1.0).abs() > 0.001
    }

    fn pointer_zoom_factor(&self, scroll: f32, zoom_delta: f32) -> Option<f64> {
        if Self::has_zoom_delta(zoom_delta) {
            Some(self.ctrl_zoom_factor(zoom_delta))
        } else if scroll.abs() > 0.0 {
            Some(self.wheel_zoom_factor(scroll))
        } else {
            None
        }
    }

    fn auto_measure(times: &[f64], samples: &[f32]) -> Option<AutoMeasurement> {
        if times.len() < 2 || samples.len() < 2 {
            return None;
        }
        let sample_count = times.len().min(samples.len());
        let samples = &samples[..sample_count];
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;

        for &sample in samples {
            min = min.min(sample);
            max = max.max(sample);
        }

        if !min.is_finite() || !max.is_finite() {
            return None;
        }

        Some(AutoMeasurement {
            first: samples[0],
            last: samples[sample_count - 1],
            min,
            max,
        })
    }

    fn interpolated_y(points: &[PlotPoint], x: f64) -> Option<f64> {
        if points.is_empty() || !x.is_finite() {
            return None;
        }
        for point in points {
            if (point.x - x).abs() <= f64::EPSILON && point.y.is_finite() {
                return Some(point.y);
            }
        }
        for pair in points.windows(2) {
            let x0 = pair[0].x;
            let y0 = pair[0].y;
            let x1 = pair[1].x;
            let y1 = pair[1].y;
            if !x0.is_finite() || !x1.is_finite() || !y0.is_finite() || !y1.is_finite() {
                continue;
            }
            let min_x = x0.min(x1);
            let max_x = x0.max(x1);
            if x < min_x || x > max_x {
                continue;
            }
            if (x1 - x0).abs() <= f64::EPSILON {
                return Some(y0);
            }
            let ratio = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
            return Some(y0 + (y1 - y0) * ratio);
        }
        None
    }

    fn summary_cursor_y(
        &self,
        summary: &RangeSummary,
        out_index: usize,
        channel_index: usize,
        cursor_x: f64,
        time_offset: f64,
    ) -> Option<f64> {
        let mins = summary.min.get(out_index)?;
        let maxes = summary.max.get(out_index)?;
        for i in 0..summary.bin_start.len().min(summary.bin_end.len()) {
            let start = summary.bin_start[i] + time_offset;
            let end = summary.bin_end[i] + time_offset;
            if cursor_x < start || cursor_x > end {
                continue;
            }
            let min = *mins.get(i)?;
            let max = *maxes.get(i)?;
            let (scaled_min, scaled_max) = self.scaled_min_max(channel_index, min, max);
            let y = (scaled_min + scaled_max) * 0.5;
            return y.is_finite().then_some(y);
        }
        None
    }

    fn cursor_value_label(
        plot_ui: &mut PlotUi,
        x: f64,
        y: f64,
        label: String,
        color: Color32,
        anchor: egui::Align2,
    ) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        plot_ui.text(
            Text::new(
                PlotPoint::new(x, y),
                RichText::new(label)
                    .small()
                    .strong()
                    .color(color)
                    .background_color(Color32::from_white_alpha(220)),
            )
            .anchor(anchor),
        );
    }

    fn draw_preview_cursor_value_labels(
        &self,
        plot_ui: &mut PlotUi,
        cursor_x: Option<f64>,
        intersections: &[(f64, Color32)],
    ) {
        let Some(cursor_x) = cursor_x else {
            return;
        };
        for (index, (y, color)) in intersections.iter().enumerate() {
            let anchor = if index % 2 == 0 {
                egui::Align2::LEFT_BOTTOM
            } else {
                egui::Align2::LEFT_TOP
            };
            Self::cursor_value_label(plot_ui, cursor_x, *y, format!("{:.2}", y), *color, anchor);
        }
        if let [(first_y, _), (second_y, _)] = intersections {
            Self::cursor_value_label(
                plot_ui,
                cursor_x,
                (first_y + second_y) * 0.5,
                format!("diff {:.2}", (second_y - first_y).abs()),
                Color32::BLACK,
                egui::Align2::LEFT_CENTER,
            );
        }
    }

    fn channel_row_ui(
        &mut self,
        ui: &mut egui::Ui,
        dataset_index: usize,
        channel: &crate::data::ChannelMeta,
        display_name: &str,
        source_label: &str,
    ) -> bool {
        let editable_name = dataset_index == 0
            || self.dataset_kind_by_index(dataset_index) != Some(SourceKind::Dat);
        if (editable_name && channel.index >= self.display_names.len())
            || (dataset_index == 0 && channel.index >= self.visible.len())
            || (dataset_index > 0
                && self
                    .imported_datasets
                    .get(dataset_index - 1)
                    .is_none_or(|dataset| channel.index >= dataset.visible.len()))
        {
            return false;
        }
        ui.push_id(("channel_row", dataset_index, channel.index), |ui| {
            let mut name_context_response: Option<egui::Response> = None;
            let row_response = ui.horizontal(|ui| {
                let mut row_hovered = false;
                let mut add_from_name = false;
                let mut color = self.sidebar_channel_color(channel.index, dataset_index);
                let color_response = if dataset_index == 0 {
                    let response = egui::color_picker::color_edit_button_srgba(
                        ui,
                        &mut color,
                        egui::color_picker::Alpha::Opaque,
                    );
                    if response.changed() {
                        if let Some(stored_color) = self.channel_colors.get_mut(channel.index) {
                            *stored_color = color;
                        }
                    }
                    response
                } else {
                    Self::color_swatch(ui, color).on_hover_text(self.t(UiText::Color))
                };
                row_hovered |= color_response.hovered();
                let mut visible = self.dataset_channel_visible(dataset_index, channel.index);
                let checkbox_response = ui.checkbox(&mut visible, "");
                row_hovered |= checkbox_response.hovered();
                if checkbox_response.changed() {
                    self.set_dataset_channel_visible(dataset_index, channel.index, visible);
                }
                let rename_hint = self.t(UiText::DoubleClickRename);
                let available_name_width = ui.available_width();
                let name_width = available_name_width
                    .clamp(CHANNEL_NAME_COLUMN_MIN_WIDTH, CHANNEL_NAME_COLUMN_MAX_WIDTH);
                let is_digital =
                    Self::channel_is_digital(self.dataset_kind_by_index(dataset_index), channel);
                let compact_display_text = Self::channel_panel_display_text(
                    display_name,
                    is_digital,
                    available_name_width,
                );
                if editable_name {
                    let Some(name) = self.display_names.get_mut(channel.index) else {
                        return row_hovered;
                    };
                    if self.editing_display_name == Some(channel.index) {
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::IMEAllowed(true));
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::IMEPurpose(
                                egui::viewport::IMEPurpose::Normal,
                            ));
                        let name_response = ui.add(
                            egui::TextEdit::singleline(name)
                                .id_source(("display_name_edit", dataset_index, channel.index))
                                .desired_width(name_width),
                        );
                        let just_requested_focus =
                            self.pending_display_name_focus == Some(channel.index);
                        if just_requested_focus {
                            name_response.request_focus();
                            self.pending_display_name_focus = None;
                        }
                        row_hovered |= name_response.hovered() || name_response.has_focus();
                        let (finish_key_pressed, ime_composing) = ui.input(|input| {
                            (
                                input.key_pressed(egui::Key::Enter)
                                    || input.key_pressed(egui::Key::Escape),
                                input.events.iter().any(|event| {
                                    matches!(
                                        event,
                                        egui::Event::CompositionStart
                                            | egui::Event::CompositionUpdate(_)
                                            | egui::Event::CompositionEnd(_)
                                    )
                                }),
                            )
                        });
                        let finish_edit = finish_key_pressed
                            || (!ime_composing
                                && !just_requested_focus
                                && name_response.lost_focus());
                        if finish_edit {
                            if finish_key_pressed {
                                name_response.surrender_focus();
                            }
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::IMEAllowed(false));
                            self.editing_display_name = None;
                        }
                    } else if let Some(compact_display_name) = compact_display_text.as_deref() {
                        let label_response = ui
                            .add_sized(
                                [name_width, ui.spacing().interact_size.y],
                                egui::Label::new(compact_display_name)
                                    .sense(egui::Sense::click())
                                    .truncate(true),
                            )
                            .on_hover_text(format!("{display_name}\n{rename_hint}"));
                        row_hovered |= label_response.hovered();
                        name_context_response = Some(label_response.clone());
                        if label_response.clicked() && !label_response.double_clicked() {
                            add_from_name = true;
                        }
                        if label_response.double_clicked() {
                            self.editing_display_name = Some(channel.index);
                            self.pending_display_name_focus = Some(channel.index);
                            ui.ctx().request_repaint();
                        }
                    }
                } else if let Some(compact_display_name) = compact_display_text.as_deref() {
                    let label_response = ui
                        .add_sized(
                            [name_width, ui.spacing().interact_size.y],
                            egui::Label::new(compact_display_name)
                                .sense(egui::Sense::click())
                                .truncate(true),
                        )
                        .on_hover_text(display_name);
                    row_hovered |= label_response.hovered();
                    name_context_response = Some(label_response.clone());
                    if label_response.clicked() && !label_response.double_clicked() {
                        add_from_name = true;
                    }
                }
                if add_from_name {
                    self.set_dataset_channel_visible(dataset_index, channel.index, true);
                }
                if compact_display_text.is_some() && !channel.unit.is_empty() {
                    row_hovered |= ui.label(format!("({})", channel.unit)).hovered();
                }
                if compact_display_text.is_some() && display_name != channel.name {
                    row_hovered |= ui
                        .label(RichText::new(format!("{source_label}: {}", channel.name)).small())
                        .hovered();
                }
                row_hovered
            });
            let row_hovered = row_response.response.hovered() || row_response.inner;
            let show_channel_menu = |ui: &mut egui::Ui, app: &mut ScopeApp| {
                ui.strong(display_name);
                ui.separator();
                app.channel_style_menu(ui, channel.index);
            };
            if let Some(response) = name_context_response {
                response.context_menu(|ui| show_channel_menu(ui, self));
            }
            row_response
                .response
                .context_menu(|ui| show_channel_menu(ui, self));
            row_hovered
        })
        .inner
    }

    fn channel_style_menu(&mut self, ui: &mut egui::Ui, channel_index: usize) {
        ui.strong(self.t(UiText::ColorSettings));
        ui.horizontal(|ui| {
            ui.label(self.t(UiText::Color));
            let mut color = self.channel_color(channel_index);
            let color_response = egui::color_picker::color_edit_button_srgba(
                ui,
                &mut color,
                egui::color_picker::Alpha::Opaque,
            );
            if color_response.changed() {
                if let Some(stored_color) = self.channel_colors.get_mut(channel_index) {
                    *stored_color = color;
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(self.t(UiText::LineStyle));
            if let Some(pattern) = self.line_patterns.get_mut(channel_index) {
                egui::ComboBox::from_id_source(("line_pattern", channel_index))
                    .selected_text(pattern.label(self.language))
                    .show_ui(ui, |ui| {
                        for candidate in ChannelLinePattern::ALL {
                            ui.selectable_value(pattern, candidate, candidate.label(self.language));
                        }
                    });
            }
        });
        ui.horizontal(|ui| {
            ui.label(self.t(UiText::LineWidth));
            if let Some(width) = self.line_widths.get_mut(channel_index) {
                let width_response = ui.add(
                    egui::DragValue::new(width)
                        .speed(0.1)
                        .clamp_range(MIN_CHANNEL_LINE_WIDTH..=MAX_CHANNEL_LINE_WIDTH),
                );
                if width_response.changed() {
                    *width = (*width).clamp(MIN_CHANNEL_LINE_WIDTH, MAX_CHANNEL_LINE_WIDTH);
                }
            }
        });

        ui.separator();
        ui.strong(self.t(UiText::ScaleRatio));
        ui.horizontal(|ui| {
            ui.label(self.t(UiText::Scale));
            if let Some(scale) = self.channel_scales.get_mut(channel_index) {
                let old_scale = *scale;
                let scale_response = ui.add(
                    egui::DragValue::new(scale)
                        .speed(0.01)
                        .clamp_range(MIN_CHANNEL_SCALE..=MAX_CHANNEL_SCALE),
                );
                *scale = Self::sanitize_channel_scale(*scale);
                if scale_response.changed() && (*scale - old_scale).abs() > f32::EPSILON {
                    self.y_min = None;
                    self.y_max = None;
                    self.measurement_cache = None;
                    self.derived_curve_cache = None;
                    self.derived_measurement_cache = None;
                    self.fft_results.clear();
                    self.needs_fft_reload = true;
                    self.needs_plot_reload = true;
                    self.needs_compare_plot_reload = true;
                    self.needs_derived_reload = true;
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button(self.t(UiText::Zoom2x)).clicked() {
                self.multiply_channel_scale(channel_index, 2.0);
            }
            if ui.button(self.t(UiText::ShrinkHalf)).clicked() {
                self.multiply_channel_scale(channel_index, 0.5);
            }
            if ui.button(self.t(UiText::Reset)).clicked() {
                self.set_channel_scale(channel_index, DEFAULT_CHANNEL_SCALE);
            }
        });
    }

    fn derived_channel_style_menu(&mut self, ui: &mut egui::Ui, derived_index: usize) {
        ui.strong(self.t(UiText::ColorSettings));
        ui.horizontal(|ui| {
            ui.label(self.t(UiText::Color));
            let mut color = self.derived_channel_color(derived_index);
            let color_response = egui::color_picker::color_edit_button_srgba(
                ui,
                &mut color,
                egui::color_picker::Alpha::Opaque,
            );
            if color_response.changed() {
                if let Some(stored_color) = self.derived_colors.get_mut(derived_index) {
                    *stored_color = color;
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(self.t(UiText::LineStyle));
            if let Some(pattern) = self.derived_line_patterns.get_mut(derived_index) {
                egui::ComboBox::from_id_source(("derived_line_pattern", derived_index))
                    .selected_text(pattern.label(self.language))
                    .show_ui(ui, |ui| {
                        for candidate in ChannelLinePattern::ALL {
                            ui.selectable_value(pattern, candidate, candidate.label(self.language));
                        }
                    });
            }
        });
    }

    fn derived_channel_row_ui(&mut self, ui: &mut egui::Ui, derived_index: usize) {
        let repid_index = derived_index.checked_sub(DERIVED_CHANNEL_COUNT);
        let repid_hover = repid_index.and_then(|index| {
            let curve = self.repid_derived_curves.get(index)?;
            Some(format!(
                "Expression: {}\nInputs: {}\nDiagnostic: {}",
                curve.script,
                Self::repid_curve_input_text(curve),
                self.repid_curve_diagnostic_text(curve)
            ))
        });
        let mut delete_repid_index = None;
        let mut repid_curve_changed = false;
        let name_label = self.tr("名称", "Name").to_owned();
        let expression_label = self.tr("表达式", "Expression").to_owned();
        let inputs_label = self.tr("输入通道", "Input Channels").to_owned();
        let diagnostic_label = self.tr("诊断", "Diagnostic").to_owned();
        let delete_label = self.tr("删除", "Delete").to_owned();
        let diagnostic_channel_count = self
            .dataset_meta_by_index(self.selected_fft_dataset_index())
            .map(|meta| meta.channels.len());
        ui.push_id(("derived_channel_row", derived_index), |ui| {
            let row_response = ui.horizontal(|ui| {
                let mut color = self.derived_channel_color(derived_index);
                let color_response = egui::color_picker::color_edit_button_srgba(
                    ui,
                    &mut color,
                    egui::color_picker::Alpha::Opaque,
                );
                if color_response.changed() {
                    if let Some(stored_color) = self.derived_colors.get_mut(derived_index) {
                        *stored_color = color;
                    }
                }
                let mut visible = self
                    .derived_visible
                    .get(derived_index)
                    .copied()
                    .unwrap_or(false);
                if ui.checkbox(&mut visible, "").changed() {
                    if let Some(stored_visible) = self.derived_visible.get_mut(derived_index) {
                        *stored_visible = visible;
                    }
                    if visible {
                        let active_pane = self.current_scope_pane();
                        if let Some(pane) = self.derived_panes.get_mut(derived_index) {
                            *pane = active_pane;
                        }
                    }
                    self.clear_derived_runtime_state();
                }
                let available_label_width = ui.available_width();
                let label_width = available_label_width
                    .clamp(CHANNEL_NAME_COLUMN_MIN_WIDTH, CHANNEL_NAME_COLUMN_MAX_WIDTH);
                if let Some(display_text) = Self::channel_panel_display_text(
                    &self.derived_channel_name(derived_index),
                    false,
                    available_label_width,
                ) {
                    let label_response = ui
                        .add_sized(
                            [label_width, ui.spacing().interact_size.y],
                            egui::Label::new(display_text)
                                .sense(egui::Sense::click())
                                .truncate(true),
                        )
                        .on_hover_text(
                            repid_hover
                                .clone()
                                .unwrap_or_else(|| self.derived_channel_name(derived_index)),
                        );
                    if label_response.clicked() && !label_response.double_clicked() {
                        if let Some(stored_visible) = self.derived_visible.get_mut(derived_index) {
                            *stored_visible = true;
                        }
                        let active_pane = self.current_scope_pane();
                        if let Some(pane) = self.derived_panes.get_mut(derived_index) {
                            *pane = active_pane;
                        }
                        self.clear_derived_runtime_state();
                    }
                }
            });
            row_response.response.context_menu(|ui| {
                ui.strong(self.derived_channel_name(derived_index));
                ui.separator();
                self.derived_channel_style_menu(ui, derived_index);
                if let Some(repid_index) = repid_index {
                    if let Some(curve) = self.repid_derived_curves.get_mut(repid_index) {
                        ui.separator();
                        ui.label(name_label.as_str());
                        if ui.text_edit_singleline(&mut curve.name).changed() {
                            repid_curve_changed = true;
                        }
                        ui.label(expression_label.as_str());
                        ui.monospace(curve.script.as_str());
                        ui.label(format!(
                            "{}: {}",
                            inputs_label,
                            Self::repid_curve_input_text(curve)
                        ));
                        ui.label(format!(
                            "{}: {}",
                            diagnostic_label,
                            curve.diagnostic(diagnostic_channel_count)
                        ));
                        if ui.button(delete_label.as_str()).clicked() {
                            delete_repid_index = Some(repid_index);
                            ui.close_menu();
                        }
                    }
                }
            });
        });
        if repid_curve_changed {
            self.clear_derived_runtime_state();
        }
        if let Some(repid_index) = delete_repid_index {
            self.remove_repid_derived_curve(repid_index);
        }
    }

    fn derived_channels_panel_ui(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        let selected_count = self.selected_derived_channels().len();
        ui.strong(format!(
            "{} ({selected_count}/{})",
            self.t(UiText::Derived),
            self.derived_output_count()
        ));
        ui.horizontal(|ui| {
            if ui
                .button(self.tr("生成正序预设", "Add Sequence Presets"))
                .on_hover_text(self.tr(
                    "使用右侧分析入口的三相电压和三相电流通道生成正序电压、有功、无功、有功电流、无功电流。",
                    "Use the current voltage/current A-B-C analysis inputs to add U/P/Q/IP/IQ derived curves.",
                ))
                .clicked()
            {
                self.add_repid_sequence_presets();
            }
        });
        for index in 0..self.derived_output_count() {
            self.derived_channel_row_ui(ui, index);
        }
    }

    fn channel_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading(self.t(UiText::Channels));
        if self.scope_pane_count() > 1 {
            ui.label(format!(
                "{} {}",
                self.t(UiText::ActivePane),
                self.current_scope_pane() + 1
            ));
        }
        let filter_hint = self.t(UiText::FilterChannelsHint);
        ui.horizontal(|ui| {
            let (filter_width, show_clear) =
                Self::channel_filter_width(ui.available_width(), !self.channel_filter.is_empty());
            ui.add(
                egui::TextEdit::singleline(&mut self.channel_filter)
                    .hint_text(filter_hint)
                    .desired_width(filter_width),
            );
            if show_clear
                && ui
                    .add_sized([42.0, ui.spacing().interact_size.y], egui::Button::new("×"))
                    .on_hover_text(self.t(UiText::Clear))
                    .clicked()
            {
                self.channel_filter.clear();
            }
        });

        let Some(meta) = self.meta().cloned() else {
            ui.label(self.t(UiText::NoDataLoaded));
            return;
        };
        let filter_terms = self
            .channel_filter
            .split_whitespace()
            .map(|term| term.to_lowercase())
            .collect::<Vec<_>>();
        let matching_count = meta
            .channels
            .iter()
            .filter(|channel| {
                let display_name = self.channel_name(channel.index);
                let searchable = format!("{} {}", display_name, channel.name).to_lowercase();
                filter_terms.iter().all(|term| searchable.contains(term))
            })
            .count();
        ui.separator();

        let mut hovered_channel = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.strong(self.t(UiText::Datasets));
            self.dataset_groups_ui(ui, &filter_terms, &mut hovered_channel);
        });
        if matching_count == 0 {
            ui.label(self.t(UiText::NoMatchingChannels));
        }
        self.hovered_channel = hovered_channel;
    }

    fn analysis_dataset_selector(&mut self, ui: &mut egui::Ui) {
        if self.meta().is_none() {
            return;
        }
        self.fft_dataset_index = self.selected_fft_dataset_index();
        let old_dataset = self.fft_dataset_index;
        ui.strong(self.t(UiText::AnalysisInput));
        let input_combo_width = Self::analysis_combo_width(
            (ui.available_width() - ANALYSIS_INPUT_LABEL_WIDTH - ui.spacing().item_spacing.x)
                .max(0.0),
        );
        ui.horizontal(|ui| {
            ui.add_sized(
                [ANALYSIS_INPUT_LABEL_WIDTH, 0.0],
                egui::Label::new(self.t(UiText::AnalysisDataset)),
            );
            egui::ComboBox::from_id_source("analysis_dataset_select")
                .width(input_combo_width)
                .selected_text(self.dataset_label(self.fft_dataset_index))
                .show_ui(ui, |ui| {
                    for dataset_index in 0..self.dataset_count() {
                        let dataset_label = self.dataset_label(dataset_index);
                        ui.selectable_value(
                            &mut self.fft_dataset_index,
                            dataset_index,
                            dataset_label,
                        );
                    }
                });
        });
        if self.fft_dataset_index != old_dataset {
            self.measurement_cache = None;
            self.derived_measurement_cache = None;
            self.derived_curve_cache = None;
            self.prepared_derived_curve_cache = PreparedPlotSeries::default();
            self.fft_channel_user_selected = false;
            self.sequence_channels_user_selected = false;
            self.sequence_channels = self
                .preferred_sequence_channels(&self.fft_channel_options())
                .unwrap_or([0, 1, 2]);
            self.dq_source_channels_user_selected = false;
            self.pll_source_channels = self
                .preferred_pll_source_channels(&self.fft_channel_options())
                .unwrap_or([0, 1, 2]);
            self.dq_source_channels = self.pll_source_channels;
            self.fft_results.clear();
            self.needs_fft_reload = true;
            self.needs_derived_reload = true;
        }

        let channel_options = self.fft_channel_options();
        if channel_options.is_empty() {
            self.fft_results.clear();
            self.needs_fft_reload = false;
            ui.label(self.t(UiText::NoAnalogFftChannels));
            return;
        }
        let preferred_channel = self.preferred_fft_channel(&channel_options);
        if !channel_options.contains(&self.fft_channel)
            || (!self.fft_channel_user_selected && Some(self.fft_channel) != preferred_channel)
        {
            self.fft_channel = preferred_channel.unwrap_or(channel_options[0]);
            self.fft_channel_user_selected = false;
            self.measurement_cache = None;
            self.derived_measurement_cache = None;
            self.fft_results.clear();
            self.needs_fft_reload = true;
            self.sequence_channels = self
                .related_three_phase_channels_from_anchor(self.fft_channel, &channel_options)
                .or_else(|| self.preferred_sequence_channels(&channel_options))
                .unwrap_or([self.fft_channel, self.fft_channel, self.fft_channel]);
            self.sequence_channels_user_selected = false;
        }

        let mut changed_channel = false;
        ui.horizontal(|ui| {
            ui.add_sized(
                [ANALYSIS_INPUT_LABEL_WIDTH, 0.0],
                egui::Label::new(self.t(UiText::AnalysisChannel)),
            );
            let selected_name = self.fft_channel_name(self.fft_dataset_index, self.fft_channel);
            let selected_short =
                Self::compact_label(&selected_name, ANALYSIS_CHANNEL_LABEL_CHARS + 2);
            let response = egui::ComboBox::from_id_source("analysis_channel_select")
                .width(input_combo_width)
                .selected_text(selected_short)
                .show_ui(ui, |ui| {
                    for channel_index in &channel_options {
                        let channel_name =
                            self.fft_channel_name(self.fft_dataset_index, *channel_index);
                        let channel_short =
                            Self::compact_label(&channel_name, ANALYSIS_CHANNEL_LABEL_CHARS + 8);
                        if ui
                            .selectable_value(&mut self.fft_channel, *channel_index, channel_short)
                            .on_hover_text(channel_name)
                            .changed()
                        {
                            changed_channel = true;
                        }
                    }
                });
            response.response.on_hover_text(selected_name);
        });
        if changed_channel {
            self.fft_channel_user_selected = true;
            self.measurement_cache = None;
            self.derived_measurement_cache = None;
            self.fft_results.clear();
            self.needs_fft_reload = true;
            if let Some(channels) =
                self.related_three_phase_channels_from_anchor(self.fft_channel, &channel_options)
            {
                self.sequence_channels = channels;
                self.sequence_channels_user_selected = false;
                self.dq_source_channels = channels;
                self.dq_source_channels_user_selected = false;
                self.derived_curve_cache = None;
                self.prepared_derived_curve_cache = PreparedPlotSeries::default();
                self.needs_derived_reload = true;
            }
        }

        self.analysis_three_phase_selector_ui(ui, &channel_options);
    }

    fn ensure_sequence_channels_for_options(&mut self, channel_options: &[usize]) {
        if !Self::analysis_input_shows_three_phase_selectors(channel_options.len()) {
            return;
        }
        let preferred = self.preferred_sequence_channels(channel_options);
        let valid_selection =
            Self::valid_three_phase_selection(self.sequence_channels, channel_options);
        if !valid_selection || (!self.sequence_channels_user_selected && preferred.is_some()) {
            self.sequence_channels = preferred
                .or_else(|| Self::default_sequence_channels_from_options(channel_options))
                .unwrap_or([0, 1, 2]);
            self.sequence_channels_user_selected = false;
        }
    }

    fn analysis_three_phase_selector_ui(&mut self, ui: &mut egui::Ui, channel_options: &[usize]) {
        let Some(_) = Self::analysis_input_three_phase_row_layout(
            channel_options.len(),
            ui.available_width(),
        ) else {
            return;
        };
        self.ensure_sequence_channels_for_options(channel_options);
        let options = channel_options
            .iter()
            .map(|channel| {
                (
                    *channel,
                    self.fft_channel_name(self.fft_dataset_index, *channel),
                    self.related_three_phase_channels_from_anchor(*channel, channel_options),
                )
            })
            .collect::<Vec<_>>();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_sized(
                [ANALYSIS_INPUT_LABEL_WIDTH, 0.0],
                egui::Label::new(self.t(UiText::ThreePhaseChannels)),
            );
            let changed = Self::three_phase_channel_selectors_ui(
                ui,
                "analysis_sequence_channel",
                self.fft_dataset_index,
                &options,
                &mut self.sequence_channels,
            );
            if changed {
                self.sequence_channels_user_selected = true;
                self.sequence_cache = None;
            }
        });
    }

    fn poll_measurement_worker(&mut self, expected_key: &MeasurementJobKey) {
        let Some(joined) =
            Self::take_finished_job(&mut self.measurement_worker, "Measurement worker panicked.")
        else {
            return;
        };
        self.measurement_worker_key = None;
        self.measurement_cancel = None;
        let Ok(result) = joined else {
            self.last_error = Some("Measurement worker panicked.".to_owned());
            return;
        };
        let result_key = MeasurementJobKey {
            generation: result.generation,
            dataset_index: result.dataset_index,
            start: result.start,
            end: result.end,
            channels: result.channels.clone(),
        };
        if &result_key != expected_key {
            return;
        }
        match result.result {
            Ok(rows) => {
                self.measurement_cache = Some(MeasurementCache {
                    dataset_index: result.dataset_index,
                    start: result.start,
                    end: result.end,
                    channels: result.channels,
                    rows,
                });
            }
            Err(error) => self.last_error = Some(error),
        }
    }

    fn start_measurement_worker(&mut self, key: MeasurementJobKey) {
        if self.measurement_worker_key.as_ref() == Some(&key) {
            return;
        }
        if let Some(token) = &self.measurement_cancel {
            token.cancel();
        }
        self.measurement_worker = None;
        self.measurement_worker_key = None;
        self.measurement_cancel = None;
        let Some((source, read_start, read_end)) = self.analysis_read_request(key.dataset_index)
        else {
            return;
        };
        let channels = key.channels.clone();
        let channel_scales = channels
            .iter()
            .map(|channel| (*channel, self.channel_scale(*channel)))
            .collect::<Vec<_>>();
        self.measurement_worker_key = Some(key.clone());
        let cancel_token = JobCancelToken::new();
        let worker_cancel = cancel_token.clone();
        self.measurement_cancel = Some(cancel_token);
        Self::spawn_job(&mut self.measurement_worker, move || {
            let result = Self::worker_result("Measurement worker panicked.", || {
                let data_cancel = worker_cancel.data_token();
                if read_end <= read_start {
                    Ok(Vec::new())
                } else {
                    source
                        .read_range_cancellable(
                            read_start,
                            read_end,
                            &channels,
                            MAX_AUTO_MEASURE_POINTS,
                            &data_cancel,
                        )
                        .map_err(|error| error.to_string())
                        .map(|block| {
                            let mut rows = Vec::new();
                            if worker_cancel.is_cancelled() {
                                return rows;
                            }
                            if !block.times.is_empty() {
                                for (out_index, (channel_index, scale)) in
                                    channel_scales.iter().enumerate()
                                {
                                    let Some(values) = block.channels.get(out_index) else {
                                        continue;
                                    };
                                    let scaled_values =
                                        if (*scale - DEFAULT_CHANNEL_SCALE).abs() <= f32::EPSILON {
                                            values.clone()
                                        } else {
                                            values.iter().map(|value| *value * *scale).collect()
                                        };
                                    if let Some(measurement) =
                                        Self::auto_measure(&block.times, &scaled_values)
                                    {
                                        rows.push((*channel_index, measurement));
                                    }
                                }
                            }
                            rows
                        })
                }
            });
            MeasurementJobResult {
                generation: key.generation,
                dataset_index: key.dataset_index,
                start: key.start,
                end: key.end,
                channels: key.channels,
                result,
            }
        });
    }

    fn poll_derived_measurement_worker(&mut self, expected_key: &DerivedJobKey) {
        let Some(joined) = Self::take_finished_job(
            &mut self.derived_measurement_worker,
            "PLL/dq0 measurement worker panicked.",
        ) else {
            return;
        };
        self.derived_measurement_worker_key = None;
        self.derived_measurement_cancel = None;
        let Ok(result) = joined else {
            self.last_error = Some("PLL/dq0 measurement worker panicked.".to_owned());
            return;
        };
        let result_key = DerivedJobKey {
            generation: result.generation,
            dataset_index: result.dataset_index,
            start: result.start,
            end: result.end,
            pll_channels: result.pll_channels,
            dq_channels: result.dq_channels,
            channels: result.channels.clone(),
            repid_signatures: result.repid_signatures.clone(),
        };
        if &result_key != expected_key {
            return;
        }
        match result.result {
            Ok(rows) => {
                self.derived_measurement_cache = Some(DerivedMeasurementCache {
                    dataset_index: result.dataset_index,
                    start: result.start,
                    end: result.end,
                    pll_channels: result.pll_channels,
                    dq_channels: result.dq_channels,
                    repid_signatures: result.repid_signatures,
                    channels: result.channels,
                    rows,
                });
            }
            Err(error) => self.last_error = Some(error),
        }
    }

    fn start_derived_measurement_worker(&mut self, key: DerivedJobKey, channels: Vec<usize>) {
        if channels.is_empty() || self.derived_measurement_worker_key.as_ref() == Some(&key) {
            return;
        }
        if let Some(token) = &self.derived_measurement_cancel {
            token.cancel();
        }
        self.derived_measurement_worker = None;
        self.derived_measurement_worker_key = None;
        self.derived_measurement_cancel = None;
        let Some((source, read_start, read_end, _)) =
            self.dataset_read_request_for_range(key.dataset_index, key.start, key.end)
        else {
            return;
        };
        let pll_scales = key.pll_channels.map(|channel| self.channel_scale(channel));
        let dq_scales = key.dq_channels.map(|channel| self.channel_scale(channel));
        let channel_scales = self.channel_scales.clone();
        let repid_curves = self.repid_derived_curves.clone();
        let sample_rate_hz = self.sample_rate_hz.max(1.0);
        let harmonic_base_hz = self.harmonic_base_hz.max(0.001);
        let skip_digital_by_samples =
            self.dataset_kind_by_index(key.dataset_index) != Some(SourceKind::Cloud);
        let generation = key.generation;
        self.derived_measurement_worker_key = Some(key.clone());
        let cancel_token = JobCancelToken::new();
        let worker_cancel = cancel_token.clone();
        self.derived_measurement_cancel = Some(cancel_token);
        Self::spawn_job(&mut self.derived_measurement_worker, move || {
            let result = Self::worker_result("PLL/dq0 measurement worker panicked.", || {
                let data_cancel = worker_cancel.data_token();
                Self::load_derived_data_with_cancel(
                    source,
                    read_start,
                    read_end,
                    &channels,
                    &repid_curves,
                    key.pll_channels,
                    key.dq_channels,
                    pll_scales,
                    dq_scales,
                    &channel_scales,
                    sample_rate_hz,
                    harmonic_base_hz,
                    skip_digital_by_samples,
                    MAX_AUTO_MEASURE_POINTS,
                    Some(&data_cancel),
                )
                .map(|block| {
                    let mut rows = Vec::new();
                    if worker_cancel.is_cancelled() {
                        return rows;
                    }
                    for derived_index in &channels {
                        let Some(values) = block.channels.get(*derived_index) else {
                            continue;
                        };
                        if let Some(measurement) = Self::auto_measure(&block.times, values) {
                            rows.push((*derived_index, measurement));
                        }
                    }
                    rows
                })
            });
            DerivedMeasurementJobResult {
                generation,
                dataset_index: key.dataset_index,
                start: key.start,
                end: key.end,
                pll_channels: key.pll_channels,
                dq_channels: key.dq_channels,
                repid_signatures: key.repid_signatures,
                channels,
                result,
            }
        });
    }

    fn poll_sequence_worker(&mut self, expected_key: &SequenceJobKey) {
        let Some(joined) =
            Self::take_finished_job(&mut self.sequence_worker, "Sequence worker panicked.")
        else {
            return;
        };
        self.sequence_worker_key = None;
        self.sequence_cancel = None;
        let Ok(result) = joined else {
            self.sequence_cache = Some(SequenceCache {
                dataset_index: expected_key.dataset_index,
                start: expected_key.start,
                end: expected_key.end,
                channels: expected_key.channels,
                result: Err("Sequence worker panicked.".to_owned()),
            });
            return;
        };
        let result_key = SequenceJobKey {
            generation: result.generation,
            dataset_index: result.dataset_index,
            start: result.start,
            end: result.end,
            channels: result.channels,
        };
        if &result_key != expected_key {
            return;
        }
        self.sequence_cache = Some(SequenceCache {
            dataset_index: result.dataset_index,
            start: result.start,
            end: result.end,
            channels: result.channels,
            result: result.result,
        });
    }

    fn start_sequence_worker(&mut self, key: SequenceJobKey) {
        if self.sequence_worker_key.as_ref() == Some(&key) {
            return;
        }
        if let Some(token) = &self.sequence_cancel {
            token.cancel();
        }
        self.sequence_worker = None;
        self.sequence_worker_key = None;
        self.sequence_cancel = None;
        let Some((source, read_start, read_end)) = self.analysis_read_request(key.dataset_index)
        else {
            return;
        };
        let channels = key.channels;
        let channel_scales = channels.map(|channel| self.channel_scale(channel));
        let sample_rate_hz = self.sample_rate_hz.max(1.0);
        let harmonic_base_hz = self.harmonic_base_hz.max(0.001);
        let skip_digital_by_samples =
            self.dataset_kind_by_index(key.dataset_index) != Some(SourceKind::Cloud);
        self.sequence_worker_key = Some(key.clone());
        let cancel_token = JobCancelToken::new();
        let worker_cancel = cancel_token.clone();
        self.sequence_cancel = Some(cancel_token);
        Self::spawn_job(&mut self.sequence_worker, move || {
            let result = Self::worker_result("Sequence worker panicked.", || {
                let data_cancel = worker_cancel.data_token();
                if read_end <= read_start {
                    Err(
                        "Sequence analysis needs at least 16 samples in the cursor range."
                            .to_owned(),
                    )
                } else {
                    source
                        .read_range_cancellable(
                            read_start,
                            read_end,
                            &channels,
                            MAX_FFT_POINTS,
                            &data_cancel,
                        )
                        .map_err(|error| error.to_string())
                        .and_then(|block| {
                            if worker_cancel.is_cancelled() {
                                return Err("Operation cancelled.".to_owned());
                            }
                            if block.channels.len() < 3 {
                                return Err("Sequence analysis needs three channels.".to_owned());
                            }
                            let samples = (0..3)
                                .map(|out_index| {
                                    let values =
                                        block.channels.get(out_index).cloned().unwrap_or_default();
                                    let scale = channel_scales[out_index];
                                    if (scale - DEFAULT_CHANNEL_SCALE).abs() <= f32::EPSILON {
                                        values
                                    } else {
                                        values.iter().map(|value| *value * scale).collect()
                                    }
                                })
                                .collect::<Vec<_>>();
                            if skip_digital_by_samples
                                && samples
                                    .iter()
                                    .any(|values| Self::samples_look_digital(values))
                            {
                                return Err(
                                    "Sequence analysis only supports analog channels.".to_owned()
                                );
                            }
                            fft::sequence_components(
                                &samples[0],
                                &samples[1],
                                &samples[2],
                                sample_rate_hz,
                                harmonic_base_hz,
                            )
                            .ok_or_else(|| {
                                "Sequence analysis needs at least 16 samples in the cursor range."
                                    .to_owned()
                            })
                        })
                }
            });
            SequenceJobResult {
                generation: key.generation,
                dataset_index: key.dataset_index,
                start: key.start,
                end: key.end,
                channels: key.channels,
                result,
            }
        });
    }

    fn measurements_panel(&mut self, ui: &mut egui::Ui) {
        let hidden_label = self.t(UiText::Hidden);
        let dt = (self.cursor_b - self.cursor_a).abs();
        let dataset_index = self.selected_fft_dataset_index();
        self.fft_dataset_index = dataset_index;

        ui.heading(self.t(UiText::Measurements));
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "{}: {:.5}s{}",
                Self::cursor_label(CursorId::A),
                self.cursor_a,
                if self.show_cursor_a { "" } else { hidden_label }
            ));
            ui.label(format!(
                "{}: {:.5}s{}",
                Self::cursor_label(CursorId::B),
                self.cursor_b,
                if self.show_cursor_b { "" } else { hidden_label }
            ));
            ui.label(format!("dX: {:.5}s", dt));
            if dt > 0.0 {
                ui.label(format!("1/dt: {:.3} Hz", 1.0 / dt));
            }
        });
        if let Some(cursor) = self.cursor_place_mode {
            ui.label(format!(
                "Placing cursor {}: click waveform to fix, Esc to cancel.",
                Self::cursor_label(cursor)
            ));
        }
        ui.add_space(4.0);

        if self.source.is_none() {
            return;
        }
        let channel_options = self.fft_channel_options();
        let channels = channel_options
            .contains(&self.fft_channel)
            .then_some(self.fft_channel)
            .into_iter()
            .collect::<Vec<_>>();
        let derived_channels = self.selected_derived_channels();
        if channels.is_empty() && derived_channels.is_empty() {
            ui.label(self.t(UiText::NoChannelsSelected));
            return;
        }
        let measurement_channels = channels.iter().copied().take(12).collect::<Vec<_>>();
        let derived_measurement_channels = derived_channels
            .iter()
            .copied()
            .take(12_usize.saturating_sub(measurement_channels.len()))
            .collect::<Vec<_>>();
        let start = self.cursor_a.min(self.cursor_b);
        let end = self.cursor_a.max(self.cursor_b);
        let measurement_key = MeasurementJobKey {
            generation: self.data_generation,
            dataset_index,
            start,
            end,
            channels: measurement_channels.clone(),
        };
        let derived_measurement_key = self.derived_key_for_range(start, end);
        if !derived_measurement_channels.is_empty() {
            self.poll_derived_measurement_worker(&derived_measurement_key);
            let derived_cache_matches =
                self.derived_measurement_cache
                    .as_ref()
                    .is_some_and(|cache| {
                        cache.dataset_index == dataset_index
                            && cache.start == start
                            && cache.end == end
                            && cache.pll_channels == self.pll_source_channels
                            && cache.dq_channels == self.dq_source_channels
                            && cache.repid_signatures == derived_measurement_key.repid_signatures
                            && cache.channels == derived_measurement_channels
                    });
            if !derived_cache_matches {
                self.start_derived_measurement_worker(
                    derived_measurement_key,
                    derived_measurement_channels.clone(),
                );
            }
        }
        self.poll_measurement_worker(&measurement_key);

        let cache_matches = match &self.measurement_cache {
            Some(cache) => {
                cache.dataset_index == dataset_index
                    && cache.start == start
                    && cache.end == end
                    && cache.channels == measurement_channels
            }
            None => false,
        };
        if !cache_matches {
            self.start_measurement_worker(measurement_key);
            ui.label(self.t(UiText::CalculatingMeasurements));
            ui.ctx().request_repaint();
            return;
        }

        let Some(cache) = &self.measurement_cache else {
            return;
        };
        egui::ScrollArea::horizontal().show(ui, |ui| {
            egui::Grid::new("measurement_table")
                .striped(true)
                .num_columns(6)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    Self::fixed_grid_label(
                        ui,
                        MEASUREMENT_CHANNEL_COLUMN_WIDTH,
                        RichText::new(self.t(UiText::Channel)).strong(),
                        false,
                        true,
                    );
                    Self::fixed_grid_label(
                        ui,
                        MEASUREMENT_VALUE_COLUMN_WIDTH,
                        RichText::new("Y1").strong(),
                        true,
                        false,
                    );
                    Self::fixed_grid_label(
                        ui,
                        MEASUREMENT_VALUE_COLUMN_WIDTH,
                        RichText::new("Y2").strong(),
                        true,
                        false,
                    );
                    Self::fixed_grid_label(
                        ui,
                        MEASUREMENT_VALUE_COLUMN_WIDTH,
                        RichText::new("dY").strong(),
                        true,
                        false,
                    );
                    Self::fixed_grid_label(
                        ui,
                        MEASUREMENT_VALUE_COLUMN_WIDTH,
                        RichText::new(self.t(UiText::Min)).strong(),
                        true,
                        false,
                    );
                    Self::fixed_grid_label(
                        ui,
                        MEASUREMENT_VALUE_COLUMN_WIDTH,
                        RichText::new(self.t(UiText::Max)).strong(),
                        true,
                        false,
                    );
                    ui.end_row();

                    for (channel_index, measurement) in &cache.rows {
                        let color = self.plot_channel_color(
                            *channel_index,
                            dataset_index,
                            self.current_scope_pane(),
                            self.scope_pane_count(),
                        );
                        let highlighted = self.hovered_channel == Some(*channel_index);
                        let text = |value: String| {
                            let rich = RichText::new(value).color(color);
                            if highlighted {
                                rich.strong()
                                    .background_color(Color32::from_rgba_premultiplied(
                                        255, 240, 160, 80,
                                    ))
                            } else {
                                rich
                            }
                        };
                        let channel_name = self.fft_channel_name(dataset_index, *channel_index);
                        Self::fixed_grid_label(
                            ui,
                            MEASUREMENT_CHANNEL_COLUMN_WIDTH,
                            text(channel_name.clone()),
                            false,
                            true,
                        )
                        .on_hover_text(channel_name);
                        Self::fixed_grid_label(
                            ui,
                            MEASUREMENT_VALUE_COLUMN_WIDTH,
                            text(format!("{:.2}", measurement.first)),
                            true,
                            false,
                        );
                        Self::fixed_grid_label(
                            ui,
                            MEASUREMENT_VALUE_COLUMN_WIDTH,
                            text(format!("{:.2}", measurement.last)),
                            true,
                            false,
                        );
                        Self::fixed_grid_label(
                            ui,
                            MEASUREMENT_VALUE_COLUMN_WIDTH,
                            text(format!("{:.2}", measurement.last - measurement.first)),
                            true,
                            false,
                        );
                        Self::fixed_grid_label(
                            ui,
                            MEASUREMENT_VALUE_COLUMN_WIDTH,
                            text(format!("{:.2}", measurement.min)),
                            true,
                            false,
                        );
                        Self::fixed_grid_label(
                            ui,
                            MEASUREMENT_VALUE_COLUMN_WIDTH,
                            text(format!("{:.2}", measurement.max)),
                            true,
                            false,
                        );
                        ui.end_row();
                    }
                    if let Some(cache) = &self.derived_measurement_cache {
                        for (derived_index, measurement) in &cache.rows {
                            let color = self.derived_channel_color(*derived_index);
                            let text = |value: String| RichText::new(value).color(color);
                            let channel_name = self.derived_channel_name(*derived_index);
                            Self::fixed_grid_label(
                                ui,
                                MEASUREMENT_CHANNEL_COLUMN_WIDTH,
                                text(channel_name.to_owned()),
                                false,
                                true,
                            )
                            .on_hover_text(channel_name);
                            Self::fixed_grid_label(
                                ui,
                                MEASUREMENT_VALUE_COLUMN_WIDTH,
                                text(format!("{:.2}", measurement.first)),
                                true,
                                false,
                            );
                            Self::fixed_grid_label(
                                ui,
                                MEASUREMENT_VALUE_COLUMN_WIDTH,
                                text(format!("{:.2}", measurement.last)),
                                true,
                                false,
                            );
                            Self::fixed_grid_label(
                                ui,
                                MEASUREMENT_VALUE_COLUMN_WIDTH,
                                text(format!("{:.2}", measurement.last - measurement.first)),
                                true,
                                false,
                            );
                            Self::fixed_grid_label(
                                ui,
                                MEASUREMENT_VALUE_COLUMN_WIDTH,
                                text(format!("{:.2}", measurement.min)),
                                true,
                                false,
                            );
                            Self::fixed_grid_label(
                                ui,
                                MEASUREMENT_VALUE_COLUMN_WIDTH,
                                text(format!("{:.2}", measurement.max)),
                                true,
                                false,
                            );
                            ui.end_row();
                        }
                    }
                });
        });
    }

    fn sequence_panel(&mut self, ui: &mut egui::Ui, channel_options: &[usize]) {
        ui.separator();
        ui.heading(self.t(UiText::Sequence));
        if channel_options.len() < 3 {
            ui.label(self.t(UiText::SequenceNeedsThreeAnalogChannels));
            return;
        }

        self.ensure_sequence_channels_for_options(channel_options);

        let start = self.cursor_a.min(self.cursor_b);
        let end = self.cursor_a.max(self.cursor_b);
        let sequence_key = SequenceJobKey {
            generation: self.data_generation,
            dataset_index: self.fft_dataset_index,
            start,
            end,
            channels: self.sequence_channels,
        };
        self.poll_sequence_worker(&sequence_key);
        let cache_matches = self.sequence_cache.as_ref().is_some_and(|cache| {
            cache.dataset_index == sequence_key.dataset_index
                && cache.start == sequence_key.start
                && cache.end == sequence_key.end
                && cache.channels == sequence_key.channels
        });
        if !cache_matches {
            self.start_sequence_worker(sequence_key);
            ui.label(self.t(UiText::CalculatingSequence));
            ui.ctx().request_repaint();
            return;
        }

        match self
            .sequence_cache
            .as_ref()
            .map(|cache| &cache.result)
            .expect("sequence cache checked above")
        {
            Ok(result) => {
                ui.label(format!(
                    "{} {}",
                    self.t(UiText::SamplesPrefix),
                    result.sample_count
                ));
                egui::Grid::new("sequence_table")
                    .striped(true)
                    .num_columns(4)
                    .spacing([10.0, 4.0])
                    .show(ui, |ui| {
                        Self::fixed_grid_label(
                            ui,
                            ANALYSIS_LABEL_COLUMN_WIDTH,
                            RichText::new(self.t(UiText::Component)).strong(),
                            false,
                            false,
                        );
                        Self::fixed_grid_label(
                            ui,
                            ANALYSIS_VALUE_COLUMN_WIDTH,
                            RichText::new(self.t(UiText::Amplitude)).strong(),
                            true,
                            false,
                        );
                        Self::fixed_grid_label(
                            ui,
                            ANALYSIS_VALUE_COLUMN_WIDTH,
                            RichText::new(self.t(UiText::Phase)).strong(),
                            true,
                            false,
                        );
                        Self::fixed_grid_label(
                            ui,
                            ANALYSIS_VALUE_COLUMN_WIDTH + 24.0,
                            RichText::new(self.t(UiText::PositiveRatio)).strong(),
                            true,
                            false,
                        );
                        ui.end_row();
                        let rows = [
                            (self.t(UiText::ZeroSequence), result.zero),
                            (self.t(UiText::PositiveSequence), result.positive),
                            (self.t(UiText::NegativeSequence), result.negative),
                        ];
                        for (label, component) in rows {
                            Self::fixed_grid_label(
                                ui,
                                ANALYSIS_LABEL_COLUMN_WIDTH,
                                label,
                                false,
                                false,
                            );
                            Self::fixed_grid_label(
                                ui,
                                ANALYSIS_VALUE_COLUMN_WIDTH,
                                format!("{:.6}", component.amplitude),
                                true,
                                false,
                            );
                            Self::fixed_grid_label(
                                ui,
                                ANALYSIS_VALUE_COLUMN_WIDTH,
                                format!("{:.2}", component.phase_deg),
                                true,
                                false,
                            );
                            Self::fixed_grid_label(
                                ui,
                                ANALYSIS_VALUE_COLUMN_WIDTH + 24.0,
                                format!("{:.2}%", component.relative_percent),
                                true,
                                false,
                            );
                            ui.end_row();
                        }
                    });
            }
            Err(error) => {
                ui.label(
                    RichText::new(self.localized_error_message(error)).color(Color32::LIGHT_RED),
                );
            }
        }
    }

    fn valid_three_phase_selection(channels: [usize; 3], options: &[usize]) -> bool {
        channels.iter().all(|channel| options.contains(channel))
            && !Self::triplet_has_duplicates(channels)
    }

    fn three_phase_channel_selectors_ui(
        ui: &mut egui::Ui,
        id_prefix: &'static str,
        dataset_index: usize,
        channel_options: &[(usize, String, Option<[usize; 3]>)],
        channels: &mut [usize; 3],
    ) -> bool {
        let phase_labels = ["A", "B", "C"];
        let mut changed = false;
        let layout = Self::three_phase_selector_layout(ui.available_width());
        let mut phase_selector = |ui: &mut egui::Ui, phase_index: usize, phase_label: &str| {
            ui.label(phase_label);
            let selected_name = channel_options
                .iter()
                .find(|(channel_index, _, _)| *channel_index == channels[phase_index])
                .map(|(_, name, _)| name.as_str())
                .unwrap_or("");
            let selected_short = Self::compact_label(selected_name, ANALYSIS_CHANNEL_LABEL_CHARS);
            let response = egui::ComboBox::from_id_source((id_prefix, dataset_index, phase_index))
                .width(Self::analysis_combo_width(ui.available_width()))
                .selected_text(selected_short)
                .show_ui(ui, |ui| {
                    for (channel_index, channel_name, related_channels) in channel_options {
                        let channel_short =
                            Self::compact_label(channel_name, ANALYSIS_CHANNEL_LABEL_CHARS + 8);
                        if ui
                            .selectable_value(
                                &mut channels[phase_index],
                                *channel_index,
                                channel_short,
                            )
                            .on_hover_text(channel_name)
                            .changed()
                        {
                            if phase_index == 0 {
                                if let Some(related) = related_channels {
                                    *channels = *related;
                                }
                            }
                            changed = true;
                        }
                    }
                });
            response.response.on_hover_text(selected_name);
        };
        match layout {
            ThreePhaseSelectorLayout::Horizontal => {
                ui.horizontal_wrapped(|ui| {
                    for (phase_index, phase_label) in phase_labels.iter().enumerate() {
                        phase_selector(ui, phase_index, phase_label);
                    }
                });
            }
            ThreePhaseSelectorLayout::Vertical => {
                for (phase_index, phase_label) in phase_labels.iter().enumerate() {
                    ui.horizontal(|ui| phase_selector(ui, phase_index, phase_label));
                }
            }
        }
        changed
    }

    fn pll_dq_panel(&mut self, ui: &mut egui::Ui, channel_options: &[usize]) {
        ui.separator();
        ui.heading(self.t(UiText::PllDq0Title));
        self.fft_dataset_index = self.selected_fft_dataset_index();
        if channel_options.len() < 3 {
            ui.label(self.t(UiText::PllNeedsThreeAnalogChannels));
            return;
        }

        if !Self::valid_three_phase_selection(self.pll_source_channels, channel_options) {
            self.pll_source_channels = self
                .preferred_pll_source_channels(channel_options)
                .or_else(|| Self::default_sequence_channels_from_options(channel_options))
                .unwrap_or([0, 1, 2]);
            self.needs_derived_reload = true;
        }
        let preferred = self.preferred_three_phase_channels(channel_options, true);
        if !Self::valid_three_phase_selection(self.dq_source_channels, channel_options)
            || (!self.dq_source_channels_user_selected && preferred.is_some())
        {
            self.dq_source_channels = preferred
                .or_else(|| Self::default_sequence_channels_from_options(channel_options))
                .unwrap_or(self.pll_source_channels);
            self.dq_source_channels_user_selected = false;
            self.needs_derived_reload = true;
        }

        let options = channel_options
            .iter()
            .map(|channel| {
                (
                    *channel,
                    self.fft_channel_name(self.fft_dataset_index, *channel),
                    self.related_three_phase_channels_from_anchor(*channel, channel_options),
                )
            })
            .collect::<Vec<_>>();
        let mut changed = false;
        ui.label(self.t(UiText::PllSyncSource));
        changed |= Self::three_phase_channel_selectors_ui(
            ui,
            "pll_source_channel",
            self.fft_dataset_index,
            &options,
            &mut self.pll_source_channels,
        );
        ui.label(self.t(UiText::Dq0Input));
        let dq_changed = Self::three_phase_channel_selectors_ui(
            ui,
            "dq_source_channel",
            self.fft_dataset_index,
            &options,
            &mut self.dq_source_channels,
        );
        if dq_changed {
            self.dq_source_channels_user_selected = true;
            changed = true;
        }

        if changed {
            self.derived_curve_cache = None;
            self.prepared_derived_curve_cache = PreparedPlotSeries::default();
            self.derived_measurement_cache = None;
            self.needs_derived_reload = true;
        }
        if Self::triplet_has_duplicates(self.pll_source_channels)
            || Self::triplet_has_duplicates(self.dq_source_channels)
        {
            ui.label(RichText::new(self.t(UiText::PllDistinctChannels)).color(Color32::LIGHT_RED));
        } else {
            let selected_count = self.selected_derived_channels().len();
            ui.label(if selected_count == 0 {
                self.t(UiText::SelectDerivedCurves)
            } else if self.derived_curve_worker.is_some() {
                self.t(UiText::CalculatingPllDq0)
            } else {
                self.t(UiText::PllDq0Enabled)
            });
        }
        self.derived_channels_panel_ui(ui);
    }

    fn fft_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("FFT");
        if self.meta().is_none() {
            ui.label(self.t(UiText::NoDataLoaded));
            return;
        }

        self.fft_dataset_index = self.selected_fft_dataset_index();

        let fft_channels = self.fft_channel_options();
        if fft_channels.is_empty() {
            self.fft_results.clear();
            self.needs_fft_reload = false;
            ui.label(self.t(UiText::NoAnalogFftChannels));
            return;
        }
        if !fft_channels.contains(&self.fft_channel) {
            self.fft_channel = self
                .preferred_fft_channel(&fft_channels)
                .unwrap_or(fft_channels[0]);
            self.fft_channel_user_selected = false;
            self.fft_results.clear();
            self.needs_fft_reload = true;
        }

        self.poll_fft_worker();
        if self.needs_fft_reload {
            self.run_fft();
        }
        if self.fft_worker.is_some() {
            ui.label(self.t(UiText::CalculatingFft));
            ui.ctx().request_repaint();
        }

        if let Some((channel_index, result)) = self.fft_results.first() {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "{} {}",
                    self.t(UiText::SamplesPrefix),
                    result.sample_count
                ));
                ui.label(
                    RichText::new(format!("THD {:.3}%", result.thd_percent))
                        .color(self.channel_color(*channel_index)),
                );
            });
            ui.separator();
            egui::Grid::new(("harmonics", *channel_index))
                .striped(true)
                .num_columns(4)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    Self::fixed_grid_label(
                        ui,
                        ANALYSIS_LABEL_COLUMN_WIDTH,
                        RichText::new(self.t(UiText::Order)).strong(),
                        false,
                        false,
                    );
                    Self::fixed_grid_label(
                        ui,
                        ANALYSIS_VALUE_COLUMN_WIDTH,
                        RichText::new(self.t(UiText::Amplitude)).strong(),
                        true,
                        false,
                    );
                    Self::fixed_grid_label(
                        ui,
                        ANALYSIS_VALUE_COLUMN_WIDTH,
                        RichText::new(self.t(UiText::Phase)).strong(),
                        true,
                        false,
                    );
                    Self::fixed_grid_label(
                        ui,
                        ANALYSIS_VALUE_COLUMN_WIDTH + 28.0,
                        RichText::new(self.t(UiText::FundamentalRatio)).strong(),
                        true,
                        false,
                    );
                    ui.end_row();
                    for row in &result.harmonics {
                        let order_text = if self.language == Language::Zh {
                            format!("{}", row.order)
                        } else {
                            row.order.to_string()
                        };
                        let phase_text = if row.order == 0 || !row.phase_deg.is_finite() {
                            "--".to_owned()
                        } else {
                            format!("{:.2}", row.phase_deg)
                        };
                        if row.order == 1 {
                            Self::fixed_grid_label(
                                ui,
                                ANALYSIS_LABEL_COLUMN_WIDTH,
                                RichText::new(order_text).strong(),
                                false,
                                false,
                            );
                            Self::fixed_grid_label(
                                ui,
                                ANALYSIS_VALUE_COLUMN_WIDTH,
                                RichText::new(format!("{:.6}", row.amplitude)).strong(),
                                true,
                                false,
                            );
                            Self::fixed_grid_label(
                                ui,
                                ANALYSIS_VALUE_COLUMN_WIDTH,
                                RichText::new(phase_text).strong(),
                                true,
                                false,
                            );
                            Self::fixed_grid_label(
                                ui,
                                ANALYSIS_VALUE_COLUMN_WIDTH + 28.0,
                                RichText::new(format!("{:.2}%", row.relative_percent)).strong(),
                                true,
                                false,
                            );
                        } else {
                            Self::fixed_grid_label(
                                ui,
                                ANALYSIS_LABEL_COLUMN_WIDTH,
                                order_text,
                                false,
                                false,
                            );
                            Self::fixed_grid_label(
                                ui,
                                ANALYSIS_VALUE_COLUMN_WIDTH,
                                format!("{:.6}", row.amplitude),
                                true,
                                false,
                            );
                            Self::fixed_grid_label(
                                ui,
                                ANALYSIS_VALUE_COLUMN_WIDTH,
                                phase_text,
                                true,
                                false,
                            );
                            Self::fixed_grid_label(
                                ui,
                                ANALYSIS_VALUE_COLUMN_WIDTH + 28.0,
                                format!("{:.2}%", row.relative_percent),
                                true,
                                false,
                            );
                        }
                        ui.end_row();
                    }
                });
        } else {
            ui.label(self.t(UiText::FftNeedsCursorSamples));
        }
        self.sequence_panel(ui, &fft_channels);
        self.pll_dq_panel(ui, &fft_channels);
    }

    fn plot_panel(&mut self, ui: &mut egui::Ui) {
        self.poll_plot_worker();
        self.poll_compare_plot_worker();
        let reload_ready = self.plot_reload_debounce_ready(ui.ctx());
        let layout_resize_active = self.layout_resize_active();
        let reload_allowed = Self::plot_reload_allowed(reload_ready, layout_resize_active);
        if reload_allowed {
            self.reload_derived_curve_cache();
        }
        if self.any_background_job_running() {
            ui.ctx().request_repaint();
        }
        let lightweight_plot = layout_resize_active || self.plot_interaction_debounce_active();
        if lightweight_plot {
            ui.ctx().request_repaint_after(LAYOUT_RESIZE_ACTIVE_GRACE);
        }

        let selections = self.current_plot_selections();
        let rows = self.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS);
        let cols = self.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS);
        let pane_count = rows * cols;
        self.active_scope_pane = self.active_scope_pane.min(pane_count.saturating_sub(1));
        self.sync_pane_y_bounds_len();
        let available = ui.available_size();
        let spacing = ui.spacing().item_spacing;
        let pane_width =
            ((available.x - spacing.x * (cols.saturating_sub(1) as f32)) / cols as f32).max(80.0);
        let pane_height = if pane_count <= 1 {
            available.y.max(260.0)
        } else {
            ((available.y.max(320.0) - spacing.y * (rows.saturating_sub(1) as f32)) / rows as f32)
                .max(140.0)
        };
        if reload_allowed && self.needs_plot_reload {
            self.reload_plot_cache(pane_width);
        }
        if reload_allowed && self.needs_compare_plot_reload {
            self.reload_compare_plot_cache(pane_width);
        }
        let pane_selections = self.pane_plot_selections(&selections, pane_count);
        self.ensure_pane_prepared_plot_cache(&pane_selections, pane_count);
        let y_bounds = self.current_y_bounds_for_panes_cached(&pane_selections, pane_count);

        if pane_count <= 1 {
            let pane_prepared = self
                .pane_prepared_plot_cache
                .panes
                .first()
                .cloned()
                .unwrap_or_default();
            self.draw_scope_pane(
                ui,
                0,
                1,
                &pane_selections[0],
                &pane_prepared,
                y_bounds[0],
                pane_width,
                pane_height,
                lightweight_plot,
            );
            return;
        }

        ui.vertical(|ui| {
            for row in 0..rows {
                ui.horizontal(|ui| {
                    for col in 0..cols {
                        let pane_index = row * cols + col;
                        let pane_prepared = self
                            .pane_prepared_plot_cache
                            .panes
                            .get(pane_index)
                            .cloned()
                            .unwrap_or_default();
                        ui.allocate_ui(egui::vec2(pane_width, pane_height), |ui| {
                            self.draw_scope_pane(
                                ui,
                                pane_index,
                                pane_count,
                                &pane_selections[pane_index],
                                &pane_prepared,
                                y_bounds[pane_index],
                                pane_width,
                                pane_height,
                                lightweight_plot,
                            );
                        });
                    }
                });
            }
        });
    }

    fn draw_scope_pane(
        &mut self,
        ui: &mut egui::Ui,
        pane_index: usize,
        pane_count: usize,
        selections: &PanePlotSelections,
        prepared: &PanePreparedPlotPane,
        y_bounds: (f64, f64),
        pane_width: f32,
        pane_height: f32,
        lightweight_plot: bool,
    ) {
        let (plot_y_min, plot_y_max) = y_bounds;
        let show_dataset_prefix =
            self.pane_has_dataset_comparison(selections, pane_index, pane_count);
        let legend_series_limit = if self.software_rendering_performance_mode() {
            SOFTWARE_MAX_SCOPE_LEGEND_SERIES
        } else {
            MAX_SCOPE_LEGEND_SERIES
        };
        let show_legend = (pane_count > 1 || show_dataset_prefix)
            && Self::pane_visible_series_count(selections) <= legend_series_limit;
        let grid_color = Self::scope_grid_color(ui);
        let mut plot = Plot::new(format!("scope_plot_{pane_index}"))
            .width(pane_width)
            .height(pane_height)
            .show_grid(false)
            .allow_drag(false)
            .allow_scroll(false)
            .allow_zoom(false);
        if show_legend {
            plot = plot.legend(Legend::default());
        }
        let response = plot.show(ui, |plot_ui| {
            plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                [self.view_start, plot_y_min],
                [self.view_end, plot_y_max],
            ));
            Self::draw_scope_dashed_grid(
                plot_ui,
                self.view_start,
                self.view_end,
                plot_y_min,
                plot_y_max,
                grid_color,
            );
            let preview_cursor_x = (!lightweight_plot)
                .then(|| {
                    self.cursor_place_mode
                        .and_then(|_| plot_ui.pointer_coordinate())
                        .map(|point| point.x)
                })
                .flatten();
            let mut preview_intersections = Vec::new();

            for (local_index, (_out_index, channel_index)) in selections.primary.iter().enumerate()
            {
                if local_index >= prepared.primary_samples.points.len() {
                    continue;
                }
                let Some(points) = prepared.primary_samples.points.get(local_index) else {
                    continue;
                };
                let channel_name = self.channel_name(*channel_index);
                let legend_name =
                    self.plot_legend_name(0, &channel_name, show_dataset_prefix, None);
                let line_color = self.plot_channel_color(*channel_index, 0, pane_index, pane_count);
                if let Some(y) = preview_cursor_x.and_then(|x| Self::interpolated_y(points, x)) {
                    preview_intersections.push((y, line_color));
                }
                plot_ui.line(
                    Line::new(Self::frame_plot_points(
                        points,
                        prepared.primary_samples.lightweight_points.get(local_index),
                        lightweight_plot,
                    ))
                    .name(legend_name)
                    .color(line_color)
                    .style(self.channel_line_pattern(*channel_index).plot_style())
                    .width(self.visible_line_width(*channel_index)),
                );
            }

            if let (Some(summary), Some(prepared_summary)) =
                (&self.plot_summary, &prepared.primary_summary)
            {
                for (local_index, (out_index, channel_index)) in
                    selections.primary.iter().enumerate()
                {
                    if local_index >= prepared_summary.points.len() {
                        continue;
                    }
                    let Some(envelope) = prepared_summary.points.get(local_index) else {
                        continue;
                    };
                    let channel_name = self.channel_name(*channel_index);
                    let legend_name = self.plot_envelope_legend_name(
                        0,
                        &channel_name,
                        show_dataset_prefix,
                        false,
                    );
                    let line_color =
                        self.plot_channel_color(*channel_index, 0, pane_index, pane_count);
                    if let Some(y) = preview_cursor_x.and_then(|x| {
                        self.summary_cursor_y(summary, *out_index, *channel_index, x, 0.0)
                    }) {
                        preview_intersections.push((y, line_color));
                    }
                    Self::plot_summary_envelope(
                        plot_ui,
                        SummaryEnvelopeDraw {
                            high_points: envelope,
                            high_lightweight_points: prepared_summary
                                .lightweight_points
                                .get(local_index),
                            low_points: prepared_summary.envelope_low_points.get(local_index),
                            low_lightweight_points: prepared_summary
                                .envelope_low_lightweight_points
                                .get(local_index),
                            lightweight: lightweight_plot,
                            legend_name,
                            line_color,
                            line_style: self.channel_line_pattern(*channel_index).plot_style(),
                            line_width: self.visible_line_width(*channel_index),
                        },
                    );
                }
            }

            for (dataset_index, dataset) in self.imported_datasets.iter().enumerate() {
                let compare_selected = selections
                    .imported
                    .get(dataset_index)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let prepared_imported = prepared.imported.get(dataset_index);
                let dataset_line_style = dataset.line_pattern.plot_style();
                let time_offset = self.dataset_time_offset(dataset_index + 1);
                for (local_index, (_out_index, channel_index)) in
                    compare_selected.iter().enumerate()
                {
                    let Some(prepared_imported) = prepared_imported else {
                        continue;
                    };
                    if local_index >= prepared_imported.samples.points.len() {
                        continue;
                    }
                    let Some(points) = prepared_imported.samples.points.get(local_index) else {
                        continue;
                    };
                    let channel_name = self.channel_name(*channel_index);
                    let legend_name = self.plot_legend_name(
                        dataset_index + 1,
                        &channel_name,
                        show_dataset_prefix,
                        None,
                    );
                    let line_color = self.plot_channel_color(
                        *channel_index,
                        dataset_index + 1,
                        pane_index,
                        pane_count,
                    );
                    if let Some(y) = preview_cursor_x.and_then(|x| Self::interpolated_y(points, x))
                    {
                        preview_intersections.push((y, line_color));
                    }
                    plot_ui.line(
                        Line::new(Self::frame_plot_points(
                            points,
                            prepared_imported
                                .samples
                                .lightweight_points
                                .get(local_index),
                            lightweight_plot,
                        ))
                        .name(legend_name)
                        .color(line_color)
                        .style(dataset_line_style)
                        .width(self.compare_line_width(*channel_index)),
                    );
                }

                if let (Some(summary), Some(prepared_imported)) =
                    (&dataset.plot_summary, prepared_imported)
                {
                    let Some(prepared_summary) = &prepared_imported.summary else {
                        continue;
                    };
                    for (local_index, (out_index, channel_index)) in
                        compare_selected.iter().enumerate()
                    {
                        if local_index >= prepared_summary.points.len() {
                            continue;
                        }
                        let Some(envelope) = prepared_summary.points.get(local_index) else {
                            continue;
                        };
                        let channel_name = self.channel_name(*channel_index);
                        let legend_name = self.plot_envelope_legend_name(
                            dataset_index + 1,
                            &channel_name,
                            show_dataset_prefix,
                            false,
                        );
                        let line_color = self.plot_channel_color(
                            *channel_index,
                            dataset_index + 1,
                            pane_index,
                            pane_count,
                        );
                        if let Some(y) = preview_cursor_x.and_then(|x| {
                            self.summary_cursor_y(
                                summary,
                                *out_index,
                                *channel_index,
                                x,
                                time_offset,
                            )
                        }) {
                            preview_intersections.push((y, line_color));
                        }
                        Self::plot_summary_envelope(
                            plot_ui,
                            SummaryEnvelopeDraw {
                                high_points: envelope,
                                high_lightweight_points: prepared_summary
                                    .lightweight_points
                                    .get(local_index),
                                low_points: prepared_summary.envelope_low_points.get(local_index),
                                low_lightweight_points: prepared_summary
                                    .envelope_low_lightweight_points
                                    .get(local_index),
                                lightweight: lightweight_plot,
                                legend_name,
                                line_color,
                                line_style: dataset_line_style,
                                line_width: self.compare_line_width(*channel_index),
                            },
                        );
                    }
                }
            }

            for (local_index, (_out_index, derived_index)) in selections.derived.iter().enumerate()
            {
                if local_index >= prepared.derived.points.len() {
                    continue;
                }
                let Some(points) = prepared.derived.points.get(local_index) else {
                    continue;
                };
                let line_color = self.derived_channel_color(*derived_index);
                if let Some(y) = preview_cursor_x.and_then(|x| Self::interpolated_y(points, x)) {
                    preview_intersections.push((y, line_color));
                }
                plot_ui.line(
                    Line::new(Self::frame_plot_points(
                        points,
                        prepared.derived.lightweight_points.get(local_index),
                        lightweight_plot,
                    ))
                    .name(self.derived_channel_name(*derived_index))
                    .color(line_color)
                    .style(self.derived_line_pattern(*derived_index).plot_style())
                    .width(DEFAULT_CHANNEL_LINE_WIDTH + 0.2),
                );
            }

            let cursor_label_y = plot_y_max - (plot_y_max - plot_y_min) * 0.05;
            if self.show_cursor_a {
                let color = Self::cursor_color(CursorId::A);
                plot_ui.vline(VLine::new(self.cursor_a).color(color).width(2.5));
                plot_ui.text(
                    Text::new(
                        PlotPoint::new(self.cursor_a, cursor_label_y),
                        RichText::new(Self::cursor_label(CursorId::A))
                            .strong()
                            .color(Color32::BLACK)
                            .background_color(color),
                    )
                    .anchor(egui::Align2::CENTER_TOP),
                );
            }
            if self.show_cursor_b {
                let color = Self::cursor_color(CursorId::B);
                plot_ui.vline(VLine::new(self.cursor_b).color(color).width(2.5));
                plot_ui.text(
                    Text::new(
                        PlotPoint::new(self.cursor_b, cursor_label_y),
                        RichText::new(Self::cursor_label(CursorId::B))
                            .strong()
                            .color(Color32::BLACK)
                            .background_color(color),
                    )
                    .anchor(egui::Align2::CENTER_TOP),
                );
            }

            if let (Some(cursor), Some(cursor_x)) = (self.cursor_place_mode, preview_cursor_x) {
                plot_ui.vline(
                    VLine::new(cursor_x)
                        .color(Self::cursor_color(cursor))
                        .style(LineStyle::Dashed { length: 6.0 })
                        .width(2.5),
                );
            }
            self.draw_preview_cursor_value_labels(
                plot_ui,
                preview_cursor_x,
                &preview_intersections,
            );
        });

        if pane_count > 1
            && response.response.hovered()
            && ui.ctx().input(|input| input.pointer.any_pressed())
        {
            self.set_active_scope_pane(pane_index);
        }
        if pane_count > 1 && self.current_scope_pane() == pane_index {
            ui.painter().rect_stroke(
                response.response.rect.expand(1.0),
                0.0,
                Stroke::new(2.0, Color32::from_rgb(25, 130, 220)),
            );
        }

        let hover_time = response
            .response
            .hover_pos()
            .map(|pos| response.transform.value_from_position(pos).x);

        response.response.context_menu(|ui| {
            if ui.button(self.t(UiText::PlaceCursorX1)).clicked() {
                self.cursor_place_mode = Some(CursorId::A);
                self.zoom_box_start = None;
                self.zoom_box_current = None;
                ui.close_menu();
            }
            if ui.button(self.t(UiText::PlaceCursorX2)).clicked() {
                self.cursor_place_mode = Some(CursorId::B);
                self.zoom_box_start = None;
                self.zoom_box_current = None;
                ui.close_menu();
            }
            if self.cursor_place_mode.is_some()
                && ui.button(self.t(UiText::CancelPlacement)).clicked()
            {
                self.cursor_place_mode = None;
                ui.close_menu();
            }
            ui.separator();
            if self.show_cursor_a {
                if ui.button(self.t(UiText::HideCursorX1)).clicked() {
                    self.show_cursor_a = false;
                    ui.close_menu();
                }
            } else if ui.button(self.t(UiText::ShowCursorX1)).clicked() {
                self.show_cursor_a = true;
                ui.close_menu();
            }
            if self.show_cursor_b {
                if ui.button(self.t(UiText::HideCursorX2)).clicked() {
                    self.show_cursor_b = false;
                    ui.close_menu();
                }
            } else if ui.button(self.t(UiText::ShowCursorX2)).clicked() {
                self.show_cursor_b = true;
                ui.close_menu();
            }
            let dataset_items = (0..=self.imported_datasets.len())
                .filter_map(|dataset_index| {
                    self.cursor_export_range_for_dataset(dataset_index)
                        .map(|_| (dataset_index, self.dataset_label(dataset_index)))
                })
                .collect::<Vec<_>>();
            if !dataset_items.is_empty() {
                ui.separator();
                ui.menu_button(self.t(UiText::ExportCursorRangeData), |ui| {
                    if self.hover_in_cursor_range(hover_time) {
                        let all_indices = dataset_items
                            .iter()
                            .map(|(dataset_index, _)| *dataset_index)
                            .collect::<Vec<_>>();
                        ui.menu_button(self.t(UiText::AllDatasets), |ui| {
                            self.cursor_export_batch_menu(ui, &all_indices, true);
                            ui.separator();
                            self.cursor_export_batch_menu(ui, &all_indices, false);
                        });
                        ui.separator();
                        for (dataset_index, label) in &dataset_items {
                            ui.menu_button(label, |ui| {
                                self.cursor_export_dataset_menu(ui, *dataset_index, true);
                                ui.separator();
                                self.cursor_export_dataset_menu(ui, *dataset_index, false);
                            });
                        }
                    } else {
                        ui.label(self.t(UiText::CursorExportHint));
                        ui.separator();
                        let all_indices = dataset_items
                            .iter()
                            .map(|(dataset_index, _)| *dataset_index)
                            .collect::<Vec<_>>();
                        ui.menu_button(self.t(UiText::AllDatasets), |ui| {
                            self.cursor_export_batch_menu(ui, &all_indices, true);
                            ui.separator();
                            self.cursor_export_batch_menu(ui, &all_indices, false);
                        });
                        ui.separator();
                        for (dataset_index, label) in &dataset_items {
                            ui.menu_button(label, |ui| {
                                self.cursor_export_dataset_menu(ui, *dataset_index, true);
                                ui.separator();
                                self.cursor_export_dataset_menu(ui, *dataset_index, false);
                            });
                        }
                    }
                });
            }
        });

        if ui.ctx().input(|input| input.key_pressed(egui::Key::Escape)) {
            self.cursor_place_mode = None;
            self.zoom_box_start = None;
            self.zoom_box_current = None;
        }

        if response.response.hovered() {
            let (raw_scroll, smooth_scroll, ctrl_down, zoom_delta) = ui.ctx().input(|input| {
                (
                    input.raw_scroll_delta.y,
                    input.smooth_scroll_delta.y,
                    input.modifiers.ctrl,
                    input.zoom_delta(),
                )
            });
            let scroll = if raw_scroll.abs() > 0.0 {
                raw_scroll
            } else {
                smooth_scroll
            };
            let center_x = hover_time.unwrap_or((self.view_start + self.view_end) * 0.5);
            if let Some(factor) = self.pointer_zoom_factor(scroll, zoom_delta) {
                let center_y = response
                    .response
                    .hover_pos()
                    .map(|pos| response.transform.value_from_position(pos).y)
                    .unwrap_or((plot_y_min + plot_y_max) * 0.5);
                if ctrl_down {
                    self.zoom(center_x, factor);
                    if pane_count > 1 {
                        self.zoom_y_with_bounds(
                            pane_index, pane_count, center_y, factor, plot_y_min, plot_y_max,
                        );
                    }
                } else {
                    self.zoom_y_with_bounds(
                        pane_index, pane_count, center_y, factor, plot_y_min, plot_y_max,
                    );
                }
                ui.ctx().request_repaint();
            }

            let drag_delta = response.response.drag_delta();
            if response.response.dragged_by(PointerButton::Secondary) && drag_delta.x.abs() > 0.0 {
                let time_per_pixel =
                    self.visible_time_span() / response.response.rect.width() as f64;
                self.pan(-(drag_delta.x as f64) * time_per_pixel);
                ui.ctx().request_repaint();
            }
        }

        if response.response.clicked_by(PointerButton::Primary) {
            if let Some(time) = hover_time {
                if let Some(cursor) = self.cursor_place_mode.take() {
                    self.active_cursor = cursor;
                    self.set_cursor(cursor, time);
                } else {
                    let distance_a = (time - self.cursor_a).abs();
                    let distance_b = (time - self.cursor_b).abs();
                    self.active_cursor = if distance_a <= distance_b {
                        CursorId::A
                    } else {
                        CursorId::B
                    };
                    self.move_active_cursor(time);
                }
            }
        }

        if self.cursor_place_mode.is_none()
            && response.response.drag_started_by(PointerButton::Primary)
        {
            self.zoom_box_start = response.response.interact_pointer_pos();
            self.zoom_box_current = self.zoom_box_start;
        }

        if self.cursor_place_mode.is_none() && response.response.dragged_by(PointerButton::Primary)
        {
            self.zoom_box_current = response.response.interact_pointer_pos();
            if let (Some(start), Some(current)) = (self.zoom_box_start, self.zoom_box_current) {
                let start = Self::clamp_to_plot_rect(start, response.response.rect);
                let current = Self::clamp_to_plot_rect(current, response.response.rect);
                if (current.x - start.x).abs() >= ZOOM_BOX_MIN_PIXELS {
                    let rect = egui::Rect::from_two_pos(start, current);
                    ui.painter().rect_filled(
                        rect,
                        0.0,
                        Color32::from_rgba_premultiplied(80, 150, 255, 28),
                    );
                    ui.painter().rect_stroke(
                        rect,
                        0.0,
                        Stroke::new(1.0, Color32::from_rgb(80, 170, 255)),
                    );
                    ui.ctx().request_repaint();
                }
            }
        }

        if self.cursor_place_mode.is_none()
            && response.response.drag_stopped_by(PointerButton::Primary)
        {
            if let (Some(start), Some(end)) =
                (self.zoom_box_start.take(), self.zoom_box_current.take())
            {
                let start = Self::clamp_to_plot_rect(start, response.response.rect);
                let end = Self::clamp_to_plot_rect(end, response.response.rect);
                if (end.x - start.x).abs() >= ZOOM_BOX_MIN_PIXELS {
                    let start_plot = response.transform.value_from_position(start);
                    let end_plot = response.transform.value_from_position(end);
                    self.zoom_to_range(start_plot.x, end_plot.x);
                }
            }
        }
    }

    fn update_inner(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.live.poll();
        self.poll_import_worker();
        if self.any_background_job_running()
            || self.live.connection_state
                != scope_analyzer::live::session::ConnectionState::Disconnected
        {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        self.sync_channel_state_lengths();
        self.apply_theme(ctx);
        if self.live.workspace_mode == scope_analyzer::live::state::WorkspaceMode::Offline {
            self.handle_shortcuts(ctx);
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| self.top_bar(ui));
        self.error_banner(ctx);
        self.help_window(ctx);
        self.options_window(ctx);
        if self.live.workspace_mode == scope_analyzer::live::state::WorkspaceMode::Live {
            self.live_workspace(ctx);
            return;
        }
        self.export_preview_window(ctx);
        self.batch_export_window(ctx);

        let window_width = ctx.screen_rect().width();
        let sidebar_ranges = if self.show_channel_panel && self.show_analysis_panel {
            Self::sidebar_width_ranges(window_width)
        } else {
            Self::sidebar_width_ranges_for_visibility(
                window_width,
                self.show_channel_panel,
                self.show_analysis_panel,
            )
        };
        let channel_panel_width = if self.show_channel_panel {
            let channel_panel_response = egui::SidePanel::left("channels")
                .resizable(true)
                .default_width(CHANNEL_PANEL_DEFAULT_WIDTH)
                .width_range(sidebar_ranges.channel.clone())
                .show(ctx, |ui| self.channel_panel(ui));
            channel_panel_response.response.rect.width()
        } else {
            0.0
        };

        let analysis_panel_width = if self.show_analysis_panel {
            let analysis_panel_response = egui::SidePanel::right("analysis")
                .resizable(true)
                .default_width(ANALYSIS_PANEL_DEFAULT_WIDTH)
                .width_range(sidebar_ranges.analysis.clone())
                .show(ctx, |ui| {
                    egui::ScrollArea::both()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_min_width(
                                ui.available_width().max(ANALYSIS_PANEL_CONTENT_MIN_WIDTH),
                            );
                            self.analysis_dataset_selector(ui);
                            ui.separator();
                            self.measurements_panel(ui);
                            ui.separator();
                            self.fft_panel(ui);
                        });
                });
            analysis_panel_response.response.rect.width()
        } else {
            0.0
        };
        self.observe_layout_panel_widths(channel_panel_width, analysis_panel_width);

        egui::CentralPanel::default().show(ctx, |ui| {
            self.plot_panel(ui);
        });
    }
}

impl eframe::App for ScopeApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| {
            self.update_inner(ctx, frame);
        })) {
            let message = Self::panic_payload_message(payload.as_ref());
            Self::append_crash_log(&format!("recovered UI panic: {message}"));
            self.last_error = Some(
                "An internal UI error was caught. The app is still running; please save your work and restart."
                    .to_owned(),
            );
            self.zoom_box_start = None;
            self.zoom_box_current = None;
            self.cursor_place_mode = None;
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::File, io::Write};

    #[test]
    fn scope_app_live_state_defaults_to_offline_workspace() {
        let live = ScopeApp::default_live_state();

        assert_eq!(
            live.workspace_mode,
            scope_analyzer::live::state::WorkspaceMode::Offline
        );
    }

    #[test]
    fn opens_scope_recording_through_the_application_dispatch_path() {
        use scope_analyzer::live::{
            protocol::{
                ChannelDescriptor, ChannelKind, ChannelTable, Frame, Message, SampleBatch,
                WireFormat, MSG_SAMPLE_BATCH,
            },
            recording::{RecordingMetadata, ScopeWriter},
        };

        let dir = unique_test_dir("scope_dispatch");
        let path = dir.join("capture.scope");
        let metadata = RecordingMetadata {
            device_id: "app-test".to_owned(),
            firmware_name: "test".to_owned(),
            tick_hz: 1_000,
            channel_table: ChannelTable {
                revision: 1,
                channels: vec![ChannelDescriptor {
                    channel_id: 0,
                    kind: ChannelKind::Analog,
                    wire_format: WireFormat::I16,
                    scale: 0.5,
                    offset: 1.0,
                    unit: "V".to_owned(),
                    name: "DSP-A".to_owned(),
                }],
            },
            sample_rate_hz: 10,
            batch_samples: 2,
            channel_mask: 1,
            client_version: "app-test".to_owned(),
        };
        let payload = Message::SampleBatch(SampleBatch {
            channel_table_revision: 1,
            first_sample_index: 0,
            sample_period_ticks: 100,
            sample_count: 2,
            channel_ids: vec![0],
            sample_data: [2_i16.to_le_bytes(), 4_i16.to_le_bytes()].concat(),
        })
        .encode_payload()
        .unwrap();
        let frame = Frame::new(MSG_SAMPLE_BATCH, 0, 1, 7, 0, payload);
        let mut writer = ScopeWriter::create(&path, metadata).unwrap();
        writer.write_sample_frame(&frame).unwrap();
        writer.finish().unwrap();

        let opened = ScopeApp::open_waveform_file_with_cancel(&path, 48_000.0, None).unwrap();

        assert_eq!(opened.kind, SourceKind::Scope);
        assert_eq!(opened.source.metadata().source_name, "capture.scope");
        assert_eq!(opened.source.metadata().channels[0].name, "DSP-A");
        let block = opened.source.read_range(0.0, 0.1, &[0], 10).unwrap();
        assert_eq!(block.channels[0], vec![2.0, 3.0]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn opens_indexed_adata_ddata_pair_without_sequence_column_and_merges_first_three_bits() {
        let dir = unique_test_dir("indexed_pair");
        let analog_path = dir.join("Tab1_ADATA.csv");
        let digital_path = dir.join("Tab1_DDATA.csv");

        let mut analog = File::create(&analog_path).unwrap();
        writeln!(analog, "NUM,ACH1,ACH2,ACH3").unwrap();
        writeln!(analog, "1,9747,4924,9527").unwrap();
        writeln!(analog, "2,9747,4940,9527").unwrap();
        drop(analog);

        let mut digital = File::create(&digital_path).unwrap();
        writeln!(digital, "Num,DCH1,DCH2,DCH3,DCH4,DCH5").unwrap();
        writeln!(digital, "1,1,1,0,0,1").unwrap();
        writeln!(digital, "2,0,1,1,1,0").unwrap();
        drop(digital);

        let opened = ScopeApp::open_local_csv_pair(&analog_path, &digital_path, 1000.0).unwrap();
        let meta = opened.source.metadata();
        assert_eq!(meta.channels.len(), 6);
        assert_eq!(meta.channels[0].name, "ACH1");
        assert_eq!(meta.channels[2].name, "ACH3");
        assert_eq!(meta.channels[3].name, "DCH1_DCH3");
        assert_eq!(meta.channels[4].name, "DCH4");
        assert_eq!(meta.channels[5].name, "DCH5");

        let block = opened
            .source
            .read_range(0.0, 0.001, &[0, 3, 4, 5], 10)
            .unwrap();
        assert_eq!(block.channels[0], vec![9747.0, 9747.0]);
        assert_eq!(block.channels[1], vec![3.0, 6.0]);
        assert_eq!(block.channels[2], vec![0.0, 1.0]);
        assert_eq!(block.channels[3], vec![1.0, 0.0]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn indexed_local_csv_pair_uses_cloud_variable_names() {
        let dir = unique_test_dir("cloud_names");
        let analog_path = dir.join("Names_ADATA.csv");
        let digital_path = dir.join("Names_DDATA.csv");

        let mut analog = File::create(&analog_path).unwrap();
        write!(analog, "NUM").unwrap();
        for index in 1..=30 {
            write!(analog, ",ACH{index}").unwrap();
        }
        writeln!(analog).unwrap();
        write!(analog, "1").unwrap();
        for index in 1..=30 {
            write!(analog, ",{index}").unwrap();
        }
        writeln!(analog).unwrap();
        drop(analog);

        let mut digital = File::create(&digital_path).unwrap();
        write!(digital, "Num").unwrap();
        for index in 1..=32 {
            write!(digital, ",DCH{index}").unwrap();
        }
        writeln!(digital).unwrap();
        write!(digital, "1").unwrap();
        for index in 1..=32 {
            write!(digital, ",{}", index % 2).unwrap();
        }
        writeln!(digital).unwrap();
        drop(digital);

        let opened = ScopeApp::open_local_csv_pair(&analog_path, &digital_path, 1000.0).unwrap();
        let meta = opened.source.metadata();
        assert_eq!(meta.channels.len(), 60);
        assert_eq!(meta.channels[0].name, "stVbus_0.iVal");
        assert_eq!(meta.channels[29].name, "stPIIBuckboost_B.iRef");
        assert_eq!(meta.channels[30].name, "LogicStsWord1.GPUOnOffSt");
        assert_eq!(meta.channels[31].name, "LogicStsWord1.Fault");
        assert_eq!(meta.channels[59].name, "unFaultFlag.GenFault");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn single_indexed_local_csv_discovers_counterpart_by_modified_time() {
        let dir = unique_test_dir("single_pair");
        let analog_path = dir.join("Single_Tab1_ADATA.csv");
        let digital_path = dir.join("Single_Tab1_DDATA.csv");

        write_indexed_analog_csv(&analog_path, &[111.0, 222.0]);
        write_indexed_digital_csv(&digital_path, &[(1, 0, 1, 1), (0, 1, 1, 0)]);

        let (opened, errors) = ScopeApp::open_waveform_files(vec![analog_path.clone()], 1000.0);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].source.metadata().channels.len(), 3);

        let block = opened[0]
            .source
            .read_range(0.0, 0.001, &[0, 1, 2], 10)
            .unwrap();
        assert_eq!(block.channels[0], vec![111.0, 222.0]);
        assert_eq!(block.channels[1], vec![5.0, 6.0]);
        assert_eq!(block.channels[2], vec![1.0, 0.0]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn batch_indexed_local_csv_pairs_by_filename_timestamp_before_name_key() {
        let dir = unique_test_dir("batch_time_pair");
        let a1 = dir.join("alpha_20260602010101_ADATA.csv");
        let d1 = dir.join("beta_20260602010101_DDATA.csv");
        let a2 = dir.join("gamma_20260602020202_ADATA.csv");
        let d2 = dir.join("delta_20260602020202_DDATA.csv");

        write_indexed_analog_csv(&a1, &[10.0, 20.0]);
        write_indexed_digital_csv(&d1, &[(1, 1, 0, 0), (0, 0, 1, 1)]);
        write_indexed_analog_csv(&a2, &[30.0, 40.0]);
        write_indexed_digital_csv(&d2, &[(0, 1, 0, 1), (1, 0, 1, 0)]);

        let (opened, errors) = ScopeApp::open_waveform_files(
            vec![a1.clone(), d2.clone(), a2.clone(), d1.clone()],
            1000.0,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(opened.len(), 2);
        assert!(opened
            .iter()
            .all(|dataset| dataset.source.metadata().channels.len() == 3));

        let merged_values = opened
            .iter()
            .map(|dataset| {
                dataset
                    .source
                    .read_range(0.0, 0.001, &[1], 10)
                    .unwrap()
                    .channels
                    .remove(0)
            })
            .collect::<Vec<_>>();
        assert!(merged_values.contains(&vec![3.0, 4.0]));
        assert!(merged_values.contains(&vec![2.0, 5.0]));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn duplicate_selected_local_csv_is_ignored_when_counterpart_is_discovered() {
        let dir = unique_test_dir("duplicate_pair");
        let analog_path = dir.join("Duplicate_Tab1_ADATA.csv");
        let digital_path = dir.join("Duplicate_Tab1_DDATA.csv");

        write_indexed_analog_csv(&analog_path, &[111.0, 222.0]);
        write_indexed_digital_csv(&digital_path, &[(1, 0, 1, 1), (0, 1, 1, 0)]);

        let (opened, errors) =
            ScopeApp::open_waveform_files(vec![analog_path.clone(), analog_path.clone()], 1000.0);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].source.metadata().channels.len(), 3);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recent_file_label_keeps_menu_compact() {
        let path = PathBuf::from(r"C:\Long\Scope\Data\waveform.csv");
        assert_eq!(ScopeApp::recent_file_label(&path), "waveform.csv");
    }

    #[test]
    fn import_finished_message_is_localized_summary() {
        assert_eq!(
            ScopeApp::import_finished_message(2, 0, Language::Zh),
            "已导入 2 个数据组。"
        );
        assert_eq!(
            ScopeApp::import_finished_message(1, 3, Language::Zh),
            "已导入 1 个数据组，3 个文件失败。"
        );
        assert_eq!(
            ScopeApp::import_finished_message(0, 2, Language::En),
            "Import finished: 0 datasets imported, 2 files failed."
        );
    }

    #[test]
    fn export_menu_labels_are_range_and_channel_oriented() {
        assert_eq!(UiText::Export.get(Language::Zh), "导出");
        assert_eq!(UiText::ExportData.get(Language::Zh), "导出数据");
        assert_eq!(
            UiText::ExportWaveformImage.get(Language::Zh),
            "导出波形图片"
        );
        assert_eq!(UiText::BatchExport.get(Language::Zh), "批量导出");
        assert_eq!(UiText::ExportAllRange.get(Language::Zh), "全部数据");
        assert_eq!(
            UiText::ExportCursorRangeData.get(Language::Zh),
            "光标 X1-X2"
        );
        assert_eq!(
            UiText::ExportCheckedChannels.get(Language::Zh),
            "已勾选通道"
        );
        assert_eq!(UiText::ExportAllChannels.get(Language::Zh), "全部通道");
        assert_eq!(UiText::ExportAllFormats.get(Language::Zh), "全部格式...");

        assert_eq!(UiText::Export.get(Language::En), "Export");
        assert_eq!(
            UiText::ExportWaveformImage.get(Language::En),
            "Export Waveform Image"
        );
        assert_eq!(UiText::BatchExport.get(Language::En), "Batch Export");
        assert_eq!(
            UiText::ExportCursorRangeData.get(Language::En),
            "Cursor X1-X2"
        );
    }

    #[test]
    fn export_status_messages_are_localized() {
        assert_eq!(
            ScopeApp::export_finished_message(12, Language::Zh),
            "已导出 12 行。"
        );
        assert_eq!(
            ScopeApp::export_cursor_finished_message(5, Language::Zh),
            "已导出光标区间数据 5 行。"
        );
        assert_eq!(
            ScopeApp::export_failed_message("磁盘已满", Language::Zh),
            "导出失败: 磁盘已满"
        );
        assert_eq!(
            ScopeApp::export_cursor_failed_message("磁盘已满", Language::Zh),
            "导出光标区间失败: 磁盘已满"
        );
    }

    #[test]
    fn config_files_are_tagged_by_section() {
        let json = ScopeApp::serialize_config_file(
            ConfigSection::Names,
            &NamesConfig {
                display_names: vec!["电网电压".to_owned()],
            },
        )
        .unwrap();

        assert!(json.contains("\"scope_config_type\": \"names\""));
        let decoded: NamesConfig =
            ScopeApp::deserialize_config_file(ConfigSection::Names, &json).unwrap();
        assert_eq!(decoded.display_names, vec!["电网电压"]);
    }

    #[test]
    fn config_import_rejects_wrong_or_ambiguous_section() {
        let display_json = ScopeApp::serialize_config_file(
            ConfigSection::Display,
            &DisplayConfig {
                channel_colors: vec![[1, 2, 3, 255]],
                ..DisplayConfig::default()
            },
        )
        .unwrap();

        let shortcut_result = ScopeApp::deserialize_config_file::<ShortcutConfig>(
            ConfigSection::Shortcut,
            &display_json,
        );
        assert!(shortcut_result.is_err());

        let ambiguous_json = r#"{"display_names":["A"]}"#;
        let shortcut_result = ScopeApp::deserialize_config_file::<ShortcutConfig>(
            ConfigSection::Shortcut,
            ambiguous_json,
        );
        assert!(shortcut_result.is_err());
    }

    #[test]
    fn config_recent_files_are_split_by_section() {
        assert_eq!(
            ConfigSection::Names.recent_file_name(),
            "scope-recent-names.json"
        );
        assert_eq!(
            ConfigSection::Display.recent_file_name(),
            "scope-recent-display-configs.json"
        );
        assert_eq!(
            ConfigSection::Shortcut.recent_file_name(),
            "scope-recent-shortcut-configs.json"
        );
        assert_eq!(
            ConfigSection::Dataset.recent_file_name(),
            "scope-recent-dataset-configs.json"
        );
    }

    #[test]
    fn config_sections_define_default_files_and_labels() {
        assert_eq!(ConfigSection::Names.default_file_name(), "scope-names.json");
        assert_eq!(
            ConfigSection::Display.default_file_name(),
            "scope-display.json"
        );
        assert_eq!(
            ConfigSection::Shortcut.default_file_name(),
            "scope-shortcuts.json"
        );
        assert_eq!(
            ConfigSection::Dataset.default_file_name(),
            "scope-datasets.json"
        );
        assert_eq!(ConfigSection::Names.label(Language::Zh), "变量名配置");
        assert_eq!(
            ConfigSection::Dataset.label(Language::En),
            "Dataset Settings"
        );
    }

    #[test]
    fn config_success_messages_are_localized_by_section() {
        assert_eq!(
            ConfigSection::Display.exported_message(Language::Zh),
            "已导出显示配置。"
        );
        assert_eq!(
            ConfigSection::Shortcut.imported_message(Language::Zh),
            "已导入快捷键配置。"
        );
        assert_eq!(
            ConfigSection::Dataset.exported_message(Language::En),
            "Dataset settings exported."
        );
    }

    #[test]
    fn matching_legacy_config_files_still_import() {
        let names: NamesConfig =
            ScopeApp::deserialize_config_file(ConfigSection::Names, r#"{"display_names":["A"]}"#)
                .unwrap();
        assert_eq!(names.display_names, vec!["A"]);

        let shortcuts: ShortcutConfig = ScopeApp::deserialize_config_file(
            ConfigSection::Shortcut,
            r#"{"reset_view":{"ctrl":true,"alt":false,"shift":false,"key":"R"}}"#,
        )
        .unwrap();
        assert_eq!(shortcuts.reset_view.label(), "Ctrl+R");
    }

    #[test]
    fn real_dat_samples_read_zoom_and_dense_summary() {
        let files = [
            "Data/20250214_140409.dat",
            "Data/20250214_140850.dat",
            "Data/20250214_140918.dat",
        ];

        for file in files {
            let path = sample_data_path(file);
            if !path.exists() {
                continue;
            }
            let source = DatDataSource::open(&path).unwrap();
            let meta = source.metadata();
            let start_time = meta.start_time;
            let end_time = meta.end_time;
            assert!(meta.sample_count > 0, "{file}");
            assert!(meta.channels.len() >= 7, "{file}");
            assert!(
                meta.channels
                    .iter()
                    .any(|channel| channel.name.contains("电网")),
                "{file}"
            );

            let channels = [3, 4, 5, 6];
            let zoom_end = (start_time + 1.0).min(end_time);
            let zoom = source
                .read_range(start_time, zoom_end, &channels, 10_000)
                .unwrap();
            assert!(!zoom.times.is_empty(), "{file}");
            assert_eq!(zoom.channels.len(), channels.len(), "{file}");
            assert!(
                zoom.channels
                    .iter()
                    .flatten()
                    .all(|value| value.is_finite()),
                "{file}"
            );

            let summary = source
                .summarize_range(start_time, end_time, &channels, 1200)
                .unwrap();
            let expected_min_bins = meta.sample_count.min(900) as usize;
            assert!(
                summary.bin_start.len() >= expected_min_bins,
                "{file}: summary bins {} < {expected_min_bins}",
                summary.bin_start.len()
            );
            assert_summary_is_finite(&summary, channels.len(), file);

            let plot_data =
                ScopeApp::load_plot_data(Arc::new(source), start_time, end_time, &[3], 1, 1200.0)
                    .unwrap();
            assert!(
                matches!(plot_data, Some(PlotJobData::Samples(_))),
                "{file}: medium DAT full view should use raw sampled plot data"
            );
        }
    }

    #[test]
    fn digital_channel_name_shortens_to_suffix_only_when_width_is_tight() {
        let name = "LogicStsWord2.BatteryAReady";
        assert_eq!(
            ScopeApp::channel_panel_display_name(name, true, 96.0),
            "BatteryAReady"
        );
        assert_eq!(
            ScopeApp::channel_panel_display_name(name, true, 320.0),
            name
        );
        assert_eq!(
            ScopeApp::channel_panel_display_name(name, false, 96.0),
            name
        );
        assert_eq!(
            ScopeApp::channel_panel_display_name("Fault", true, 40.0),
            "Fault"
        );
    }

    #[test]
    fn channel_names_are_hidden_when_left_sidebar_is_very_narrow() {
        assert_eq!(
            ScopeApp::channel_panel_display_text("stVbus_0.iVal", false, 32.0),
            None
        );
        assert_eq!(
            ScopeApp::channel_panel_display_text("LogicStsWord2.BatteryAReady", true, 70.0),
            Some("BatteryAReady".to_owned())
        );
        assert_eq!(
            ScopeApp::channel_panel_display_text("stVbus_0.iVal", false, 140.0),
            Some("stVbus_0.iVal".to_owned())
        );
    }

    #[test]
    fn channel_filter_shrinks_and_hides_clear_button_when_sidebar_is_narrow() {
        assert_eq!(ScopeApp::channel_filter_width(60.0, true), (60.0, false));
        assert_eq!(ScopeApp::channel_filter_width(36.0, true), (36.0, false));
        assert_eq!(ScopeApp::channel_filter_width(120.0, true), (74.0, true));
    }

    #[test]
    fn sidebar_headers_fall_back_to_short_labels_when_narrow() {
        let full = "数据A 105329025C120033_mc02_21_20260507192102_wave_data";
        assert_eq!(ScopeApp::sidebar_header_label(full, "数据A", 80.0), "数据A");
        let compact = ScopeApp::sidebar_header_label(full, "数据A", 180.0);
        assert!(compact.starts_with("数据A"));
        assert!(compact.ends_with("..."));
        assert!(compact.len() < full.len());
    }

    #[test]
    fn sidebar_shortcuts_default_to_vscode_style_bindings() {
        let shortcuts = ShortcutConfig::default();
        assert_eq!(shortcuts.toggle_channel_panel.label(), "Ctrl+B");
        assert_eq!(shortcuts.toggle_analysis_panel.label(), "Ctrl+Alt+B");
    }

    #[test]
    fn default_language_is_chinese() {
        assert_eq!(default_language(), Language::Zh);
    }

    #[test]
    fn cjk_font_search_includes_cross_platform_fallbacks() {
        let candidates = ScopeApp::cjk_font_candidates();

        assert!(candidates
            .iter()
            .any(|path| path.ends_with("NotoSansCJK-Regular.ttc")));
        assert!(candidates
            .iter()
            .any(|path| path.ends_with("Arial Unicode.ttf")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_installation_has_a_loadable_cjk_font() {
        let (name, bytes) = ScopeApp::load_cjk_font().expect("macOS CJK font fallback");

        assert!(name.starts_with("scope-cjk-"));
        assert!(bytes.len() > 1_000_000);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn installed_macos_font_renders_live_scope_chinese_labels() {
        let ctx = egui::Context::default();
        ScopeApp::install_cjk_fonts(&ctx);
        ctx.begin_frame(egui::RawInput::default());

        ctx.fonts(|fonts| {
            assert!(fonts.has_glyphs(&egui::FontId::proportional(14.0), "中文实时触发录波回放"));
        });
        let _ = ctx.end_frame();
    }

    #[test]
    fn export_controls_use_readable_chinese_labels() {
        assert_eq!(UiText::UiLanguage.get(Language::Zh), "界面语言");
        assert_eq!(UiText::ImageExportLabels.get(Language::Zh), "导出图片标注");
        assert_eq!(UiText::Config.get(Language::Zh), "配置");
        assert_eq!(UiText::NameConfig.get(Language::Zh), "变量名配置");
        assert_eq!(UiText::DisplayConfig.get(Language::Zh), "显示配置");
        assert_eq!(UiText::ShortcutConfig.get(Language::Zh), "快捷键配置");
        assert_eq!(UiText::DatasetConfig.get(Language::Zh), "数据组配置");
        assert_eq!(UiText::ExportQuality.get(Language::Zh), "导出质量");
        assert_eq!(UiText::ImageFormat.get(Language::Zh), "图片格式");
        assert_eq!(UiText::ShowAnnotations.get(Language::Zh), "显示标注");
        assert_eq!(UiText::CursorDataTable.get(Language::Zh), "显示光标数据表");
        assert_eq!(UiText::PllDq0Title.get(Language::Zh), "PLL/dq0");
        assert_eq!(ExportPaneScope::All.label(Language::Zh), "全部子窗口");
        assert_eq!(ExportTimeRangeMode::View.label(Language::Zh), "当前视图");
        assert_eq!(
            ExportTimeRangeMode::Cursor.label(Language::Zh),
            "光标 X1-X2"
        );
        assert_eq!(ExportTimeRangeMode::Manual.label(Language::Zh), "手动范围");
        assert_eq!(ExportArrowLineStyle::Solid.label(Language::Zh), "箭头实线");
        assert_eq!(
            ExportArrowLineStyle::Thick.label(Language::En),
            "➜ Thick arrow"
        );
    }

    #[test]
    fn analysis_combo_width_never_exceeds_available_width() {
        assert_eq!(ScopeApp::analysis_combo_width(72.0), 72.0);
        assert_eq!(ScopeApp::analysis_combo_width(175.0), 175.0);
        assert_eq!(ScopeApp::analysis_combo_width(600.0), 420.0);
    }

    #[test]
    fn legend_names_use_dataset_prefix_without_envelope_duplicates() {
        assert_eq!(
            ScopeApp::plot_legend_name_text(Some("数据A"), "stVg_0.iA", None),
            "数据A: stVg_0.iA"
        );
        assert_eq!(
            ScopeApp::plot_legend_name_text(None, "stVg_0.iA", None),
            "stVg_0.iA"
        );
        assert_eq!(
            ScopeApp::plot_envelope_legend_name_text(Some("数据B"), "stVg_0.iA", false),
            Some("数据B: stVg_0.iA".to_owned())
        );
        assert_eq!(
            ScopeApp::plot_envelope_legend_name_text(Some("数据B"), "stVg_0.iA", true),
            None
        );
    }

    #[test]
    fn color_rule_keeps_base_for_first_series_and_avoids_collisions() {
        let base = Color32::from_rgb(42, 157, 143);
        let first = ScopeApp::plot_color_with_collision_rank(base, 0, 0, 0);
        let same_variable_dataset_b = ScopeApp::plot_color_with_collision_rank(base, 0, 1, 1);
        let repeated_palette_channel = ScopeApp::plot_color_with_collision_rank(base, 12, 0, 1);

        assert_eq!(first, base);
        assert_ne!(same_variable_dataset_b, base);
        assert_ne!(repeated_palette_channel, base);
        assert_ne!(same_variable_dataset_b, repeated_palette_channel);
    }

    #[test]
    fn export_label_scale_allows_large_report_labels() {
        assert_eq!(MAX_EXPORT_LABEL_SCALE, 8);
        assert_eq!(ScopeApp::effective_export_label_scale(8), 8);
        assert_eq!(ScopeApp::effective_export_cursor_table_scale(8), 4);
    }

    #[test]
    fn export_dpi_labels_are_plain_values() {
        for dpi in ExportDpi::ALL {
            let zh = dpi.label(Language::Zh);
            let en = dpi.label(Language::En);
            assert_eq!(zh, format!("{} DPI", dpi.value()));
            assert_eq!(en, format!("{} DPI", dpi.value()));
            for label in [zh, en] {
                assert!(!label.contains("Word"));
                assert!(!label.contains("\u{6587}\u{6863}"));
                assert!(!label.contains("\u{62a5}\u{544a}"));
                assert!(!label.contains("\u{8bba}\u{6587}"));
                assert!(!label.contains("\u{5b9e}\u{9a8c}"));
            }
        }
    }

    #[test]
    fn export_dpi_presets_are_sorted_by_value() {
        let values = ExportDpi::ALL
            .iter()
            .map(|dpi| dpi.value())
            .collect::<Vec<_>>();
        assert_eq!(values, vec![150, 300, 600]);
    }

    #[test]
    fn export_color_palette_is_split_into_two_ordered_rows() {
        let palette = ScopeApp::export_color_palette();
        let rows = ScopeApp::export_color_palette_rows();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 9);
        assert_eq!(rows[1].len(), 9);
        let first_row: [Color32; 9] = palette[0..9].try_into().unwrap();
        let second_row: [Color32; 9] = palette[9..18].try_into().unwrap();
        assert_eq!(rows[0], first_row);
        assert_eq!(rows[1], second_row);
    }

    #[test]
    fn export_color_picker_corner_triangle_uses_bottom_right_corner() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(40.0, 40.0));
        let points = ScopeApp::export_color_picker_corner_triangle(rect);

        assert_eq!(points[0], egui::pos2(50.0, 60.0));
        assert_eq!(points[1], egui::pos2(30.0, 60.0));
        assert_eq!(points[2], egui::pos2(50.0, 40.0));
    }

    #[test]
    fn export_auto_variable_labels_default_to_disabled() {
        assert!(!default_export_auto_variable_labels_enabled());
    }

    #[test]
    fn export_annotations_default_to_visible() {
        assert!(default_export_annotations_visible());
    }

    #[test]
    fn export_overlay_annotations_gate_variable_labels_but_not_cursor_table() {
        assert!(!ScopeApp::should_draw_export_overlay_annotations(false));
        assert!(ScopeApp::should_draw_export_overlay_annotations(true));
        assert!(!ScopeApp::should_draw_export_variable_labels(false, true));
        assert!(!ScopeApp::should_draw_export_variable_labels(true, false));
        assert!(ScopeApp::should_draw_export_variable_labels(true, true));
        let one_curve = ScopeApp::export_cursor_table_height(1, 3);
        let two_curves = ScopeApp::export_cursor_table_height(2, 3);
        assert_eq!(two_curves - one_curve, Canvas::text_height(3) + 9);
        assert!(two_curves > one_curve);
    }

    #[test]
    fn export_preview_fit_scale_keeps_full_image_visible() {
        let image = egui::vec2(3200.0, 1800.0);
        let available = egui::vec2(1600.0, 900.0);

        assert_eq!(ScopeApp::export_preview_fit_scale(image, available), 0.5);
        assert_eq!(
            ScopeApp::export_preview_display_scale(
                ExportPreviewZoomMode::Fit,
                1.0,
                image,
                available
            ),
            0.5
        );
        assert_eq!(
            ScopeApp::export_preview_display_scale(
                ExportPreviewZoomMode::Manual,
                1.5,
                image,
                available
            ),
            1.5
        );
    }

    #[test]
    fn export_preview_zoom_steps_follow_fixed_presets() {
        assert_eq!(ScopeApp::next_export_preview_zoom_scale(0.75, 1), 1.0);
        assert_eq!(ScopeApp::next_export_preview_zoom_scale(0.75, -1), 0.5);
        assert_eq!(ScopeApp::next_export_preview_zoom_scale(0.8, 1), 1.0);
        assert_eq!(ScopeApp::next_export_preview_zoom_scale(0.8, -1), 0.75);
        assert_eq!(ScopeApp::next_export_preview_zoom_scale(4.0, 1), 4.0);
        assert_eq!(ScopeApp::export_preview_zoom_label(0.75), "75%");
    }

    #[test]
    fn export_image_format_defaults_to_png() {
        assert_eq!(default_export_image_format(), ExportImageFormat::Png);
        assert_eq!(ExportImageFormat::Png.label(), "PNG");
        assert_eq!(ExportImageFormat::Png.extension(), "png");
        assert_eq!(ExportImageFormat::Svg.label(), "SVG");
        assert_eq!(ExportImageFormat::Svg.extension(), "svg");
    }

    #[test]
    fn export_resolution_is_derived_from_dpi() {
        assert_eq!(
            ScopeApp::export_resolution_for_dpi_value(150),
            ExportResolution::Standard
        );
        assert_eq!(
            ScopeApp::export_resolution_for_dpi_value(300),
            ExportResolution::Ultra
        );
        assert_eq!(
            ScopeApp::export_resolution_for_dpi_value(600),
            ExportResolution::Ultra
        );
    }

    #[test]
    fn export_selection_channel_toggle_adds_sorts_dedupes_and_removes() {
        let mut channels = vec![3, 1];

        assert!(ScopeApp::set_export_selection_channel(
            &mut channels,
            2,
            true
        ));
        assert_eq!(channels, vec![1, 2, 3]);
        assert!(!ScopeApp::set_export_selection_channel(
            &mut channels,
            2,
            true
        ));
        assert_eq!(channels, vec![1, 2, 3]);
        assert!(ScopeApp::set_export_selection_channel(
            &mut channels,
            1,
            false
        ));
        assert_eq!(channels, vec![2, 3]);
        assert!(!ScopeApp::set_export_selection_channel(
            &mut channels,
            99,
            false
        ));
        assert_eq!(channels, vec![2, 3]);
    }

    #[test]
    fn removing_export_curve_label_source_updates_preview_selection_only() {
        let mut selections = PlotSelections {
            primary: vec![1, 2, 3],
            imported: vec![vec![4, 5], vec![6]],
            derived: vec![0, 2],
        };

        assert!(ScopeApp::remove_export_curve_label_source(
            &mut selections,
            ExportCurveLabelSource::Imported {
                dataset_index: 0,
                channel_index: 5,
            },
        ));
        assert_eq!(selections.primary, vec![1, 2, 3]);
        assert_eq!(selections.imported, vec![vec![4], vec![6]]);
        assert_eq!(selections.derived, vec![0, 2]);

        assert!(ScopeApp::remove_export_curve_label_source(
            &mut selections,
            ExportCurveLabelSource::Primary(2),
        ));
        assert_eq!(selections.primary, vec![1, 3]);

        assert!(ScopeApp::remove_export_curve_label_source(
            &mut selections,
            ExportCurveLabelSource::Derived(0),
        ));
        assert_eq!(selections.derived, vec![2]);

        assert!(!ScopeApp::remove_export_curve_label_source(
            &mut selections,
            ExportCurveLabelSource::Imported {
                dataset_index: 3,
                channel_index: 9,
            },
        ));
    }

    #[test]
    fn export_render_layers_place_annotations_above_waveform() {
        assert_eq!(
            ScopeApp::export_render_layer_order(),
            [
                ExportRenderLayer::BaseWaveform,
                ExportRenderLayer::VariableLabels,
                ExportRenderLayer::ManualAnnotations,
            ]
        );
    }

    #[test]
    fn export_preview_primary_actions_are_save_then_copy() {
        assert_eq!(
            ScopeApp::export_preview_primary_actions(),
            [
                ExportPreviewPrimaryAction::SaveImage,
                ExportPreviewPrimaryAction::CopyImage,
            ]
        );
    }

    #[test]
    fn clipboard_bitmap_bytes_are_bmp_file_with_32bit_pixels() {
        let rgba = [10, 20, 30, 255, 40, 50, 60, 255];
        let bytes = ScopeApp::clipboard_bitmap_bytes([1, 2], &rgba).unwrap();

        assert_eq!(&bytes[0..2], b"BM");
        assert_eq!(u32::from_le_bytes(bytes[10..14].try_into().unwrap()), 54);
        assert_eq!(u32::from_le_bytes(bytes[14..18].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(bytes[18..22].try_into().unwrap()), 1);
        assert_eq!(i32::from_le_bytes(bytes[22..26].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(bytes[28..30].try_into().unwrap()), 32);
        assert_eq!(&bytes[54..58], &[60, 50, 40, 255]);
        assert_eq!(&bytes[58..62], &[30, 20, 10, 255]);
    }

    #[test]
    fn manually_dragged_export_labels_use_canvas_bounds() {
        let canvas = ClipRect {
            left: 0,
            top: 0,
            right: 1200,
            bottom: 900,
        };
        let plot = ClipRect {
            left: 120,
            top: 80,
            right: 1080,
            bottom: 640,
        };

        assert_eq!(
            ScopeApp::clamp_export_label_position([48, 720], canvas, 160, 32),
            (48, 720)
        );
        assert_ne!(
            ScopeApp::clamp_export_label_position([48, 720], canvas, 160, 32),
            (
                48.clamp(plot.left + 5, plot.right - 160 - 5),
                720.clamp(plot.top + 5, plot.bottom - 32 - 5),
            )
        );
        assert_eq!(
            ScopeApp::clamp_export_label_position([1170, 890], canvas, 160, 32),
            (1035, 863)
        );
    }

    #[test]
    fn export_text_annotation_uses_selected_canvas_position() {
        let annotation = ScopeApp::new_export_text_annotation(Language::Zh, [420, 260], 6);

        assert!(annotation.text.is_empty());
        assert_eq!(annotation.position, CanvasPoint::new(420, 260));
        assert_eq!(annotation.scale, 6);
    }

    #[test]
    fn inline_text_editor_rect_tracks_annotation_position_in_preview() {
        let annotation = ScopeApp::new_export_text_annotation(Language::En, [200, 120], 4);
        let image_rect =
            egui::Rect::from_min_size(egui::pos2(30.0, 40.0), egui::vec2(800.0, 480.0));

        let rect = ScopeApp::export_inline_text_editor_rect(image_rect, &annotation, 0.5);

        assert_eq!(rect.min, egui::pos2(130.0, 100.0));
        assert!(rect.width() >= 160.0);
        assert!(rect.height() >= 28.0);
    }

    #[test]
    fn sidebar_shortcut_toggles_are_independent_from_text_input_focus() {
        assert_eq!(
            ScopeApp::sidebar_visibility_after_shortcuts(true, true, false, true),
            (true, false, true)
        );
        assert_eq!(
            ScopeApp::sidebar_visibility_after_shortcuts(true, false, true, false),
            (false, false, true)
        );
        assert_eq!(
            ScopeApp::sidebar_visibility_after_shortcuts(true, true, false, false),
            (true, true, false)
        );
    }

    #[test]
    fn export_history_shortcuts_map_ctrl_z_and_ctrl_y() {
        assert_eq!(
            ScopeApp::export_history_shortcut_action(true, true, false, true, false),
            Some(ExportHistoryAction::Undo)
        );
        assert_eq!(
            ScopeApp::export_history_shortcut_action(true, false, true, true, false),
            Some(ExportHistoryAction::Redo)
        );
        assert_eq!(
            ScopeApp::export_history_shortcut_action(true, true, true, true, true),
            None
        );
        assert_eq!(
            ScopeApp::export_history_shortcut_action(false, true, true, true, false),
            None
        );
    }

    #[test]
    fn export_annotation_keyboard_shortcuts_ignore_text_input() {
        assert_eq!(
            ScopeApp::export_annotation_keyboard_action(
                true, false, true, false, false, false, false, false
            ),
            Some(ExportAnnotationKeyboardAction::Delete)
        );
        assert_eq!(
            ScopeApp::export_annotation_keyboard_action(
                true, false, false, true, false, false, false, false
            ),
            Some(ExportAnnotationKeyboardAction::Nudge([-1, 0]))
        );
        assert_eq!(
            ScopeApp::export_annotation_keyboard_action(
                true, false, false, false, true, false, false, true
            ),
            Some(ExportAnnotationKeyboardAction::Nudge([10, 0]))
        );
        assert_eq!(
            ScopeApp::export_annotation_keyboard_action(
                true, true, true, true, false, false, false, true
            ),
            None
        );
    }

    #[test]
    fn selected_export_annotations_can_be_deleted_or_moved() {
        let mut label_positions = vec![Some([10, 20])];
        let mut text_annotations = vec![ScopeApp::new_export_text_annotation(
            Language::Zh,
            [30, 40],
            4,
        )];
        let mut arrow_annotations = vec![ExportArrowAnnotation {
            start: CanvasPoint::new(5, 6),
            end: CanvasPoint::new(20, 30),
            color: ScopeApp::annotation_color(Color32::WHITE),
            width: 2,
            head_size: 8.0,
            line_style: AnnotationLineStyle::Solid,
        }];
        let mut rectangle_annotations = vec![ExportRectangleAnnotation::from_corners_with_kind(
            CanvasPoint::new(15, 25),
            CanvasPoint::new(40, 60),
            ShapeKind::Rectangle,
            ScopeApp::annotation_color(Color32::WHITE),
            2,
            AnnotationLineStyle::Solid,
        )];
        let mut ink_strokes = vec![ExportInkStroke {
            points: vec![CanvasPoint::new(1, 2), CanvasPoint::new(3, 4)],
            color: ScopeApp::annotation_color(Color32::WHITE),
            width: 2,
        }];
        let canvas = [120, 90];

        assert!(ScopeApp::nudge_export_annotation_selection(
            ExportAnnotationSelection::VariableLabel(0),
            [3, -2],
            canvas,
            &mut label_positions,
            &mut text_annotations,
            &mut arrow_annotations,
            &mut rectangle_annotations,
            &mut ink_strokes,
        ));
        assert_eq!(label_positions[0], Some([13, 18]));

        assert!(ScopeApp::nudge_export_annotation_selection(
            ExportAnnotationSelection::Arrow(0),
            [2, 3],
            canvas,
            &mut label_positions,
            &mut text_annotations,
            &mut arrow_annotations,
            &mut rectangle_annotations,
            &mut ink_strokes,
        ));
        assert_eq!(arrow_annotations[0].start, CanvasPoint::new(7, 9));
        assert_eq!(arrow_annotations[0].end, CanvasPoint::new(22, 33));

        assert!(ScopeApp::nudge_export_annotation_selection(
            ExportAnnotationSelection::Rectangle(0),
            [-5, 7],
            canvas,
            &mut label_positions,
            &mut text_annotations,
            &mut arrow_annotations,
            &mut rectangle_annotations,
            &mut ink_strokes,
        ));
        assert_eq!(rectangle_annotations[0].rect.left, 10);
        assert_eq!(rectangle_annotations[0].rect.top, 32);

        assert!(ScopeApp::nudge_export_annotation_selection(
            ExportAnnotationSelection::Ink(0),
            [10, 20],
            canvas,
            &mut label_positions,
            &mut text_annotations,
            &mut arrow_annotations,
            &mut rectangle_annotations,
            &mut ink_strokes,
        ));
        assert_eq!(ink_strokes[0].points[0], CanvasPoint::new(11, 22));
        assert_eq!(ink_strokes[0].points[1], CanvasPoint::new(13, 24));

        assert!(ScopeApp::delete_export_annotation_selection(
            ExportAnnotationSelection::Text(0),
            &mut label_positions,
            &mut text_annotations,
            &mut arrow_annotations,
            &mut rectangle_annotations,
            &mut ink_strokes,
        ));
        assert!(text_annotations.is_empty());
    }

    #[test]
    fn export_tool_shortcuts_select_annotation_tools() {
        assert_eq!(
            ScopeApp::export_tool_shortcut(
                true, true, false, true, false, false, false, false, false
            ),
            Some(ExportPreviewTool::Rectangle)
        );
        assert_eq!(
            ScopeApp::export_tool_shortcut(
                true, true, false, false, true, false, false, false, false
            ),
            Some(ExportPreviewTool::Arrow)
        );
        assert_eq!(
            ScopeApp::export_tool_shortcut(
                true, true, true, true, false, false, false, false, false
            ),
            None
        );
    }

    #[test]
    fn export_toolbar_line_styles_are_ordered_for_icon_dropdown() {
        assert_eq!(
            ExportArrowLineStyle::TOOLBAR,
            [
                ExportArrowLineStyle::Solid,
                ExportArrowLineStyle::Dashed,
                ExportArrowLineStyle::Dotted,
                ExportArrowLineStyle::Thick,
                ExportArrowLineStyle::Double,
            ]
        );
    }

    #[test]
    fn export_manual_arrow_head_size_respects_toggle() {
        assert_eq!(ScopeApp::manual_export_arrow_head_size(false, 12.0), 0.0);
        assert_eq!(ScopeApp::manual_export_arrow_head_size(true, 12.0), 12.0);
    }

    #[test]
    fn export_arrow_head_shortcut_uses_tab_outside_text_input() {
        assert!(ScopeApp::export_arrow_head_shortcut(true, true, false));
        assert!(!ScopeApp::export_arrow_head_shortcut(true, true, true));
        assert!(!ScopeApp::export_arrow_head_shortcut(false, true, false));
        assert!(!ScopeApp::export_arrow_head_shortcut(true, false, false));
    }

    #[test]
    fn responsive_sidebar_ranges_expand_but_preserve_plot_space() {
        let compact = ScopeApp::sidebar_width_ranges(980.0);
        assert_eq!(*compact.channel.start(), CHANNEL_PANEL_MIN_WIDTH);
        assert_eq!(*compact.analysis.start(), ANALYSIS_PANEL_MIN_WIDTH);
        assert!(compact.channel.end() + compact.analysis.end() <= 980.0 - MIN_CENTRAL_PANEL_WIDTH);

        let left_only = ScopeApp::sidebar_width_ranges_for_visibility(980.0, true, false);
        assert!(left_only.channel.end() > compact.channel.end());
        assert_eq!(*left_only.analysis.end(), ANALYSIS_PANEL_MIN_WIDTH);

        let right_only = ScopeApp::sidebar_width_ranges_for_visibility(980.0, false, true);
        assert!(right_only.analysis.end() > compact.analysis.end());
        assert_eq!(*right_only.channel.end(), CHANNEL_PANEL_MIN_WIDTH);

        let normal = ScopeApp::sidebar_width_ranges(1440.0);
        assert!(normal.channel.end() > &CHANNEL_PANEL_MAX_WIDTH);
        assert!(normal.analysis.end() > &ANALYSIS_PANEL_MAX_WIDTH);

        let wide = ScopeApp::sidebar_width_ranges(1920.0);
        assert!(wide.channel.end() > normal.channel.end());
        assert!(wide.analysis.end() > normal.analysis.end());
        assert!(wide.channel.end() <= &(1920.0 * MAX_CHANNEL_PANEL_FRACTION));
        assert!(wide.analysis.end() <= &(1920.0 * MAX_ANALYSIS_PANEL_FRACTION));
    }

    #[test]
    fn layout_resize_defers_plot_reload_even_when_debounce_is_ready() {
        assert!(!ScopeApp::plot_reload_allowed(true, true));
        assert!(!ScopeApp::plot_reload_allowed(false, false));
        assert!(ScopeApp::plot_reload_allowed(true, false));
    }

    #[test]
    fn plot_cache_width_is_bucketed_for_sidebar_resize_hits() {
        assert_eq!(
            ScopeApp::plot_cache_pixel_width(1001.0),
            ScopeApp::plot_cache_pixel_width(1023.0)
        );
        assert!(
            ScopeApp::plot_cache_pixel_width(1025.0) > ScopeApp::plot_cache_pixel_width(1023.0)
        );
    }

    #[test]
    fn plot_load_prefers_summary_for_large_multi_channel_sample_product() {
        assert!(!ScopeApp::plot_should_use_summary(100_000, 2));
        assert!(ScopeApp::plot_should_use_summary(100_000, 3));
        assert!(ScopeApp::plot_should_use_summary(300_000, 1));
    }

    #[test]
    fn summary_bins_account_for_two_envelope_lines_per_series() {
        assert_eq!(ScopeApp::summary_bins_for_channels(320, 1024.0), 96);
        assert!(
            ScopeApp::summary_bins_for_channels(40, 1024.0)
                < ScopeApp::summary_bins_for_channels(8, 1024.0)
        );
    }

    #[test]
    fn software_renderer_uses_lower_plot_budget() {
        assert!(ScopeApp::renderer_value_uses_software_performance_mode(
            Some("glow-software")
        ));
        assert!(ScopeApp::renderer_value_uses_software_performance_mode(
            Some("mesa")
        ));
        assert!(!ScopeApp::renderer_value_uses_software_performance_mode(
            Some("glow")
        ));

        assert!(
            ScopeApp::summary_bins_for_channels_with_mode(16, 1024.0, true)
                < ScopeApp::summary_bins_for_channels_with_mode(16, 1024.0, false)
        );
    }

    #[test]
    fn summary_envelope_preserves_min_max_instead_of_centerline() {
        let summary = RangeSummary {
            bin_start: vec![0.0, 0.1, 0.2],
            bin_end: vec![0.1, 0.2, 0.3],
            min: vec![vec![-100.0, -80.0, -120.0]],
            max: vec![vec![100.0, 80.0, 120.0]],
        };

        let envelope =
            ScopeApp::summary_envelope_series(&summary, 0, 0.0, 1.0, summary.bin_start.len());

        assert_eq!(envelope.high.len(), 3);
        assert_eq!(envelope.low.len(), 3);
        assert_eq!(envelope.bounds, Some((-120.0, 120.0)));
        assert!(envelope.high.iter().all(|point| point.y > 0.0));
        assert!(envelope.low.iter().all(|point| point.y < 0.0));
    }

    #[test]
    fn plot_job_preparation_uses_result_key_channels() {
        let key = PlotCacheKey {
            generation: 7,
            start_bits: 0.0_f64.to_bits(),
            end_bits: 1.0_f64.to_bits(),
            channels: vec![0, 2, 4],
            scale_bits: vec![1.0_f32.to_bits(); 3],
            time_offset_bits: 0.0_f64.to_bits(),
            plot_pixel_width: 1024,
            budget_series_count: 3,
        };

        assert_eq!(
            ScopeApp::plot_job_channels_for_prepare(Some(&key), &[1, 3]),
            vec![0, 2, 4]
        );
        assert_eq!(
            ScopeApp::plot_job_channels_for_prepare(None, &[1, 3]),
            vec![1, 3]
        );
    }

    #[test]
    fn three_phase_selectors_stack_vertically_when_analysis_panel_is_tight() {
        assert_eq!(
            ScopeApp::three_phase_selector_layout(640.0),
            ThreePhaseSelectorLayout::Vertical
        );
        assert_eq!(
            ScopeApp::three_phase_selector_layout(900.0),
            ThreePhaseSelectorLayout::Horizontal
        );
    }

    #[test]
    fn unified_analysis_input_places_three_phase_selector_on_next_row_when_tight() {
        assert!(!ScopeApp::analysis_input_shows_three_phase_selectors(2));
        assert!(ScopeApp::analysis_input_shows_three_phase_selectors(3));
        assert_eq!(
            ScopeApp::analysis_input_three_phase_row_layout(3, 640.0),
            Some(ThreePhaseSelectorLayout::Vertical)
        );
        assert_eq!(
            ScopeApp::analysis_input_three_phase_row_layout(3, 900.0),
            Some(ThreePhaseSelectorLayout::Horizontal)
        );
        assert_eq!(
            ScopeApp::analysis_input_three_phase_row_layout(2, 900.0),
            None
        );
    }

    #[test]
    fn real_cloud_csv_reads_plot_summary_fft_and_measurement_samples() {
        let path = sample_data_path("Data/105329025C120033_mc02_21_20260507192102_wave_data.csv");
        if !path.exists() {
            return;
        }

        let opened = ScopeApp::open_waveform_file(&path, 1000.0).unwrap();
        assert_eq!(opened.kind, SourceKind::Cloud);
        let meta = opened.source.metadata();
        assert_eq!(meta.channels.len(), 60);
        assert!(meta.sample_count > 0);

        let channels = [0, 3, 4, 5, 30];
        let block = opened
            .source
            .read_range(meta.start_time, meta.end_time.min(0.5), &channels, 20_000)
            .unwrap();
        assert!(!block.times.is_empty());
        assert_eq!(block.channels.len(), channels.len());
        assert!(block
            .channels
            .iter()
            .flatten()
            .all(|value| value.is_finite()));

        let summary = opened
            .source
            .summarize_range(meta.start_time, meta.end_time, &channels, 1000)
            .unwrap();
        assert!(!summary.bin_start.is_empty());
        assert_summary_is_finite(&summary, channels.len(), "real cloud csv");

        let plot_data = ScopeApp::load_plot_data(
            opened.source.clone(),
            meta.start_time,
            meta.end_time,
            &channels,
            channels.len(),
            1200.0,
        )
        .unwrap();
        assert!(plot_data.is_some());

        let fft_samples = block.channels.first().cloned().unwrap_or_default();
        assert!(!fft_samples.is_empty());
        let _ = fft::analyze(
            "real".to_owned(),
            &fft_samples,
            meta.nominal_sample_rate_hz,
            50.0,
            9,
        );
    }

    #[test]
    fn repid_curves_extend_derived_output_names() {
        let curves = vec![crate::repid_derived::RepidDerivedCurve {
            name: "VG positive RMS".to_owned(),
            raw_name: "VG positive RMS".to_owned(),
            script_vars: Vec::new(),
            script: "pseqll(Ch1,Ch2,Ch3)/(220)".to_owned(),
            min: None,
            max: None,
            k: 1.0,
            b: 0.0,
            color: None,
            pen_style: None,
            auto_color: true,
            time_offset: 0.0,
            time_scale: 1.0,
        }];

        assert_eq!(
            ScopeApp::derived_output_count_for_repid_curves(curves.len()),
            DERIVED_CHANNEL_COUNT + 1
        );
        assert_eq!(
            ScopeApp::derived_channel_name_for_repid_curves(DERIVED_CHANNEL_COUNT, &curves),
            "VG positive RMS"
        );
    }

    #[test]
    fn repid_curve_loads_without_builtin_pll_outputs() {
        struct FixedSource {
            meta: DatasetMeta,
            times: Vec<f64>,
            channels: Vec<Vec<f32>>,
        }

        impl DataSource for FixedSource {
            fn open(_path: &Path) -> DataResult<Self>
            where
                Self: Sized,
            {
                unreachable!("test source is constructed directly")
            }

            fn metadata(&self) -> &DatasetMeta {
                &self.meta
            }

            fn read_range(
                &self,
                _start_time: f64,
                _end_time: f64,
                channels: &[usize],
                _max_points: usize,
            ) -> DataResult<SampleBlock> {
                Ok(SampleBlock {
                    times: self.times.clone(),
                    channels: channels
                        .iter()
                        .map(|channel| self.channels[*channel].clone())
                        .collect(),
                })
            }

            fn summarize_range(
                &self,
                start_time: f64,
                end_time: f64,
                channels: &[usize],
                target_bins: usize,
            ) -> DataResult<RangeSummary> {
                let block = self.read_range(start_time, end_time, channels, target_bins)?;
                Ok(RangeSummary::from_samples(
                    &block,
                    channels.len(),
                    start_time,
                    end_time,
                    target_bins,
                ))
            }
        }

        let count = 128;
        let sample = |phase_offset: f64| {
            (0..count)
                .map(|index| {
                    let angle = std::f64::consts::TAU * index as f64 / count as f64;
                    (10.0_f64 * 2.0_f64.sqrt() * (angle + phase_offset).cos()) as f32
                })
                .collect::<Vec<_>>()
        };
        let source = Arc::new(FixedSource {
            meta: DatasetMeta {
                source_name: "fixed".to_owned(),
                channels: (0..3)
                    .map(|index| ChannelMeta {
                        index,
                        name: format!("CH{}", index + 1),
                        unit: String::new(),
                        sample_rate_hz: 1000.0,
                        scale: 1.0,
                        default_visible: false,
                    })
                    .collect(),
                start_time: 0.0,
                end_time: 1.0,
                sample_count: count as u64,
                nominal_sample_rate_hz: 1000.0,
            },
            times: (0..count).map(|index| index as f64 / 1000.0).collect(),
            channels: vec![
                sample(0.0),
                sample(-2.0 * std::f64::consts::PI / 3.0),
                sample(2.0 * std::f64::consts::PI / 3.0),
            ],
        });
        let curves = vec![crate::repid_derived::RepidDerivedCurve {
            name: "I positive RMS".to_owned(),
            raw_name: "I positive RMS".to_owned(),
            script_vars: Vec::new(),
            script: "pseqph(Ch1,Ch2,Ch3)".to_owned(),
            min: None,
            max: None,
            k: 1.0,
            b: 0.0,
            color: None,
            pen_style: None,
            auto_color: true,
            time_offset: 0.0,
            time_scale: 1.0,
        }];

        let block = ScopeApp::load_derived_data_with_cancel(
            source,
            0.0,
            1.0,
            &[DERIVED_CHANNEL_COUNT],
            &curves,
            [0, 1, 2],
            [0, 1, 2],
            [1.0; 3],
            [1.0; 3],
            &[1.0; 3],
            1000.0,
            50.0,
            true,
            1000,
            None,
        )
        .unwrap();

        assert_eq!(block.channels.len(), DERIVED_CHANNEL_COUNT + 1);
        assert!(block.channels[DERIVED_CHANNEL_COUNT]
            .iter()
            .all(|value| (*value - 10.0).abs() < 0.02));
    }

    fn sample_data_path(relative: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
    }

    fn assert_summary_is_finite(summary: &RangeSummary, channel_count: usize, label: &str) {
        assert_eq!(summary.min.len(), channel_count, "{label}");
        assert_eq!(summary.max.len(), channel_count, "{label}");
        assert_eq!(summary.bin_start.len(), summary.bin_end.len(), "{label}");
        for channel_index in 0..channel_count {
            assert_eq!(
                summary.min[channel_index].len(),
                summary.bin_start.len(),
                "{label} channel {channel_index} min length"
            );
            assert_eq!(
                summary.max[channel_index].len(),
                summary.bin_start.len(),
                "{label} channel {channel_index} max length"
            );
            for (min, max) in summary.min[channel_index]
                .iter()
                .zip(&summary.max[channel_index])
            {
                assert!(min.is_finite(), "{label} channel {channel_index} min");
                assert!(max.is_finite(), "{label} channel {channel_index} max");
                assert!(max >= min, "{label} channel {channel_index} range");
            }
        }
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let dir = std::env::temp_dir().join(format!(
            "scope_analyzer_{name}_{}_{}",
            std::process::id(),
            millis
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_indexed_analog_csv(path: &Path, values: &[f32]) {
        let mut file = File::create(path).unwrap();
        writeln!(file, "NUM,ACH1").unwrap();
        for (index, value) in values.iter().enumerate() {
            writeln!(file, "{},{}", index + 1, value).unwrap();
        }
    }

    fn write_indexed_digital_csv(path: &Path, rows: &[(u8, u8, u8, u8)]) {
        let mut file = File::create(path).unwrap();
        writeln!(file, "Num,DCH1,DCH2,DCH3,DCH4").unwrap();
        for (index, (bit0, bit1, bit2, dch4)) in rows.iter().enumerate() {
            writeln!(file, "{},{},{},{},{}", index + 1, bit0, bit1, bit2, dch4).unwrap();
        }
    }
}
