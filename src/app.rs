use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::Once,
    time::{SystemTime, UNIX_EPOCH},
};

use eframe::egui::{self, Color32, PointerButton, RichText, Stroke};
use egui_plot::{
    Bar, BarChart, Legend, Line, LineStyle, Plot, PlotBounds, PlotPoint, PlotPoints, Text, VLine,
};
use serde::{Deserialize, Serialize};

use crate::{
    data::{
        CloudCsvDataSource, CsvDataSource, DataResult, DataSource, DatasetMeta, RangeSummary,
        SampleBlock,
    },
    fft::{self, FftResult},
};

const MAX_DRAW_POINTS_PER_CHANNEL: usize = 20_000;
const MAX_TOTAL_DRAW_POINTS: usize = 120_000;
const MIN_DRAW_POINTS_PER_CHANNEL: usize = 256;
const MAX_FFT_POINTS: usize = 262_144;
const MAX_AUTO_MEASURE_POINTS: usize = 131_072;
const ZOOM_BOX_MIN_PIXELS: f32 = 8.0;
const CONFIG_VERSION: u32 = 1;
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
const CHANNEL_PANEL_DEFAULT_WIDTH: f32 = 230.0;
const CHANNEL_PANEL_MAX_WIDTH: f32 = 360.0;
const MEASUREMENT_CHANNEL_COLUMN_WIDTH: f32 = 120.0;
const MAX_SCOPE_LAYOUT_ROWS: usize = 4;
const MAX_SCOPE_LAYOUT_COLS: usize = 4;
const MAX_TIME_SYNC_POINTS: usize = 20_000;

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

fn default_wheel_zoom_sensitivity() -> f64 {
    DEFAULT_WHEEL_ZOOM_SENSITIVITY
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
    key: ShortcutKey,
}

impl ShortcutBinding {
    fn new(ctrl: bool, key: ShortcutKey) -> Self {
        Self { ctrl, key }
    }

    fn label(self) -> String {
        if self.ctrl {
            format!("Ctrl+{}", self.key.label())
        } else {
            self.key.label().to_owned()
        }
    }

    fn pressed(self, input: &egui::InputState) -> bool {
        input.modifiers.ctrl == self.ctrl && input.key_pressed(self.key.egui_key())
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
}

impl Default for ShortcutConfig {
    fn default() -> Self {
        Self {
            reset_view: default_reset_view_shortcut(),
            fit_cursors: default_fit_cursors_shortcut(),
            toggle_cursors: default_toggle_cursors_shortcut(),
            select_all: default_select_all_shortcut(),
            select_none: default_select_none_shortcut(),
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

fn default_shortcuts() -> ShortcutConfig {
    ShortcutConfig::default()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AppConfig {
    version: u32,
    display_names: Vec<String>,
    #[serde(default, skip_serializing)]
    visible: Vec<bool>,
    #[serde(default, skip_serializing)]
    channel_colors: Vec<[u8; 4]>,
    #[serde(default, skip_serializing)]
    line_widths: Vec<f32>,
    #[serde(default, skip_serializing)]
    line_patterns: Vec<ChannelLinePattern>,
    #[serde(default, skip_serializing)]
    channel_scales: Vec<f32>,
    #[serde(default, skip_serializing)]
    channel_panes: Vec<usize>,
    #[serde(default, skip_serializing)]
    fft_channel: usize,
    #[serde(default = "default_wheel_zoom_sensitivity", skip_serializing)]
    wheel_zoom_sensitivity: f64,
    #[serde(default = "default_sample_rate_hz", skip_serializing)]
    sample_rate_hz: f64,
    #[serde(default = "default_harmonic_base_hz", skip_serializing)]
    harmonic_base_hz: f64,
    #[serde(default = "default_scope_layout_rows", skip_serializing)]
    scope_layout_rows: usize,
    #[serde(default = "default_scope_layout_cols", skip_serializing)]
    scope_layout_cols: usize,
    #[serde(default = "default_language", skip_serializing)]
    language: Language,
    #[serde(default = "default_theme_mode", skip_serializing)]
    theme_mode: ThemeMode,
    #[serde(default = "default_shortcuts", skip_serializing)]
    shortcuts: ShortcutConfig,
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
    start: f64,
    end: f64,
    channels: Vec<usize>,
    rows: Vec<(usize, AutoMeasurement)>,
}

struct ImportedDataset {
    source: Box<dyn DataSource>,
    kind: SourceKind,
    path: PathBuf,
    display_name: String,
    visible: Vec<bool>,
    line_pattern: ChannelLinePattern,
    time_offset: f64,
    plot_cache: SampleBlock,
    plot_summary: Option<RangeSummary>,
    selected_for_delete: bool,
}

pub struct ScopeApp {
    source: Option<Box<dyn DataSource>>,
    source_kind: Option<SourceKind>,
    imported_datasets: Vec<ImportedDataset>,
    primary_selected_for_delete: bool,
    primary_dataset_name: String,
    visible: Vec<bool>,
    display_names: Vec<String>,
    editing_display_name: Option<usize>,
    pending_display_name_focus: Option<usize>,
    channel_colors: Vec<Color32>,
    line_widths: Vec<f32>,
    line_patterns: Vec<ChannelLinePattern>,
    channel_scales: Vec<f32>,
    channel_panes: Vec<usize>,
    active_scope_pane: usize,
    hovered_channel: Option<usize>,
    view_start: f64,
    view_end: f64,
    y_min: Option<f64>,
    y_max: Option<f64>,
    cursor_a: f64,
    cursor_b: f64,
    show_cursor_a: bool,
    show_cursor_b: bool,
    active_cursor: CursorId,
    channel_filter: String,
    show_help: bool,
    show_options: bool,
    wheel_zoom_sensitivity: f64,
    sample_rate_hz: f64,
    harmonic_base_hz: f64,
    sync_time_axes: bool,
    time_sync_status: String,
    scope_layout_rows: usize,
    scope_layout_cols: usize,
    language: Language,
    theme_mode: ThemeMode,
    shortcuts: ShortcutConfig,
    last_error: Option<String>,
    loaded_path: Option<PathBuf>,
    recent_files: Vec<PathBuf>,
    recent_configs: Vec<PathBuf>,
    plot_cache: SampleBlock,
    plot_summary: Option<RangeSummary>,
    fft_results: Vec<(usize, FftResult)>,
    measurement_cache: Option<MeasurementCache>,
    fft_dataset_index: usize,
    fft_channel: usize,
    fft_channel_user_selected: bool,
    needs_fft_reload: bool,
    needs_plot_reload: bool,
    needs_compare_plot_reload: bool,
    cursor_place_mode: Option<CursorId>,
    zoom_box_start: Option<egui::Pos2>,
    zoom_box_current: Option<egui::Pos2>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CursorId {
    A,
    B,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Cloud,
    Local,
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
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::install_panic_hook();
        Self::install_cjk_fonts(&cc.egui_ctx);

        let recent_files = Self::load_recent_files();
        let recent_configs = Self::load_recent_configs();
        Self {
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
            active_scope_pane: 0,
            hovered_channel: None,
            view_start: 0.0,
            view_end: 1.0,
            y_min: None,
            y_max: None,
            cursor_a: 0.25,
            cursor_b: 0.75,
            show_cursor_a: true,
            show_cursor_b: true,
            active_cursor: CursorId::A,
            channel_filter: String::new(),
            show_help: false,
            show_options: false,
            wheel_zoom_sensitivity: DEFAULT_WHEEL_ZOOM_SENSITIVITY,
            sample_rate_hz: default_sample_rate_hz(),
            harmonic_base_hz: default_harmonic_base_hz(),
            sync_time_axes: false,
            time_sync_status: String::new(),
            scope_layout_rows: default_scope_layout_rows(),
            scope_layout_cols: default_scope_layout_cols(),
            language: default_language(),
            theme_mode: default_theme_mode(),
            shortcuts: default_shortcuts(),
            last_error: None,
            loaded_path: None,
            recent_files,
            recent_configs,
            plot_cache: SampleBlock::default(),
            plot_summary: None,
            fft_results: Vec::new(),
            measurement_cache: None,
            fft_dataset_index: 0,
            fft_channel: 0,
            fft_channel_user_selected: false,
            needs_fft_reload: false,
            needs_plot_reload: false,
            needs_compare_plot_reload: false,
            cursor_place_mode: None,
            zoom_box_start: None,
            zoom_box_current: None,
        }
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
        env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.join("scope-crash.log")))
            .unwrap_or_else(|| PathBuf::from("scope-crash.log"))
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
                    .insert(0, font_name.clone());
            }
        }

        if let Some((font_name, font_data)) = Self::load_icon_font() {
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
        let windows_dir = env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let fonts_dir = windows_dir.join("Fonts");
        let candidates = [
            "Deng.ttf",
            "simhei.ttf",
            "simsunb.ttf",
            "NotoSansSC-VF.ttf",
            "msyh.ttc",
            "simsun.ttc",
        ];

        candidates.iter().find_map(|file_name| {
            let path = fonts_dir.join(file_name);
            fs::read(path)
                .ok()
                .map(|font_data| (format!("scope-cjk-{file_name}"), font_data))
        })
    }

    fn load_icon_font() -> Option<(String, Vec<u8>)> {
        let windows_dir = env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let fonts_dir = windows_dir.join("Fonts");
        fs::read(fonts_dir.join("segmdl2.ttf"))
            .ok()
            .map(|font_data| ("scope-icons-segmdl2".to_owned(), font_data))
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
                .unwrap_or_else(|| format!("Dataset {}", index + 1))
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
            self.channel_colors.extend(
                (old_color_len..state_len)
                    .map(|channel_index| Self::default_channel_color(channel_index)),
            );
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
    }

    fn set_source(&mut self, source: Box<dyn DataSource>, path: PathBuf, kind: SourceKind) {
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
        self.loaded_path = Some(path);
        self.source = Some(source);
        self.source_kind = Some(kind);
        self.primary_selected_for_delete = false;
        self.fft_dataset_index = 0;
        self.fft_channel_user_selected = false;
        self.fft_channel = self
            .preferred_fft_channel(&self.fft_channel_options())
            .unwrap_or(0);
        self.last_error = None;
        self.needs_plot_reload = true;
        self.needs_compare_plot_reload = true;
        self.cursor_place_mode = None;
    }

    fn add_imported_dataset(
        &mut self,
        source: Box<dyn DataSource>,
        path: PathBuf,
        kind: SourceKind,
    ) {
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
            plot_cache: SampleBlock::default(),
            plot_summary: None,
            selected_for_delete: false,
        });
        self.last_error = None;
        self.needs_compare_plot_reload = true;
        self.y_min = None;
        self.y_max = None;
    }

    fn clear_imported_datasets(&mut self) {
        self.imported_datasets.clear();
        self.needs_compare_plot_reload = false;
        self.y_min = None;
        self.y_max = None;
    }

    fn clear_all_datasets(&mut self) {
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
        self.active_scope_pane = 0;
        self.hovered_channel = None;
        self.primary_selected_for_delete = false;
        self.plot_cache = SampleBlock::default();
        self.plot_summary = None;
        self.fft_results.clear();
        self.fft_dataset_index = 0;
        self.measurement_cache = None;
        self.needs_plot_reload = false;
        self.needs_compare_plot_reload = false;
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

    fn recent_files_path() -> PathBuf {
        env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
            .or_else(|| env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
            .join("scope-recent-files.json")
    }

    fn recent_configs_path() -> PathBuf {
        env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
            .or_else(|| env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
            .join("scope-recent-configs.json")
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

    fn load_recent_configs() -> Vec<PathBuf> {
        let Ok(text) = std::fs::read_to_string(Self::recent_configs_path()) else {
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

    fn save_recent_configs(&self) {
        let recent = RecentFiles {
            paths: self.recent_configs.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&recent) {
            let _ = std::fs::write(Self::recent_configs_path(), json);
        }
    }

    fn remember_recent_file(&mut self, path: &Path) {
        let normalized = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.recent_files.retain(|existing| existing != &normalized);
        self.recent_files.insert(0, normalized);
        self.recent_files.truncate(MAX_RECENT_FILES);
        self.save_recent_files();
    }

    fn remember_recent_config(&mut self, path: &Path) {
        let normalized = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.recent_configs
            .retain(|existing| existing != &normalized);
        self.recent_configs.insert(0, normalized);
        self.recent_configs.truncate(MAX_RECENT_CONFIGS);
        self.save_recent_configs();
    }

    fn clear_recent_files(&mut self) {
        self.recent_files.clear();
        self.save_recent_files();
    }

    fn clear_recent_configs(&mut self) {
        self.recent_configs.clear();
        self.save_recent_configs();
    }

    fn recent_file_label(path: &Path) -> String {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let Some(parent) = path.parent() else {
            return name;
        };
        format!("{name}  ({})", parent.display())
    }

    fn open_standard_csv(&mut self, path: PathBuf) -> bool {
        match CsvDataSource::open(&path) {
            Ok(source) => {
                self.set_source(Box::new(source), path, SourceKind::Local);
                true
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn open_cloud_csv(&mut self, path: PathBuf) -> bool {
        match CloudCsvDataSource::open_with_sample_rate(&path, self.sample_rate_hz) {
            Ok(source) => {
                self.set_source(Box::new(source), path, SourceKind::Cloud);
                true
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

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

    fn open_standard_compare_csv(&mut self, path: PathBuf) -> bool {
        match CsvDataSource::open(&path) {
            Ok(source) => {
                self.add_imported_dataset(Box::new(source), path, SourceKind::Local);
                true
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn open_cloud_compare_csv(&mut self, path: PathBuf) -> bool {
        match CloudCsvDataSource::open_with_sample_rate(&path, self.sample_rate_hz) {
            Ok(source) => {
                self.add_imported_dataset(Box::new(source), path, SourceKind::Cloud);
                true
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn open_auto_compare_csv(&mut self, path: PathBuf) -> bool {
        if self.source.is_none() {
            self.last_error = Some(
                self.tr(
                    "请先导入主数据，或一次选择多个 CSV 数据文件。",
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
            self.last_error = Some(
                self.tr(
                    "请选择至少一个 CSV 数据文件。",
                    "Select at least one CSV data file.",
                )
                .to_owned(),
            );
            return false;
        }

        let mut imported_any = false;
        for path in paths {
            let opened = if self.source.is_none() {
                self.open_auto_csv(path)
            } else {
                self.open_auto_compare_csv(path)
            };
            imported_any |= opened;
        }
        imported_any
    }

    fn looks_like_cloud_csv(path: &Path) -> Result<bool, String> {
        let file = File::open(path).map_err(|error| error.to_string())?;
        let mut reader = BufReader::new(file);
        let mut header = String::new();
        let bytes = reader
            .read_line(&mut header)
            .map_err(|error| error.to_string())?;
        if bytes == 0 {
            return Err("空文件：CSV 没有表头或数据内容。".to_owned());
        }
        let first_column = header
            .trim_start_matches('\u{feff}')
            .trim()
            .split(',')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"');
        Ok(first_column.eq_ignore_ascii_case("Content"))
    }

    fn current_config(&self) -> AppConfig {
        AppConfig {
            version: CONFIG_VERSION,
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
            fft_channel: self.fft_channel,
            wheel_zoom_sensitivity: self.wheel_zoom_sensitivity,
            sample_rate_hz: self.sample_rate_hz,
            harmonic_base_hz: self.harmonic_base_hz,
            scope_layout_rows: self.scope_layout_rows,
            scope_layout_cols: self.scope_layout_cols,
            language: self.language,
            theme_mode: self.theme_mode,
            shortcuts: self.shortcuts,
        }
    }

    fn apply_config(&mut self, config: AppConfig) {
        let channel_count = self.display_names.len();
        for (index, name) in config
            .display_names
            .into_iter()
            .enumerate()
            .take(channel_count)
        {
            self.display_names[index] = name;
        }
        self.needs_fft_reload = true;
    }

    fn apply_runtime_config(&mut self, config: AppConfig) {
        self.apply_config(AppConfig {
            version: config.version,
            display_names: config.display_names.clone(),
            visible: Vec::new(),
            channel_colors: Vec::new(),
            line_widths: Vec::new(),
            line_patterns: Vec::new(),
            channel_scales: Vec::new(),
            channel_panes: Vec::new(),
            fft_channel: 0,
            wheel_zoom_sensitivity: default_wheel_zoom_sensitivity(),
            sample_rate_hz: default_sample_rate_hz(),
            harmonic_base_hz: default_harmonic_base_hz(),
            scope_layout_rows: default_scope_layout_rows(),
            scope_layout_cols: default_scope_layout_cols(),
            language: default_language(),
            theme_mode: default_theme_mode(),
            shortcuts: default_shortcuts(),
        });
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
        self.hovered_channel = None;
        self.needs_plot_reload = true;
        self.needs_compare_plot_reload = true;
        self.needs_fft_reload = true;
        self.measurement_cache = None;
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
            .filter_map(|(index, dataset)| {
                (dataset.kind == SourceKind::Cloud).then(|| (index, dataset.path.clone()))
            })
            .collect::<Vec<_>>();
        if main_cloud_path.is_none() && imported_cloud_paths.is_empty() {
            self.needs_fft_reload = true;
            return;
        }
        let config = self.current_config();
        let primary_dataset_name = self.primary_dataset_name.clone();
        if let Some(path) = main_cloud_path {
            match CloudCsvDataSource::open_with_sample_rate(&path, self.sample_rate_hz) {
                Ok(source) => {
                    self.set_source(Box::new(source), path, SourceKind::Cloud);
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
                        dataset.source = Box::new(source);
                        dataset.plot_cache = SampleBlock::default();
                        dataset.plot_summary = None;
                    }
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
        self.needs_compare_plot_reload = true;
    }

    fn export_config(&mut self) {
        if self.display_names.is_empty() {
            self.last_error = Some(
                self.tr(
                    "请先打开波形 CSV，再导出变量名。",
                    "Open a waveform CSV before exporting names.",
                )
                .to_owned(),
            );
            return;
        }
        let filter_name = self.tr("变量名配置", "Display names config");
        let Some(path) = rfd::FileDialog::new()
            .add_filter(filter_name, &["json"])
            .set_file_name("scope-config.json")
            .save_file()
        else {
            return;
        };
        match serde_json::to_string_pretty(&self.current_config()) {
            Ok(json) => {
                if let Err(error) = std::fs::write(&path, json) {
                    self.last_error = Some(match self.language {
                        Language::Zh => format!("导出变量名失败: {error}"),
                        Language::En => format!("Failed to export names: {error}"),
                    });
                } else {
                    self.remember_recent_config(&path);
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
                    "请先打开波形 CSV，再导入变量名。",
                    "Open a waveform CSV before importing names.",
                )
                .to_owned(),
            );
            return;
        }
        let filter_name = self.tr("变量名配置", "Display names config");
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
                    "请先打开波形 CSV，再导入变量名。",
                    "Open a waveform CSV before importing names.",
                )
                .to_owned(),
            );
            return;
        }
        match std::fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                serde_json::from_str::<AppConfig>(&text).map_err(|error| error.to_string())
            }) {
            Ok(config) => {
                self.apply_config(config);
                self.remember_recent_config(&path);
            }
            Err(error) => {
                self.last_error = Some(match self.language {
                    Language::Zh => format!("导入变量名失败: {error}"),
                    Language::En => format!("Failed to import names: {error}"),
                });
            }
        }
    }

    fn selected_channels(&self) -> Vec<usize> {
        let valid_channels = self
            .meta()
            .map(|meta| {
                meta.channels
                    .iter()
                    .map(|channel| channel.index)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.visible
            .iter()
            .enumerate()
            .filter_map(|(index, visible)| {
                (*visible && valid_channels.contains(&index)).then_some(index)
            })
            .collect()
    }

    fn selected_imported_channels(&self, dataset_index: usize) -> Vec<usize> {
        let Some(compare_meta) = self.imported_meta(dataset_index) else {
            return Vec::new();
        };
        let valid_channels = compare_meta
            .channels
            .iter()
            .map(|channel| channel.index)
            .collect::<Vec<_>>();
        self.imported_datasets
            .get(dataset_index)
            .map(|dataset| {
                dataset
                    .visible
                    .iter()
                    .enumerate()
                    .filter_map(|(index, visible)| {
                        (*visible && valid_channels.contains(&index)).then_some(index)
                    })
                    .collect()
            })
            .unwrap_or_default()
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
        let Some(meta) = self.dataset_meta_by_index(dataset_index) else {
            return Vec::new();
        };
        match self.dataset_kind_by_index(dataset_index) {
            Some(SourceKind::Cloud) => meta
                .channels
                .iter()
                .filter(|channel| channel.index < 30)
                .map(|channel| channel.index)
                .collect(),
            _ => meta
                .channels
                .iter()
                .filter(|channel| !Self::looks_like_digital_name(&channel.name))
                .map(|channel| channel.index)
                .collect(),
        }
    }

    fn dataset_meta_by_index(&self, index: usize) -> Option<&DatasetMeta> {
        if index == 0 {
            self.meta()
        } else {
            self.imported_meta(index - 1)
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
        self.selected_channels()
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

    fn channel_is_digital(kind: Option<SourceKind>, channel: &crate::data::ChannelMeta) -> bool {
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

    fn find_sync_channel(meta: &DatasetMeta, target: &str) -> Option<usize> {
        meta.channels
            .iter()
            .find(|channel| Self::sync_channel_key(&channel.name).contains(target))
            .map(|channel| channel.index)
    }

    fn sync_channel_pairs(primary: &DatasetMeta, other: &DatasetMeta) -> Vec<(usize, usize)> {
        ["stvg0ia", "stvg0ib", "stvg0ic"]
            .iter()
            .filter_map(|target| {
                Some((
                    Self::find_sync_channel(primary, target)?,
                    Self::find_sync_channel(other, target)?,
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
    ) -> DataResult<Option<f64>> {
        let primary_meta = primary.metadata();
        let other_meta = other.metadata();
        let pairs = Self::sync_channel_pairs(primary_meta, other_meta);
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
        let Some(primary) = self.source.as_deref() else {
            self.time_sync_status = self.tr("请先导入数据。", "Import data first.").to_owned();
            return;
        };
        if self.imported_datasets.is_empty() {
            self.time_sync_status = self
                .tr(
                    "没有附加数据组需要同步。",
                    "No extra dataset groups to sync.",
                )
                .to_owned();
            return;
        }

        let frequency_hz = self.harmonic_base_hz.max(0.001);
        let mut synced = 0usize;
        let mut failed = 0usize;
        for dataset in &mut self.imported_datasets {
            match Self::phase_sync_offset_for(primary, dataset.source.as_ref(), frequency_hz) {
                Ok(Some(offset)) if offset.is_finite() => {
                    dataset.time_offset = offset;
                    dataset.plot_cache = SampleBlock::default();
                    dataset.plot_summary = None;
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
        self.time_sync_status = if self.language == Language::Zh {
            format!(
                "已同步 {} 组，失败 {} 组；基准频率 {:.3} Hz。",
                synced, failed, frequency_hz
            )
        } else {
            format!(
                "Synced {synced} group(s), failed {failed}; base frequency {frequency_hz:.3} Hz."
            )
        };
    }

    fn clear_time_axis_sync(&mut self) {
        for dataset in &mut self.imported_datasets {
            dataset.time_offset = 0.0;
            dataset.plot_cache = SampleBlock::default();
            dataset.plot_summary = None;
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

    fn draw_points_per_channel(channel_count: usize) -> usize {
        if channel_count == 0 {
            return 0;
        }
        (MAX_TOTAL_DRAW_POINTS / channel_count)
            .clamp(MIN_DRAW_POINTS_PER_CHANNEL, MAX_DRAW_POINTS_PER_CHANNEL)
    }

    fn summary_bins_for_channels(channel_count: usize) -> usize {
        // Each summary bin is drawn as min+max, so use half the raw point budget.
        (Self::draw_points_per_channel(channel_count) / 2).max(128)
    }

    fn reload_plot_cache(&mut self) {
        let Some(source) = &self.source else {
            return;
        };
        let channels = self.selected_channels();
        if channels.is_empty() {
            self.plot_cache = SampleBlock::default();
            self.plot_summary = None;
            self.needs_plot_reload = false;
            return;
        }
        let max_points = Self::draw_points_per_channel(channels.len());
        let summary_bins = Self::summary_bins_for_channels(channels.len());
        let estimated_points = ((self.view_end - self.view_start)
            * source.metadata().nominal_sample_rate_hz)
            .max(0.0) as usize;
        if estimated_points > max_points * 2 {
            match source.summarize_range(self.view_start, self.view_end, &channels, summary_bins) {
                Ok(summary) => {
                    self.plot_cache = SampleBlock::default();
                    self.plot_summary = Some(summary);
                    self.needs_plot_reload = false;
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        } else {
            match source.read_range(self.view_start, self.view_end, &channels, max_points) {
                Ok(block) => {
                    self.plot_cache = block;
                    self.plot_summary = None;
                    self.needs_plot_reload = false;
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
    }

    fn reload_compare_plot_cache(&mut self) {
        let sync_time_axes = self.sync_time_axes;
        for dataset in &mut self.imported_datasets {
            let compare_count = dataset.source.metadata().channels.len();
            let channels = dataset
                .visible
                .iter()
                .enumerate()
                .filter_map(|(channel, visible)| {
                    (*visible && channel < compare_count).then_some(channel)
                })
                .collect::<Vec<_>>();
            if channels.is_empty() {
                dataset.plot_cache = SampleBlock::default();
                dataset.plot_summary = None;
                continue;
            }
            let max_points = Self::draw_points_per_channel(channels.len());
            let summary_bins = Self::summary_bins_for_channels(channels.len());
            let offset = if sync_time_axes {
                dataset.time_offset
            } else {
                0.0
            };
            let meta = dataset.source.metadata();
            let read_start = (self.view_start - offset).max(meta.start_time);
            let read_end = (self.view_end - offset).min(meta.end_time);
            if read_end <= read_start {
                dataset.plot_cache = SampleBlock::default();
                dataset.plot_summary = None;
                continue;
            }
            let estimated_points =
                ((read_end - read_start) * meta.nominal_sample_rate_hz).max(0.0) as usize;
            if estimated_points > max_points * 2 {
                match dataset
                    .source
                    .summarize_range(read_start, read_end, &channels, summary_bins)
                {
                    Ok(summary) => {
                        dataset.plot_cache = SampleBlock::default();
                        dataset.plot_summary = Some(summary);
                    }
                    Err(error) => self.last_error = Some(error.to_string()),
                }
            } else {
                match dataset
                    .source
                    .read_range(read_start, read_end, &channels, max_points)
                {
                    Ok(block) => {
                        dataset.plot_cache = block;
                        dataset.plot_summary = None;
                    }
                    Err(error) => self.last_error = Some(error.to_string()),
                }
            }
        }
        self.needs_compare_plot_reload = false;
    }

    fn visible_time_span(&self) -> f64 {
        (self.view_end - self.view_start).max(f64::EPSILON)
    }

    fn scope_pane_count(&self) -> usize {
        self.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS)
            * self.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS)
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
        pane_count <= 1
            || self
                .channel_panes
                .get(channel_index)
                .copied()
                .unwrap_or(0)
                .min(pane_count.saturating_sub(1))
                == pane_index
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
        self.needs_plot_reload = true;
        self.needs_compare_plot_reload = true;
    }

    fn zoom_y_with_bounds(&mut self, center: f64, factor: f64, current_min: f64, current_max: f64) {
        let old_span = (current_max - current_min).abs().max(f64::EPSILON);
        let new_span = (old_span * factor).max(f64::EPSILON);
        let ratio = ((center - current_min) / old_span).clamp(0.0, 1.0);
        self.y_min = Some(center - ratio * new_span);
        self.y_max = Some(center + (1.0 - ratio) * new_span);
    }

    fn current_y_bounds_for(
        &self,
        selected: &[usize],
        pane_index: usize,
        pane_count: usize,
    ) -> (f64, f64) {
        if let (Some(min), Some(max)) = (self.y_min, self.y_max) {
            if max > min {
                return (min, max);
            }
        }

        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        if let Some(summary) = &self.plot_summary {
            for (out_index, channel_index) in selected.iter().enumerate() {
                if !self.channel_in_scope_pane(*channel_index, pane_index, pane_count)
                    || out_index >= summary.min.len()
                    || out_index >= summary.max.len()
                {
                    continue;
                }
                for i in 0..summary.min[out_index]
                    .len()
                    .min(summary.max[out_index].len())
                {
                    let (scaled_min, scaled_max) = self.scaled_min_max(
                        *channel_index,
                        summary.min[out_index][i],
                        summary.max[out_index][i],
                    );
                    min = min.min(scaled_min);
                    max = max.max(scaled_max);
                }
            }
        } else {
            for (out_index, channel_index) in selected.iter().enumerate() {
                if !self.channel_in_scope_pane(*channel_index, pane_index, pane_count) {
                    continue;
                }
                let Some(values) = self.plot_cache.channels.get(out_index) else {
                    continue;
                };
                for value in values {
                    let value = self.scaled_value(*channel_index, *value);
                    min = min.min(value);
                    max = max.max(value);
                }
            }
        }

        for (dataset_index, dataset) in self.imported_datasets.iter().enumerate() {
            let compare_selected = self.selected_imported_channels(dataset_index);
            if let Some(summary) = &dataset.plot_summary {
                for (out_index, channel_index) in compare_selected.iter().enumerate() {
                    if !self.channel_in_scope_pane(*channel_index, pane_index, pane_count)
                        || out_index >= summary.min.len()
                        || out_index >= summary.max.len()
                    {
                        continue;
                    }
                    for i in 0..summary.min[out_index]
                        .len()
                        .min(summary.max[out_index].len())
                    {
                        let (scaled_min, scaled_max) = self.scaled_min_max(
                            *channel_index,
                            summary.min[out_index][i],
                            summary.max[out_index][i],
                        );
                        min = min.min(scaled_min);
                        max = max.max(scaled_max);
                    }
                }
            }
            for (out_index, channel_index) in compare_selected.iter().enumerate() {
                if !self.channel_in_scope_pane(*channel_index, pane_index, pane_count) {
                    continue;
                }
                let Some(values) = dataset.plot_cache.channels.get(out_index) else {
                    continue;
                };
                for value in values {
                    let value = self.scaled_value(*channel_index, *value);
                    min = min.min(value);
                    max = max.max(value);
                }
            }
        }

        if !min.is_finite() || !max.is_finite() || max <= min {
            return (-1.0, 1.0);
        }
        let padding = ((max - min) * 0.08).max(f64::EPSILON);
        (min - padding, max + padding)
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
        self.needs_plot_reload = true;
        self.needs_compare_plot_reload = true;
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
                                "所选通道是数字量，不做 FFT。",
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
                            "FFT 需要光标区间内至少 16 个样本。",
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
            self.last_error = Some(error);
        } else if self
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("FFT"))
        {
            self.last_error = None;
        }
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
            let distance_sq = |a: Color32, b: Color32| -> i32 {
                let dr = a.r() as i32 - b.r() as i32;
                let dg = a.g() as i32 - b.g() as i32;
                let db = a.b() as i32 - b.b() as i32;
                dr * dr + dg * dg + db * db
            };
            let mut accent_index = (dataset_index - 1) % ACCENTS.len();
            for step in 0..ACCENTS.len() {
                let candidate = ACCENTS[(accent_index + step) % ACCENTS.len()];
                if distance_sq(base, candidate) > 10_000 {
                    accent_index = (accent_index + step) % ACCENTS.len();
                    break;
                }
            }
            Self::blend_color(base, ACCENTS[accent_index], 0.78)
        }
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

    fn plot_channel_color(
        &self,
        channel_index: usize,
        dataset_index: usize,
        pane_index: usize,
        pane_count: usize,
    ) -> Color32 {
        let base = self.channel_color(channel_index);
        if self.pane_dataset_count_for_channel(channel_index, pane_index, pane_count) > 1 {
            Self::dataset_variant_color(base, dataset_index)
        } else {
            base
        }
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
                self.needs_plot_reload = true;
            }
        } else if let Some(dataset) = self.imported_datasets.get_mut(dataset_index - 1) {
            if dataset.line_pattern != pattern {
                dataset.line_pattern = pattern;
                self.needs_compare_plot_reload = true;
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
        let scale = self.channel_scale(index) as f64;
        let a = min as f64 * scale;
        let b = max as f64 * scale;
        if a <= b {
            (a, b)
        } else {
            (b, a)
        }
    }

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
                self.fft_results.clear();
                self.needs_fft_reload = true;
                self.needs_plot_reload = true;
                self.needs_compare_plot_reload = true;
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

    fn tr(&self, zh: &'static str, en: &'static str) -> &'static str {
        match self.language {
            Language::Zh => zh,
            Language::En => en,
        }
    }

    fn icon_label(icon: &str, label: &str) -> String {
        format!("{icon}  {label}")
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

    fn fit_to_cursors(&mut self) {
        self.zoom_to_range(self.cursor_a, self.cursor_b);
    }

    fn toggle_cursor_visibility(&mut self) {
        let show = !(self.show_cursor_a || self.show_cursor_b);
        self.show_cursor_a = show;
        self.show_cursor_b = show;
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let (reset_view, fit_cursors, toggle_cursors, select_all, select_none) =
            ctx.input(|input| {
                (
                    self.shortcuts.reset_view.pressed(input),
                    self.shortcuts.fit_cursors.pressed(input),
                    self.shortcuts.toggle_cursors.pressed(input),
                    self.shortcuts.select_all.pressed(input),
                    self.shortcuts.select_none.pressed(input),
                )
            });

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

    fn scope_layout_menu(&mut self, ui: &mut egui::Ui) {
        ui.strong(self.tr("示波器布局", "Scope Layout"));
        ui.horizontal(|ui| {
            ui.label(self.tr("纵向", "Rows"));
            ui.add(
                egui::Slider::new(&mut self.scope_layout_rows, 1..=MAX_SCOPE_LAYOUT_ROWS)
                    .show_value(true),
            );
        });
        ui.horizontal(|ui| {
            ui.label(self.tr("横向", "Columns"));
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
            self.tr("当前栏", "Active Pane"),
            self.current_scope_pane() + 1
        ));
        ui.label(self.tr(
            "先点击示波器栏，再勾选变量，变量会进入当前栏。",
            "Click a scope pane first, then check variables to place them there.",
        ));
        ui.separator();
        ui.label(self.tr("快速选择", "Quick Select"));
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
            if ui.button(self.tr("单栏", "Single")).clicked() {
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
        let delete_group_label =
            Self::icon_label("\u{E74D}", self.tr("删除数据组", "Delete Dataset"));
        let mut delete_group = None;
        let primary_header = self.dataset_label(0);
        let primary_meta = self.meta().cloned();
        let primary_response = egui::CollapsingHeader::new(primary_header)
            .id_source(("dataset_group", 0usize))
            .default_open(true)
            .show(ui, |ui| {
                if let Some(meta) = &primary_meta {
                    self.channel_sections_ui(ui, 0, meta, filter_terms, hovered_channel);
                }
            });
        let mut delete_primary = false;
        primary_response.header_response.context_menu(|ui| {
            ui.strong(self.tr("数据组设置", "Dataset Settings"));
            ui.horizontal(|ui| {
                ui.label(self.tr("线型", "Line style"));
                let mut pattern = self.dataset_line_pattern(0);
                egui::ComboBox::from_id_source(("dataset_line_pattern", 0usize))
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
                self.set_dataset_line_pattern(0, pattern);
            });
            ui.separator();
            if ui.button(delete_group_label.clone()).clicked() {
                delete_primary = true;
                ui.close_menu();
            }
        });
        if delete_primary {
            delete_group = Some(0);
        }

        for index in 0..self.imported_datasets.len() {
            let header = self.dataset_label(index + 1);
            let dataset_meta = self.imported_meta(index).cloned();
            let response = egui::CollapsingHeader::new(header)
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
            response.header_response.context_menu(|ui| {
                ui.strong(self.tr("数据组设置", "Dataset Settings"));
                ui.horizontal(|ui| {
                    ui.label(self.tr("线型", "Line style"));
                    let dataset_index = index + 1;
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
                if ui.button(delete_group_label.clone()).clicked() {
                    delete_this = true;
                    ui.close_menu();
                }
            });
            if delete_this {
                delete_group = Some(index + 1);
            }
        }

        if let Some(dataset_index) = delete_group {
            self.delete_dataset_group(dataset_index);
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
        let source_label = self.tr("原始", "src");
        let mut analog_entries = Vec::new();
        let mut digital_entries = Vec::new();
        let source_kind = self.dataset_kind_by_index(dataset_index);

        for channel in &meta.channels {
            if channel.index >= self.visible.len() || channel.index >= self.display_names.len() {
                continue;
            }
            let display_name = self.channel_name(channel.index);
            let searchable = format!("{} {}", display_name, channel.name).to_lowercase();
            if !filter_terms.iter().all(|term| searchable.contains(term)) {
                continue;
            }
            if Self::channel_is_digital(source_kind, channel) {
                digital_entries.push((channel.clone(), display_name));
            } else {
                analog_entries.push((channel.clone(), display_name));
            }
        }

        self.channel_section_ui(
            ui,
            dataset_index,
            self.tr("模拟量", "Analog"),
            true,
            &analog_entries,
            source_label,
            hovered_channel,
        );
        self.channel_section_ui(
            ui,
            dataset_index,
            self.tr("数字量", "Digital"),
            false,
            &digital_entries,
            source_label,
            hovered_channel,
        );
    }

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
                    ui.label(self.tr("没有匹配的变量。", "No matching channels."));
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
        ui.horizontal(|ui| {
            ui.menu_button(
                Self::icon_label("\u{E8E5}", self.tr("导入数据", "Import Data")),
                |ui| {
                    if ui
                        .button(Self::icon_label(
                            "\u{E8E5}",
                            self.tr("导入数据", "Import Data"),
                        ))
                        .clicked()
                    {
                        let filter_name = self.tr("波形 CSV", "Waveform CSV");
                        if let Some(paths) = rfd::FileDialog::new()
                            .add_filter(filter_name, &["csv"])
                            .pick_files()
                        {
                            self.import_data_files(paths);
                        }
                        ui.close_menu();
                    }

                    ui.separator();
                    ui.strong(Self::icon_label(
                        "\u{E823}",
                        self.tr("最近文件", "Recent Files"),
                    ));
                    if self.recent_files.is_empty() {
                        ui.label(self.tr("暂无最近文件", "No recent files"));
                    } else {
                        let recent_files = self.recent_files.clone();
                        for path in recent_files {
                            let label = Self::recent_file_label(&path);
                            if path.exists() {
                                if ui.button(label).clicked() {
                                    self.import_data_files(vec![path.clone()]);
                                    ui.close_menu();
                                }
                            } else {
                                ui.label(
                                    RichText::new(format!(
                                        "{} {}",
                                        label,
                                        self.tr("(文件不存在)", "(missing)")
                                    ))
                                    .color(Color32::GRAY),
                                );
                            }
                        }
                        ui.separator();
                        if ui
                            .button(Self::icon_label(
                                "\u{E74D}",
                                self.tr("清空最近文件", "Clear Recent Files"),
                            ))
                            .clicked()
                        {
                            self.clear_recent_files();
                            ui.close_menu();
                        }
                    }
                },
            );
            ui.menu_button(
                Self::icon_label("\u{E80A}", self.tr("布局", "Layout")),
                |ui| self.scope_layout_menu(ui),
            );
            if ui
                .button(Self::icon_label(
                    "\u{E72C}",
                    self.tr("重置视图", "Reset View"),
                ))
                .clicked()
            {
                self.reset_view();
            }
            if ui
                .button(Self::icon_label(
                    "\u{E9A6}",
                    self.tr("适配光标", "Fit Cursors"),
                ))
                .clicked()
            {
                self.fit_to_cursors();
            }
            let config_title = self.tr("配置", "Config");
            ui.menu_button(Self::icon_label("\u{E713}", config_title), |ui| {
                if ui
                    .button(Self::icon_label(
                        "\u{E8B5}",
                        self.tr("导入变量名", "Import Names"),
                    ))
                    .clicked()
                {
                    self.import_config();
                    ui.close_menu();
                }
                if ui
                    .button(Self::icon_label(
                        "\u{EDE1}",
                        self.tr("导出变量名", "Export Names"),
                    ))
                    .clicked()
                {
                    self.export_config();
                    ui.close_menu();
                }
                ui.separator();
                ui.strong(Self::icon_label(
                    "\u{E823}",
                    self.tr("最近配置", "Recent Configs"),
                ));
                if self.recent_configs.is_empty() {
                    ui.label(self.tr("暂无最近配置", "No recent configs"));
                } else {
                    let recent_configs = self.recent_configs.clone();
                    for path in recent_configs {
                        let label = Self::recent_file_label(&path);
                        if path.exists() {
                            if ui.button(label).clicked() {
                                self.import_config_from_path(path);
                                ui.close_menu();
                            }
                        } else {
                            ui.label(
                                RichText::new(format!(
                                    "{} {}",
                                    label,
                                    self.tr("(文件不存在)", "(missing)")
                                ))
                                .color(Color32::GRAY),
                            );
                        }
                    }
                    ui.separator();
                    if ui
                        .button(Self::icon_label(
                            "\u{E74D}",
                            self.tr("清空最近配置", "Clear Recent Configs"),
                        ))
                        .clicked()
                    {
                        self.clear_recent_configs();
                        ui.close_menu();
                    }
                }
            });
            if ui
                .button(Self::icon_label("\u{E713}", self.tr("选项", "Options")))
                .clicked()
            {
                self.show_options = true;
            }
            if ui
                .button(Self::icon_label("\u{E897}", self.tr("帮助", "Help")))
                .clicked()
            {
                self.show_help = true;
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
                        "主数据: {} | {} 点 | {:.3}s | 数据 {:.1} Hz | FFT Fs {:.1} Hz{}",
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
        let title = self.tr("帮助", "Help");
        let language = self.language;
        egui::Window::new(title)
            .open(&mut self.show_help)
            .default_width(720.0)
            .default_height(620.0)
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if language == Language::Zh {
                        ui.heading("Scope Analyzer");
                        ui.label("Windows 离线波形分析工具，支持通道勾选、示波器式缩放、双光标测量、FFT 和 THD 分析。");
                        ui.label("通过顶部“导入数据”菜单一次选择一个或多个 CSV 数据文件。第一个文件作为主数据，后续文件作为附加数据叠加显示。软件会自动识别云端 Content 或本地数值 CSV。");

                        ui.separator();
                        ui.heading("支持的 CSV 格式");
                        ui.label("使用顶部“导入数据”菜单载入数据；可一次选择多个数据文件。软件读取第一行表头后，会自动选择云端 Content 解析器或本地数值 CSV 解析器。");
                        ui.label("主数据决定通道列表、变量名、颜色、线宽、测量和 FFT 结果；附加数据按相同通道序号以虚线叠加显示。");
                        ui.strong("云端 Content CSV");
                        ui.label("第一行为 Content。后续每行是一条十六进制报文，每条报文解析为 2 个采样点。每个采样点包含 30 个模拟量通道和 30 个数字/状态通道。模拟量按 little-endian int16 解析，第 31/32 个 raw word 按原 MATLAB 脚本规则拆成数字/状态通道。");
                        ui.label("云端 Content CSV 没有直接时间列，软件使用“选项”里的 FFT Fs 生成秒级时间轴，默认 1000 Hz。");
                        ui.add_space(6.0);
                        ui.strong("本地/数值 CSV");
                        ui.label("第一列为时间，单位秒；后续列为通道值，最多读取 128 个数值通道。文件打开时建立分块索引和 min/max 摘要，绘图只读取当前视窗或摘要。");
                        ui.monospace("time,CH1,CH2,CH3\n0.000000,0.0,0.1,0.2\n0.000010,0.1,0.2,0.3");
                        ui.label("大文件不会一次性全部载入内存；缩小时绘制 min/max 包络，放大后读取原始采样点。绘图使用总点数预算，通道越多每个通道分配的绘图点越少，缩放和平移只替换当前窗口缓存，避免内存随操作次数持续上涨。");

                        ui.separator();
                        ui.heading("波形操作");
                        ui.label("导入数据菜单：一次选择一个或多个波形文件；第一个作为主数据，后续作为附加数据并以虚线叠加。可在菜单里勾选一组或多组数据后删除。");
                        ui.label("最近文件：导入成功的 CSV 会自动加入列表，可从顶部“导入数据”菜单重新载入，也可以清空列表。列表保存为程序目录下的 scope-recent-files.json。");
                        ui.label("布局：可设置示波器纵向行数和横向列数。点击某个子窗口会选中该栏，再勾选左侧变量，变量会进入当前栏；所有子窗口共享时间轴和光标。");
                        ui.label("选项：设置 FFT Fs 和谐波基准频率。FFT Fs 默认 1000 Hz，云端 Content CSV 同时用它生成秒级时间轴；谐波基准频率默认 50 Hz。");
                        ui.label("鼠标滚轮：以鼠标位置为中心缩放纵轴幅值范围。");
                        ui.label("Ctrl + 鼠标滚轮/触控板滚动：以鼠标位置为中心缩放横轴时间范围；未按 Ctrl 时始终缩放纵轴。");
                        ui.label("选项：可调整滚轮缩放敏感度，也可切换中文/英文界面和浅色/深色主题。");
                        ui.label("左侧变量栏：按通道顺序直接显示变量，不再分组；可全选/全不选，双击变量名可编辑显示名，也可设置颜色。右键变量名可配置放大/缩小变比；颜色设置里可配置颜色、线形和线宽。搜索支持多个关键词，并会匹配显示名和原始名；有搜索条件时，全选/全不选只作用于筛选结果。载入 B 后，A 用所选线形显示，B 用同一通道颜色、线宽和倍率按相同通道序号虚线叠加。");
                        ui.label("鼠标悬停左侧变量：对应波形会加粗高亮。");
                        ui.label("导入/导出变量名：只保存和恢复变量名，不会覆盖快捷键、通道显示、颜色、线宽、倍率、FFT 设置、界面语言或主题。导入或导出成功的文件会显示在顶部“配置”的最近配置中，可直接选择复用。");
                        ui.label("左键单击波形：移动距离最近的光标。");
                        ui.label("左键拖拽波形：框选时间区域并放大。");
                        ui.label("右键单击波形：打开光标菜单。");
                        ui.label("放置光标 X1/X2：显示红色虚线预览光标，左键确认，Esc 取消。");
                        ui.label("隐藏/显示光标 X1/X2：只切换显示状态，不改变光标位置和测量结果。");
                        ui.label("右键拖拽波形：平移当前视图。");
                        ui.label("适配光标：缩放到光标 X1/X2 的时间范围。");
                        ui.label("快捷键可在“选项”里配置，默认：R 复位视图，F 适配光标，H 隐藏/显示 X1/X2 光标，Ctrl+A 全选通道，Ctrl+D 取消全选。");
                        ui.label("测量：右侧测量面板会对 X1/X2 区间内的已选通道用表格显示 Y1、Y2、ΔY、最大值和最小值；这些数值使用倍率后的通道值。");

                        ui.separator();
                        ui.heading("FFT 和 THD");
                        ui.label("FFT 面板可选择数据组和通道，分析光标 X1/X2 之间对应数据组的波形，使用倍率后的通道值。");
                        ui.label("计算前会去除直流均值并使用 Hann 窗，FFT 点数取当前选区样本数的 next power of two，最多读取 262144 点。");
                        ui.label("谐波基准频率可在选项中设置，默认 50 Hz；谐波表按该基准显示 0 次直流量和 1-10 次的幅值、相位、相对基波比例和 THD。");
                        ui.label("THD = 2 次及以上谐波平方和开根号 / 1 次谐波幅值。若选区太短或基准频率不匹配，结果需要结合波形判断。");
                    } else {
                        ui.heading("Scope Analyzer");
                        ui.label("Windows offline waveform analyzer with channel selection, oscilloscope-style zooming, cursor measurement, FFT, and THD analysis.");
                        ui.label("Use the Import Data menu to select one or more CSV data files. The first file becomes the primary dataset, and later files are overlaid as extra datasets. Content files are detected automatically.");

                        ui.separator();
                        ui.heading("Supported CSV Formats");
                        ui.label("Use the Import Data menu to load data. You can select multiple data files at once. The software reads the first CSV header and automatically chooses the cloud Content parser or the local numeric CSV parser.");
                        ui.label("The primary dataset controls the channel list, display names, colors, line widths, measurements, and FFT. Extra datasets are overlaid as dashed lines by matching channel index.");
                        ui.strong("Cloud Content CSV");
                        ui.label("The first row is Content. Each following row is a hexadecimal record. Each record is decoded into two samples. Each sample contains 30 analog channels plus 30 digital/status channels. Analog channels use little-endian int16. The 31st and 32nd raw words are expanded into digital/status channels according to the original MATLAB script.");
                        ui.label("Cloud Content CSV has no explicit time column, so FFT Fs in Options is used to generate the time axis. The default is 1000 Hz.");
                        ui.add_space(6.0);
                        ui.strong("Local / Numeric CSV");
                        ui.label("The first column is time in seconds. Remaining columns are channel values. Up to 128 numeric channels are loaded. The file is indexed in blocks and the plot reads only the current view or min/max summaries.");
                        ui.monospace("time,CH1,CH2,CH3\n0.000000,0.0,0.1,0.2\n0.000010,0.1,0.2,0.3");
                        ui.label("Large files are not loaded fully into memory. Zoomed-out views draw min/max envelopes; zoomed-in views read raw samples. Plotting uses a total point budget, so more channels receive fewer points per channel. Zoom and pan replace the current window cache instead of accumulating data.");

                        ui.separator();
                        ui.heading("Waveform Controls");
                        ui.label("Import Data menu: select one or more waveform files. The first becomes the primary dataset; later files are extra datasets overlaid as dashed lines. Use the dataset checklist in the menu to delete one or more datasets.");
                        ui.label("Recent Files: successfully imported CSV files are added automatically. Use the top Import Data menu to reopen an item, or clear the list. The list is stored as scope-recent-files.json next to the executable.");
                        ui.label("Layout: configure scope rows and columns. Click a pane to select it, then check variables on the left to place them in the active pane. All panes share the time axis and cursors.");
                        ui.label("Options: set FFT Fs and harmonic base frequency. FFT Fs defaults to 1000 Hz and is also used to convert Cloud Content CSV sample index to seconds; harmonic base frequency defaults to 50 Hz.");
                        ui.label("Mouse wheel: zoom vertical amplitude range around the pointer.");
                        ui.label("Ctrl + mouse wheel / touchpad scroll: zoom horizontal time range around the pointer; without Ctrl, pointer zoom always changes the vertical axis.");
                        ui.label("Options: adjust mouse wheel zoom sensitivity and choose Chinese/English UI language plus light/dark theme.");
                        ui.label("Left channel list: channels are shown directly in channel order without grouping. Use All/None controls, double-click a variable name to edit its display name, and set colors inline. Right-click a variable name to configure scale ratio; Color Settings configure color, line style, and line width. Search supports multiple keywords and matches display name and original name; when search is active, All/None only affects filtered results. After B is loaded, A uses the selected line style and B is dashed with the same channel color, width, scale, and matching channel index.");
                        ui.label("Hover a variable in the left list: the corresponding waveform becomes thicker.");
                        ui.label("Import/Export Names: only save and restore display names. Shortcuts, channel visibility, colors, line widths, scale factors, FFT settings, UI language, and theme are not overwritten. Successfully imported or exported files appear under Recent Configs in the top Config menu for quick reuse.");
                        ui.label("Left click plot: move the nearest cursor to the clicked position.");
                        ui.label("Left drag plot: box-select a time range and zoom in.");
                        ui.label("Right click plot: open cursor menu.");
                        ui.label("Place Cursor X1/X2: shows a red dashed preview cursor; left click confirms, Esc cancels.");
                        ui.label("Hide/Show Cursor X1/X2: toggles cursor visibility without changing cursor position or measurements.");
                        ui.label("Right drag plot: pan the current view.");
                        ui.label("Fit Cursors: zoom to the time range between cursor A and cursor B.");
                        ui.label("Shortcuts can be configured in Options. Defaults: R resets view, F fits cursors, H hides/shows X1/X2 cursors, Ctrl+A selects all channels, Ctrl+D deselects all channels.");
                        ui.label("Measurements: the right panel shows Y1, Y2, ΔY, max, and min in a table for selected channels in the X1-X2 range. Values use the channel scale factor.");

                        ui.separator();
                        ui.heading("FFT and THD");
                        ui.label("The FFT panel can choose a dataset group and channel, then analyzes that dataset between cursor X1 and cursor X2 using the scaled channel values.");
                        ui.label("Before FFT, the DC mean is removed and a Hann window is applied. FFT length is the next power of two for the selected samples, with up to 262144 points read.");
                        ui.label("The harmonic base frequency can be set in Options. Default is 50 Hz. The harmonic table shows the 0th DC component plus amplitude, phase, percent of fundamental, and THD for the 1st-10th orders.");
                        ui.label("THD is sqrt(sum of harmonic powers from the 2nd harmonic upward) divided by the 1st harmonic amplitude. Short selections or mismatched base frequency should be interpreted with the waveform.");
                    }
                });
            });
    }

    fn options_window(&mut self, ctx: &egui::Context) {
        let title = self.tr("选项", "Options");
        let mut open = self.show_options;
        egui::Window::new(title)
            .open(&mut open)
            .default_width(360.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading(self.tr("交互", "Interaction"));
                ui.horizontal(|ui| {
                    ui.label(self.tr("系统语言", "Language"));
                    egui::ComboBox::from_id_source("language_select")
                        .selected_text(self.language.label())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.language, Language::Zh, "中文");
                            ui.selectable_value(&mut self.language, Language::En, "English");
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(self.tr("主题", "Theme"));
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
                let old_sample_rate = self.sample_rate_hz;
                let sample_rate_prefix = self.tr("FFT Fs: ", "FFT Fs: ");
                ui.add(
                    egui::DragValue::new(&mut self.sample_rate_hz)
                        .speed(10.0)
                        .clamp_range(1.0..=10_000_000.0)
                        .suffix(" Hz")
                        .prefix(sample_rate_prefix),
                );
                if (self.sample_rate_hz - old_sample_rate).abs() > f64::EPSILON {
                    self.sample_rate_hz = self.sample_rate_hz.clamp(1.0, 10_000_000.0);
                    self.reload_cloud_with_current_sample_rate();
                }
                ui.label(self.tr(
                    "默认 FFT Fs 为 1000 Hz。云端 Content CSV 会用该值生成时间轴；FFT 频率轴也明确使用该值。",
                    "Default FFT Fs is 1000 Hz. Cloud Content CSV uses this value for the time axis; the FFT frequency axis explicitly uses it too.",
                ));
                let old_harmonic_base = self.harmonic_base_hz;
                let harmonic_base_prefix = self.tr("谐波基准: ", "Harmonic base: ");
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
                }
                ui.label(self.tr(
                    "谐波明细按该基准频率显示 0 次直流量和 1-10 次谐波，默认 50 Hz。",
                    "Harmonics show the 0th DC component and the 1st-10th orders using this base frequency. Default is 50 Hz.",
                ));
                ui.separator();
                ui.heading(self.tr("时间轴同步", "Time Axis Sync"));
                let previous_sync = self.sync_time_axes;
                let sync_axes_label = self.tr("统一数据组时间轴", "Align dataset time axes");
                ui.checkbox(&mut self.sync_time_axes, sync_axes_label);
                if self.sync_time_axes != previous_sync {
                    self.needs_compare_plot_reload = true;
                    self.fft_results.clear();
                    self.needs_fft_reload = true;
                }
                ui.horizontal(|ui| {
                    if ui
                        .button(self.tr("按 stVg_0.iA/iB/iC 相位同步", "Sync by stVg_0.iA/iB/iC phase"))
                        .clicked()
                    {
                        self.sync_time_axes_by_phase();
                    }
                    if ui.button(self.tr("清除同步", "Clear Sync")).clicked() {
                        self.clear_time_axis_sync();
                    }
                });
                ui.label(self.tr(
                    "以主数据为基准，按谐波基准频率计算三相电压相位差，并平移附加数据时间轴。",
                    "Uses the primary dataset as reference, calculates three-phase voltage phase difference at the harmonic base frequency, then shifts extra dataset time axes.",
                ));
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
                let zoom_label = self.tr("滚轮缩放敏感度", "Wheel zoom sensitivity");
                ui.add(
                    egui::Slider::new(
                        &mut self.wheel_zoom_sensitivity,
                        MIN_WHEEL_ZOOM_SENSITIVITY..=MAX_WHEEL_ZOOM_SENSITIVITY,
                    )
                        .text(zoom_label)
                        .logarithmic(false),
                );
                if self.language == Language::Zh {
                    ui.label(format!("当前: 每格滚轮 {:.1}%", self.wheel_zoom_sensitivity * 100.0));
                } else {
                    ui.label(format!(
                        "Current: {:.1}% per wheel step",
                        self.wheel_zoom_sensitivity * 100.0
                    ));
                }
                if ui.button(self.tr("重置敏感度", "Reset Sensitivity")).clicked() {
                    self.wheel_zoom_sensitivity = DEFAULT_WHEEL_ZOOM_SENSITIVITY;
                }
                ui.separator();
                ui.heading(self.tr("快捷键", "Shortcuts"));
                let reset_view_label = self.tr("复位视图", "Reset View");
                let fit_cursors_label = self.tr("适配光标", "Fit Cursors");
                let toggle_cursors_label = self.tr("隐藏/显示光标", "Hide/Show Cursors");
                let select_all_label = self.tr("全选通道", "Select All Channels");
                let select_none_label = self.tr("取消全选", "Deselect All Channels");
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
                if ui.button(self.tr("重置快捷键", "Reset Shortcuts")).clicked() {
                    self.shortcuts = ShortcutConfig::default();
                }
                ui.separator();
                ui.label(self.tr(
                    "鼠标滚轮缩放纵轴。",
                    "Mouse wheel zooms the vertical axis.",
                ));
                ui.label(self.tr(
                    "Ctrl + 鼠标滚轮/触控板滚动缩放横轴；不按 Ctrl 缩放纵轴。",
                    "Ctrl + mouse wheel / touchpad scroll zooms the horizontal axis; without Ctrl it zooms the vertical axis.",
                ));
            });
        self.show_options = open;
    }

    fn shortcut_binding_ui(
        ui: &mut egui::Ui,
        id: &'static str,
        label: &'static str,
        binding: &mut ShortcutBinding,
    ) {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.checkbox(&mut binding.ctrl, "Ctrl");
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

    fn channel_row_ui(
        &mut self,
        ui: &mut egui::Ui,
        dataset_index: usize,
        channel: &crate::data::ChannelMeta,
        display_name: &str,
        source_label: &str,
    ) -> bool {
        if channel.index >= self.display_names.len()
            || (dataset_index == 0 && channel.index >= self.visible.len())
            || (dataset_index > 0
                && self
                    .imported_datasets
                    .get(dataset_index - 1)
                    .map_or(true, |dataset| channel.index >= dataset.visible.len()))
        {
            return false;
        }
        ui.push_id(("channel_row", dataset_index, channel.index), |ui| {
            let mut name_context_response: Option<egui::Response> = None;
            let row_response = ui.horizontal(|ui| {
                let mut row_hovered = false;
                let mut add_from_name = false;
                let mut color = self.plot_channel_color(
                    channel.index,
                    dataset_index,
                    self.current_scope_pane(),
                    self.scope_pane_count(),
                );
                let color_response = egui::color_picker::color_edit_button_srgba(
                    ui,
                    &mut color,
                    egui::color_picker::Alpha::Opaque,
                );
                row_hovered |= color_response.hovered();
                if color_response.changed() {
                    if let Some(stored_color) = self.channel_colors.get_mut(channel.index) {
                        *stored_color = color;
                        self.needs_plot_reload = true;
                        self.needs_compare_plot_reload = true;
                    }
                }
                let mut visible = self.dataset_channel_visible(dataset_index, channel.index);
                let checkbox_response = ui.checkbox(&mut visible, "");
                row_hovered |= checkbox_response.hovered();
                if checkbox_response.changed() {
                    self.set_dataset_channel_visible(dataset_index, channel.index, visible);
                }
                let rename_hint = self.tr("双击修改变量名", "Double-click to rename");
                if let Some(name) = self.display_names.get_mut(channel.index) {
                    let name_width = 150.0;
                    if self.editing_display_name == Some(channel.index) {
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
                        if name_response.changed() {
                            self.fft_results.clear();
                            self.needs_fft_reload = true;
                        }
                        let finish_edit = ui.input(|input| {
                            input.key_pressed(egui::Key::Enter)
                                || input.key_pressed(egui::Key::Escape)
                        }) || (!just_requested_focus
                            && name_response.lost_focus());
                        if finish_edit {
                            if ui.input(|input| {
                                input.key_pressed(egui::Key::Enter)
                                    || input.key_pressed(egui::Key::Escape)
                            }) {
                                name_response.surrender_focus();
                            }
                            self.editing_display_name = None;
                        }
                    } else {
                        let label_response = ui
                            .add_sized(
                                [name_width, ui.spacing().interact_size.y],
                                egui::Label::new(name.as_str())
                                    .sense(egui::Sense::click())
                                    .truncate(true),
                            )
                            .on_hover_text(rename_hint);
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
                }
                if add_from_name {
                    self.set_dataset_channel_visible(dataset_index, channel.index, true);
                }
                if !channel.unit.is_empty() {
                    row_hovered |= ui.label(format!("({})", channel.unit)).hovered();
                }
                if display_name != channel.name {
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
        ui.strong(self.tr("颜色设置", "Color Settings"));
        ui.horizontal(|ui| {
            ui.label(self.tr("颜色", "Color"));
            let mut color = self.channel_color(channel_index);
            let color_response = egui::color_picker::color_edit_button_srgba(
                ui,
                &mut color,
                egui::color_picker::Alpha::Opaque,
            );
            if color_response.changed() {
                if let Some(stored_color) = self.channel_colors.get_mut(channel_index) {
                    *stored_color = color;
                    self.needs_plot_reload = true;
                    self.needs_compare_plot_reload = true;
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(self.tr("线形", "Line style"));
            if let Some(pattern) = self.line_patterns.get_mut(channel_index) {
                let old_pattern = *pattern;
                egui::ComboBox::from_id_source(("line_pattern", channel_index))
                    .selected_text(pattern.label(self.language))
                    .show_ui(ui, |ui| {
                        for candidate in ChannelLinePattern::ALL {
                            ui.selectable_value(pattern, candidate, candidate.label(self.language));
                        }
                    });
                if *pattern != old_pattern {
                    self.needs_plot_reload = true;
                    self.needs_compare_plot_reload = true;
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(self.tr("线宽", "Line width"));
            if let Some(width) = self.line_widths.get_mut(channel_index) {
                let width_response = ui.add(
                    egui::DragValue::new(width)
                        .speed(0.1)
                        .clamp_range(MIN_CHANNEL_LINE_WIDTH..=MAX_CHANNEL_LINE_WIDTH),
                );
                if width_response.changed() {
                    *width = (*width).clamp(MIN_CHANNEL_LINE_WIDTH, MAX_CHANNEL_LINE_WIDTH);
                    self.needs_plot_reload = true;
                    self.needs_compare_plot_reload = true;
                }
            }
        });

        ui.separator();
        ui.strong(self.tr("变比", "Scale Ratio"));
        ui.horizontal(|ui| {
            ui.label(self.tr("倍率", "Scale"));
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
                    self.fft_results.clear();
                    self.needs_fft_reload = true;
                    self.needs_plot_reload = true;
                    self.needs_compare_plot_reload = true;
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button(self.tr("放大 2x", "Zoom 2x")).clicked() {
                self.multiply_channel_scale(channel_index, 2.0);
            }
            if ui.button(self.tr("缩小 1/2", "Shrink 1/2")).clicked() {
                self.multiply_channel_scale(channel_index, 0.5);
            }
            if ui.button(self.tr("重置", "Reset")).clicked() {
                self.set_channel_scale(channel_index, DEFAULT_CHANNEL_SCALE);
            }
        });
    }

    fn channel_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading(self.tr("变量", "Channels"));
        if self.scope_pane_count() > 1 {
            ui.label(format!(
                "{} {}",
                self.tr("当前栏", "Active pane"),
                self.current_scope_pane() + 1
            ));
        }
        let filter_hint = self.tr(
            "筛选变量，支持多关键词",
            "Filter channels, multiple keywords",
        );
        ui.horizontal(|ui| {
            let clear_width = if self.channel_filter.is_empty() {
                0.0
            } else {
                52.0
            };
            let filter_width = (ui.available_width() - clear_width).clamp(80.0, 220.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.channel_filter)
                    .hint_text(filter_hint)
                    .desired_width(filter_width),
            );
            if !self.channel_filter.is_empty() && ui.button(self.tr("清除", "Clear")).clicked() {
                self.channel_filter.clear();
            }
        });

        let Some(meta) = self.meta().cloned() else {
            ui.label(self.tr("未加载数据。", "No data loaded."));
            return;
        };
        let filter_terms = self
            .channel_filter
            .split_whitespace()
            .map(|term| term.to_lowercase())
            .collect::<Vec<_>>();
        let entries = meta
            .channels
            .iter()
            .filter_map(|channel| {
                let display_name = self.channel_name(channel.index);
                let searchable = format!("{} {}", display_name, channel.name).to_lowercase();
                if !filter_terms.iter().all(|term| searchable.contains(term)) {
                    return None;
                }
                Some((channel.clone(), display_name))
            })
            .collect::<Vec<_>>();
        let filtered_indexes = entries
            .iter()
            .map(|(channel, _)| channel.index)
            .collect::<Vec<_>>();
        ui.horizontal(|ui| {
            if ui.button(self.tr("全选", "All")).clicked() {
                if filter_terms.is_empty() {
                    self.set_all_channels_visible(true);
                } else {
                    self.set_channels_visible(&filtered_indexes, true);
                }
            }
            if ui.button(self.tr("全不选", "None")).clicked() {
                if filter_terms.is_empty() {
                    self.set_all_channels_visible(false);
                } else {
                    self.set_channels_visible(&filtered_indexes, false);
                }
            }
        });
        ui.separator();

        let mut hovered_channel = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.strong(self.tr("数据组", "Datasets"));
            self.dataset_groups_ui(ui, &filter_terms, &mut hovered_channel);
        });
        if entries.is_empty() {
            ui.label(self.tr("没有匹配的变量。", "No matching channels."));
        }
        self.hovered_channel = hovered_channel;
    }

    fn measurements_panel(&mut self, ui: &mut egui::Ui) {
        let hidden_label = self.tr("（隐藏）", " (hidden)");
        let dt = (self.cursor_b - self.cursor_a).abs();

        ui.heading(self.tr("测量", "Measurements"));
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
            ui.label(format!("ΔX: {:.5}s", dt));
            if dt > 0.0 {
                ui.label(format!("1/dt: {:.3} Hz", 1.0 / dt));
            }
        });
        if let Some(cursor) = self.cursor_place_mode {
            if self.language == Language::Zh {
                ui.label(format!(
                    "正在放置光标 {}：单击波形固定，Esc 取消。",
                    Self::cursor_label(cursor)
                ));
            } else {
                ui.label(format!(
                    "Placing cursor {}: click waveform to fix, Esc to cancel.",
                    Self::cursor_label(cursor)
                ));
            }
        }
        ui.add_space(4.0);

        if self.source.is_none() {
            return;
        }
        let channels = self.selected_channels();
        if channels.is_empty() {
            return;
        }
        let measurement_channels = channels.iter().copied().take(12).collect::<Vec<_>>();
        let start = self.cursor_a.min(self.cursor_b);
        let end = self.cursor_a.max(self.cursor_b);

        let cache_matches = match &self.measurement_cache {
            Some(cache) => {
                cache.start == start && cache.end == end && cache.channels == measurement_channels
            }
            None => false,
        };
        if !cache_matches {
            let mut rows = Vec::new();
            if let Some(source) = self.source.as_ref() {
                if let Ok(block) =
                    source.read_range(start, end, &measurement_channels, MAX_AUTO_MEASURE_POINTS)
                {
                    for (out_index, &channel_index) in measurement_channels.iter().enumerate() {
                        let Some(values) = block.channels.get(out_index) else {
                            continue;
                        };
                        let scaled_values = self.scaled_samples(channel_index, values);
                        if let Some(measurement) = Self::auto_measure(&block.times, &scaled_values)
                        {
                            rows.push((channel_index, measurement));
                        }
                    }
                }
            }
            self.measurement_cache = Some(MeasurementCache {
                start,
                end,
                channels: measurement_channels,
                rows,
            });
        }

        let Some(cache) = &self.measurement_cache else {
            return;
        };
        egui::ScrollArea::horizontal().show(ui, |ui| {
            egui::Grid::new("measurement_table")
                .striped(true)
                .num_columns(6)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(
                            MEASUREMENT_CHANNEL_COLUMN_WIDTH,
                            ui.spacing().interact_size.y,
                        ),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(self.tr("通道", "Channel")).strong(),
                                )
                                .truncate(true),
                            );
                        },
                    );
                    ui.strong("Y1");
                    ui.strong("Y2");
                    ui.strong("ΔY");
                    ui.strong(self.tr("最小", "Min"));
                    ui.strong(self.tr("最大", "Max"));
                    ui.end_row();

                    for (channel_index, measurement) in &cache.rows {
                        let color = self.channel_color(*channel_index);
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
                        let channel_name = self.channel_name(*channel_index);
                        ui.allocate_ui_with_layout(
                            egui::vec2(
                                MEASUREMENT_CHANNEL_COLUMN_WIDTH,
                                ui.spacing().interact_size.y,
                            ),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.add(egui::Label::new(text(channel_name.clone())).truncate(true))
                            },
                        )
                        .inner
                        .on_hover_text(channel_name);
                        ui.label(text(format!("{:.2}", measurement.first)));
                        ui.label(text(format!("{:.2}", measurement.last)));
                        ui.label(text(format!("{:.2}", measurement.last - measurement.first)));
                        ui.label(text(format!("{:.2}", measurement.min)));
                        ui.label(text(format!("{:.2}", measurement.max)));
                        ui.end_row();
                    }
                });
        });
    }

    fn fft_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("FFT");
        if self.meta().is_none() {
            ui.label(self.tr("未加载数据。", "No data loaded."));
            return;
        }

        self.fft_dataset_index = self.selected_fft_dataset_index();
        let mut fft_dataset_changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(self.tr("数据组", "Dataset"));
            egui::ComboBox::from_id_source("fft_dataset_select")
                .selected_text(self.dataset_label(self.fft_dataset_index))
                .show_ui(ui, |ui| {
                    for dataset_index in 0..self.dataset_count() {
                        let dataset_label = self.dataset_label(dataset_index);
                        if ui
                            .selectable_value(
                                &mut self.fft_dataset_index,
                                dataset_index,
                                dataset_label,
                            )
                            .changed()
                        {
                            fft_dataset_changed = true;
                        }
                    }
                });
        });
        if fft_dataset_changed {
            self.fft_channel_user_selected = false;
            self.fft_results.clear();
            self.needs_fft_reload = true;
        }

        let fft_channels = self.fft_channel_options();
        if fft_channels.is_empty() {
            self.fft_results.clear();
            self.needs_fft_reload = false;
            ui.label(self.tr(
                "没有可用于 FFT 的模拟量通道。",
                "No analog channels are available for FFT.",
            ));
            return;
        }
        let preferred_fft_channel = self.preferred_fft_channel(&fft_channels);
        let should_use_preferred = !fft_channels.contains(&self.fft_channel)
            || (!self.fft_channel_user_selected && Some(self.fft_channel) != preferred_fft_channel);
        if should_use_preferred {
            self.fft_channel = preferred_fft_channel.unwrap_or(fft_channels[0]);
            self.fft_results.clear();
            self.needs_fft_reload = true;
        }
        let mut fft_channel_changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(self.tr("通道", "Channel"));
            egui::ComboBox::from_id_source("fft_channel_select")
                .selected_text(self.fft_channel_name(self.fft_dataset_index, self.fft_channel))
                .show_ui(ui, |ui| {
                    for channel_index in &fft_channels {
                        let channel_name =
                            self.fft_channel_name(self.fft_dataset_index, *channel_index);
                        if ui
                            .selectable_value(&mut self.fft_channel, *channel_index, channel_name)
                            .changed()
                        {
                            fft_channel_changed = true;
                        }
                    }
                });
        });
        if fft_channel_changed {
            self.fft_channel_user_selected = true;
            self.fft_results.clear();
            self.needs_fft_reload = true;
        }

        if self.needs_fft_reload {
            self.run_fft();
        }

        if let Some((channel_index, result)) = self.fft_results.first() {
            ui.horizontal_wrapped(|ui| {
                ui.label(if self.language == Language::Zh {
                    format!("样本数: {}", result.sample_count)
                } else {
                    format!("Samples: {}", result.sample_count)
                });
                ui.label(
                    RichText::new(format!("THD {:.3}%", result.thd_percent))
                        .color(self.channel_color(*channel_index)),
                );
            });
            ui.separator();
            Plot::new("fft_harmonic_bar_plot")
                .height(180.0)
                .include_y(0.0)
                .include_x(0.0)
                .show(ui, |plot_ui| {
                    let bars = result
                        .harmonics
                        .iter()
                        .map(|row| Bar::new(row.order as f64, row.amplitude as f64).width(0.7))
                        .collect::<Vec<_>>();
                    plot_ui.bar_chart(
                        BarChart::new(bars)
                            .name(&result.channel_name)
                            .color(self.channel_color(*channel_index)),
                    );
                });
            ui.separator();
            egui::Grid::new(("harmonics", *channel_index))
                .striped(true)
                .num_columns(4)
                .show(ui, |ui| {
                    ui.strong(self.tr("次数", "Order"));
                    ui.strong(self.tr("幅值", "Amplitude"));
                    ui.strong(self.tr("相位", "Phase"));
                    ui.strong(self.tr("相对基波比例", "% Fundamental"));
                    ui.end_row();
                    for row in &result.harmonics {
                        let order_text = if self.language == Language::Zh {
                            format!("{}次", row.order)
                        } else {
                            row.order.to_string()
                        };
                        let phase_text = if row.order == 0 || !row.phase_deg.is_finite() {
                            "--".to_owned()
                        } else {
                            format!("{:.2}", row.phase_deg)
                        };
                        if row.order == 1 {
                            ui.strong(order_text);
                            ui.strong(format!("{:.6}", row.amplitude));
                            ui.strong(phase_text);
                            ui.strong(format!("{:.2}%", row.relative_percent));
                        } else {
                            ui.label(order_text);
                            ui.label(format!("{:.6}", row.amplitude));
                            ui.label(phase_text);
                            ui.label(format!("{:.2}%", row.relative_percent));
                        }
                        ui.end_row();
                    }
                });
        } else {
            ui.label(self.tr(
                "FFT 需要光标区间内至少 16 个样本。",
                "FFT needs at least 16 samples in the cursor range.",
            ));
        }
    }

    fn plot_panel(&mut self, ui: &mut egui::Ui) {
        if self.needs_plot_reload {
            self.reload_plot_cache();
        }
        if self.needs_compare_plot_reload {
            self.reload_compare_plot_cache();
        }

        let selected = self.selected_channels();
        let rows = self.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS);
        let cols = self.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS);
        let pane_count = rows * cols;
        self.active_scope_pane = self.active_scope_pane.min(pane_count.saturating_sub(1));
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

        if pane_count <= 1 {
            self.draw_scope_pane(ui, 0, 1, &selected, pane_width, pane_height);
            return;
        }

        ui.vertical(|ui| {
            for row in 0..rows {
                ui.horizontal(|ui| {
                    for col in 0..cols {
                        let pane_index = row * cols + col;
                        ui.allocate_ui(egui::vec2(pane_width, pane_height), |ui| {
                            self.draw_scope_pane(
                                ui,
                                pane_index,
                                pane_count,
                                &selected,
                                pane_width,
                                pane_height,
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
        selected: &[usize],
        pane_width: f32,
        pane_height: f32,
    ) {
        let (plot_y_min, plot_y_max) = self.current_y_bounds_for(selected, pane_index, pane_count);
        let show_dataset_legend = pane_count > 1;
        let mut plot = Plot::new(format!("scope_plot_{pane_index}"))
            .width(pane_width)
            .height(pane_height)
            .allow_drag(false)
            .allow_scroll(false)
            .allow_zoom(false);
        if show_dataset_legend {
            plot = plot.legend(Legend::default());
        }
        let primary_legend_prefix = if show_dataset_legend {
            Some(format!("数据{}", Self::dataset_letter(0)))
        } else {
            None
        };
        let response = plot.show(ui, |plot_ui| {
            plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                [self.view_start, plot_y_min],
                [self.view_end, plot_y_max],
            ));

            for (out_index, channel_index) in selected.iter().enumerate() {
                if !self.channel_in_scope_pane(*channel_index, pane_index, pane_count)
                    || out_index >= self.plot_cache.channels.len()
                {
                    continue;
                }
                let raw_points = self
                    .plot_cache
                    .times
                    .iter()
                    .zip(self.plot_cache.channels[out_index].iter())
                    .map(|(time, value)| [*time, self.scaled_value(*channel_index, *value)])
                    .collect::<Vec<_>>();
                let channel_name = self.channel_name(*channel_index);
                let legend_name = if show_dataset_legend {
                    format!(
                        "{}: {channel_name}",
                        primary_legend_prefix.as_deref().unwrap_or("")
                    )
                } else {
                    channel_name
                };
                let line_color = self.plot_channel_color(*channel_index, 0, pane_index, pane_count);
                plot_ui.line(
                    Line::new(PlotPoints::from(raw_points))
                        .name(legend_name)
                        .color(line_color)
                        .style(self.channel_line_pattern(*channel_index).plot_style())
                        .width(self.visible_line_width(*channel_index)),
                );
            }

            if let Some(summary) = &self.plot_summary {
                for (out_index, channel_index) in selected.iter().enumerate() {
                    if !self.channel_in_scope_pane(*channel_index, pane_index, pane_count)
                        || out_index >= summary.min.len()
                        || out_index >= summary.max.len()
                    {
                        continue;
                    }
                    let mut envelope = Vec::with_capacity(summary.bin_start.len() * 2);
                    for i in 0..summary.bin_start.len() {
                        let mid = (summary.bin_start[i] + summary.bin_end[i]) * 0.5;
                        let (scaled_min, scaled_max) = self.scaled_min_max(
                            *channel_index,
                            summary.min[out_index][i],
                            summary.max[out_index][i],
                        );
                        envelope.push([mid, scaled_min]);
                        envelope.push([mid, scaled_max]);
                    }
                    let channel_name = self.channel_name(*channel_index);
                    let legend_name = if show_dataset_legend {
                        format!(
                            "{}: {channel_name} min/max",
                            primary_legend_prefix.as_deref().unwrap_or("")
                        )
                    } else {
                        format!("{channel_name} min/max")
                    };
                    let line_color =
                        self.plot_channel_color(*channel_index, 0, pane_index, pane_count);
                    plot_ui.line(
                        Line::new(PlotPoints::from(envelope))
                            .name(legend_name)
                            .color(line_color)
                            .style(self.channel_line_pattern(*channel_index).plot_style())
                            .width(self.visible_line_width(*channel_index)),
                    );
                }
            }

            for (dataset_index, dataset) in self.imported_datasets.iter().enumerate() {
                let compare_selected = self.selected_imported_channels(dataset_index);
                let dataset_label = dataset.display_name.clone();
                let dataset_legend_prefix = if show_dataset_legend {
                    Some(format!("数据{}", Self::dataset_letter(dataset_index + 1)))
                } else {
                    None
                };
                let dataset_line_style = dataset.line_pattern.plot_style();
                let time_offset = self.dataset_time_offset(dataset_index + 1);
                for (out_index, channel_index) in compare_selected.iter().enumerate() {
                    if !self.channel_in_scope_pane(*channel_index, pane_index, pane_count)
                        || out_index >= dataset.plot_cache.channels.len()
                    {
                        continue;
                    }
                    let raw_points = dataset
                        .plot_cache
                        .times
                        .iter()
                        .zip(dataset.plot_cache.channels[out_index].iter())
                        .map(|(time, value)| {
                            [
                                *time + time_offset,
                                self.scaled_value(*channel_index, *value),
                            ]
                        })
                        .collect::<Vec<_>>();
                    let channel_name = self.channel_name(*channel_index);
                    let legend_name = if show_dataset_legend {
                        format!(
                            "{}: {channel_name}",
                            dataset_legend_prefix.as_deref().unwrap_or("")
                        )
                    } else {
                        format!("{dataset_label}: {channel_name}")
                    };
                    let line_color = self.plot_channel_color(
                        *channel_index,
                        dataset_index + 1,
                        pane_index,
                        pane_count,
                    );
                    plot_ui.line(
                        Line::new(PlotPoints::from(raw_points))
                            .name(legend_name)
                            .color(line_color)
                            .style(dataset_line_style)
                            .width(self.compare_line_width(*channel_index)),
                    );
                }

                if let Some(summary) = &dataset.plot_summary {
                    for (out_index, channel_index) in compare_selected.iter().enumerate() {
                        if !self.channel_in_scope_pane(*channel_index, pane_index, pane_count)
                            || out_index >= summary.min.len()
                            || out_index >= summary.max.len()
                        {
                            continue;
                        }
                        let mut envelope = Vec::with_capacity(summary.bin_start.len() * 2);
                        for i in 0..summary.bin_start.len() {
                            let mid =
                                (summary.bin_start[i] + summary.bin_end[i]) * 0.5 + time_offset;
                            let (scaled_min, scaled_max) = self.scaled_min_max(
                                *channel_index,
                                summary.min[out_index][i],
                                summary.max[out_index][i],
                            );
                            envelope.push([mid, scaled_min]);
                            envelope.push([mid, scaled_max]);
                        }
                        let channel_name = self.channel_name(*channel_index);
                        let legend_name = if show_dataset_legend {
                            format!(
                                "{}: {channel_name} min/max",
                                dataset_legend_prefix.as_deref().unwrap_or("")
                            )
                        } else {
                            format!("{dataset_label}: {channel_name} min/max")
                        };
                        let line_color = self.plot_channel_color(
                            *channel_index,
                            dataset_index + 1,
                            pane_index,
                            pane_count,
                        );
                        plot_ui.line(
                            Line::new(PlotPoints::from(envelope))
                                .name(legend_name)
                                .color(line_color)
                                .style(dataset_line_style)
                                .width(self.compare_line_width(*channel_index)),
                        );
                    }
                }
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

            if let (Some(cursor), Some(pointer)) =
                (self.cursor_place_mode, plot_ui.pointer_coordinate())
            {
                plot_ui.vline(
                    VLine::new(pointer.x)
                        .color(Self::cursor_color(cursor))
                        .style(LineStyle::Dashed { length: 6.0 })
                        .width(2.5),
                );
            }
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
            if ui
                .button(self.tr("放置光标 X1", "Place Cursor X1"))
                .clicked()
            {
                self.cursor_place_mode = Some(CursorId::A);
                self.zoom_box_start = None;
                self.zoom_box_current = None;
                ui.close_menu();
            }
            if ui
                .button(self.tr("放置光标 X2", "Place Cursor X2"))
                .clicked()
            {
                self.cursor_place_mode = Some(CursorId::B);
                self.zoom_box_start = None;
                self.zoom_box_current = None;
                ui.close_menu();
            }
            if self.cursor_place_mode.is_some()
                && ui.button(self.tr("取消放置", "Cancel Placement")).clicked()
            {
                self.cursor_place_mode = None;
                ui.close_menu();
            }
            ui.separator();
            if self.show_cursor_a {
                if ui
                    .button(self.tr("隐藏光标 X1", "Hide Cursor X1"))
                    .clicked()
                {
                    self.show_cursor_a = false;
                    ui.close_menu();
                }
            } else if ui
                .button(self.tr("显示光标 X1", "Show Cursor X1"))
                .clicked()
            {
                self.show_cursor_a = true;
                ui.close_menu();
            }
            if self.show_cursor_b {
                if ui
                    .button(self.tr("隐藏光标 X2", "Hide Cursor X2"))
                    .clicked()
                {
                    self.show_cursor_b = false;
                    ui.close_menu();
                }
            } else if ui
                .button(self.tr("显示光标 X2", "Show Cursor X2"))
                .clicked()
            {
                self.show_cursor_b = true;
                ui.close_menu();
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
                if ctrl_down {
                    self.zoom(center_x, factor);
                } else {
                    let center_y = response
                        .response
                        .hover_pos()
                        .map(|pos| response.transform.value_from_position(pos).y)
                        .unwrap_or((plot_y_min + plot_y_max) * 0.5);
                    self.zoom_y_with_bounds(center_y, factor, plot_y_min, plot_y_max);
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
        self.sync_channel_state_lengths();
        self.apply_theme(ctx);
        self.handle_shortcuts(ctx);

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| self.top_bar(ui));
        self.help_window(ctx);
        self.options_window(ctx);

        egui::SidePanel::left("channels")
            .resizable(true)
            .default_width(CHANNEL_PANEL_DEFAULT_WIDTH)
            .width_range(180.0..=CHANNEL_PANEL_MAX_WIDTH)
            .show(ctx, |ui| self.channel_panel(ui));

        egui::SidePanel::right("analysis")
            .resizable(true)
            .default_width(330.0)
            .show(ctx, |ui| {
                self.measurements_panel(ui);
                ui.separator();
                self.fft_panel(ui);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(error) = &self.last_error {
                ui.label(RichText::new(error).color(Color32::LIGHT_RED));
                ui.separator();
            }
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
            self.last_error = Some(if self.language == Language::Zh {
                "界面内部错误已拦截，软件已继续运行。请保存数据并重启软件。".to_owned()
            } else {
                "An internal UI error was caught. The app is still running; please save your work and restart.".to_owned()
            });
            self.zoom_box_start = None;
            self.zoom_box_current = None;
            self.cursor_place_mode = None;
            ctx.request_repaint();
        }
    }
}
