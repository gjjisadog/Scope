use std::{
    env,
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use eframe::egui::{self, Color32, PointerButton, RichText, Stroke};
use egui_plot::{Line, LineStyle, Plot, PlotBounds, PlotPoint, PlotPoints, Text, VLine};
use serde::{Deserialize, Serialize};

use crate::{
    data::{CloudCsvDataSource, CsvDataSource, DataSource, DatasetMeta, RangeSummary, SampleBlock},
    fft::{self, FftResult, SequenceResult},
};

const MAX_DRAW_POINTS_PER_CHANNEL: usize = 20_000;
const MAX_TOTAL_DRAW_POINTS: usize = 120_000;
const MIN_DRAW_POINTS_PER_CHANNEL: usize = 256;
const MAX_FFT_POINTS: usize = 262_144;
const MAX_AUTO_MEASURE_POINTS: usize = 131_072;
const ZOOM_BOX_MIN_PIXELS: f32 = 8.0;
const CONFIG_VERSION: u32 = 1;
const DEFAULT_WHEEL_ZOOM_SENSITIVITY: f64 = 0.125;
const MIN_WHEEL_ZOOM_SENSITIVITY: f64 = 0.025;
const MAX_WHEEL_ZOOM_SENSITIVITY: f64 = 0.40;
const DEFAULT_CHANNEL_LINE_WIDTH: f32 = 1.4;
const MIN_CHANNEL_LINE_WIDTH: f32 = 0.5;
const MAX_CHANNEL_LINE_WIDTH: f32 = 8.0;
const DEFAULT_CHANNEL_SCALE: f32 = 1.0;
const MIN_CHANNEL_SCALE: f32 = -1_000_000.0;
const MAX_CHANNEL_SCALE: f32 = 1_000_000.0;
const MAX_RECENT_FILES: usize = 12;

fn default_sample_rate_hz() -> f64 {
    1000.0
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AppConfig {
    version: u32,
    display_names: Vec<String>,
    visible: Vec<bool>,
    #[serde(default)]
    channel_colors: Vec<[u8; 4]>,
    #[serde(default)]
    line_widths: Vec<f32>,
    #[serde(default)]
    channel_scales: Vec<f32>,
    fft_channel: usize,
    #[serde(default = "default_wheel_zoom_sensitivity")]
    wheel_zoom_sensitivity: f64,
    #[serde(default = "default_sample_rate_hz")]
    sample_rate_hz: f64,
    #[serde(default = "default_language")]
    language: Language,
    #[serde(default = "default_theme_mode")]
    theme_mode: ThemeMode,
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
    peak_to_peak: f32,
    mean: f32,
    rms: f32,
    frequency_hz: Option<f64>,
}

#[derive(Clone, Debug)]
struct MeasurementCache {
    start: f64,
    end: f64,
    channels: Vec<usize>,
    rows: Vec<(usize, AutoMeasurement)>,
}

pub struct ScopeApp {
    source: Option<Box<dyn DataSource>>,
    source_kind: Option<SourceKind>,
    compare_source: Option<Box<dyn DataSource>>,
    compare_source_kind: Option<SourceKind>,
    visible: Vec<bool>,
    display_names: Vec<String>,
    channel_colors: Vec<Color32>,
    line_widths: Vec<f32>,
    channel_scales: Vec<f32>,
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
    language: Language,
    theme_mode: ThemeMode,
    last_error: Option<String>,
    loaded_path: Option<PathBuf>,
    compare_loaded_path: Option<PathBuf>,
    recent_files: Vec<PathBuf>,
    plot_cache: SampleBlock,
    plot_summary: Option<RangeSummary>,
    compare_plot_cache: SampleBlock,
    compare_plot_summary: Option<RangeSummary>,
    fft_result: Option<FftResult>,
    sequence_result: Option<SequenceResult>,
    measurement_cache: Option<MeasurementCache>,
    fft_channel: usize,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChannelGroup {
    ThreePhaseVoltage,
    ThreePhaseCurrent,
    Analog,
    DigitalStatus,
    FaultStatus,
    Other,
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
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let recent_files = Self::load_recent_files();
        Self {
            source: None,
            source_kind: None,
            compare_source: None,
            compare_source_kind: None,
            visible: Vec::new(),
            display_names: Vec::new(),
            channel_colors: Vec::new(),
            line_widths: Vec::new(),
            channel_scales: Vec::new(),
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
            language: default_language(),
            theme_mode: default_theme_mode(),
            last_error: None,
            loaded_path: None,
            compare_loaded_path: None,
            recent_files,
            plot_cache: SampleBlock::default(),
            plot_summary: None,
            compare_plot_cache: SampleBlock::default(),
            compare_plot_summary: None,
            fft_result: None,
            sequence_result: None,
            measurement_cache: None,
            fft_channel: 0,
            needs_fft_reload: false,
            needs_plot_reload: false,
            needs_compare_plot_reload: false,
            cursor_place_mode: None,
            zoom_box_start: None,
            zoom_box_current: None,
        }
    }

    fn meta(&self) -> Option<&DatasetMeta> {
        self.source.as_ref().map(|source| source.metadata())
    }

    fn compare_meta(&self) -> Option<&DatasetMeta> {
        self.compare_source.as_ref().map(|source| source.metadata())
    }

    fn set_source(&mut self, source: Box<dyn DataSource>, path: PathBuf, kind: SourceKind) {
        let meta = source.metadata().clone();
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
        self.channel_colors = meta
            .channels
            .iter()
            .map(|channel| Self::default_channel_color(channel.index))
            .collect();
        self.line_widths = vec![DEFAULT_CHANNEL_LINE_WIDTH; meta.channels.len()];
        self.channel_scales = vec![DEFAULT_CHANNEL_SCALE; meta.channels.len()];
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
        self.fft_channel = 0;
        self.fft_result = None;
        self.sequence_result = None;
        self.measurement_cache = None;
        self.needs_fft_reload = true;
        self.plot_cache = SampleBlock::default();
        self.plot_summary = None;
        self.compare_plot_cache = SampleBlock::default();
        self.compare_plot_summary = None;
        self.loaded_path = Some(path);
        self.source = Some(source);
        self.source_kind = Some(kind);
        self.last_error = None;
        self.needs_plot_reload = true;
        self.needs_compare_plot_reload = true;
        self.cursor_place_mode = None;
    }

    fn set_compare_source(&mut self, source: Box<dyn DataSource>, path: PathBuf, kind: SourceKind) {
        self.compare_plot_cache = SampleBlock::default();
        self.compare_plot_summary = None;
        self.compare_loaded_path = Some(path);
        self.compare_source = Some(source);
        self.compare_source_kind = Some(kind);
        self.last_error = None;
        self.needs_compare_plot_reload = true;
        self.y_min = None;
        self.y_max = None;
    }

    fn clear_compare_source(&mut self) {
        self.compare_source = None;
        self.compare_source_kind = None;
        self.compare_loaded_path = None;
        self.compare_plot_cache = SampleBlock::default();
        self.compare_plot_summary = None;
        self.needs_compare_plot_reload = false;
        self.y_min = None;
        self.y_max = None;
    }

    fn recent_files_path() -> PathBuf {
        env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
            .or_else(|| env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
            .join("scope-recent-files.json")
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

    fn save_recent_files(&self) {
        let recent = RecentFiles {
            paths: self.recent_files.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&recent) {
            let _ = std::fs::write(Self::recent_files_path(), json);
        }
    }

    fn remember_recent_file(&mut self, path: &Path) {
        let normalized = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.recent_files.retain(|existing| existing != &normalized);
        self.recent_files.insert(0, normalized);
        self.recent_files.truncate(MAX_RECENT_FILES);
        self.save_recent_files();
    }

    fn clear_recent_files(&mut self) {
        self.recent_files.clear();
        self.save_recent_files();
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
                self.set_compare_source(Box::new(source), path, SourceKind::Local);
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
                self.set_compare_source(Box::new(source), path, SourceKind::Cloud);
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
            return self.open_auto_csv(path);
        }
        let opened = match Self::looks_like_cloud_csv(&path) {
            Ok(true) => self.open_cloud_compare_csv(path),
            Ok(false) => self.open_standard_compare_csv(path),
            Err(error) => {
                self.last_error = Some(error);
                false
            }
        };
        if opened {
            if let Some(path) = self.compare_loaded_path.clone() {
                self.remember_recent_file(&path);
            }
        }
        opened
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
            channel_scales: self.channel_scales.clone(),
            fft_channel: self.fft_channel,
            wheel_zoom_sensitivity: self.wheel_zoom_sensitivity,
            sample_rate_hz: self.sample_rate_hz,
            language: self.language,
            theme_mode: self.theme_mode,
        }
    }

    fn apply_config(&mut self, config: AppConfig) {
        self.language = config.language;
        self.theme_mode = config.theme_mode;
        let channel_count = self.display_names.len();
        for (index, name) in config.display_names.into_iter().enumerate().take(channel_count) {
            self.display_names[index] = name;
        }
        for (index, visible) in config.visible.into_iter().enumerate().take(self.visible.len()) {
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
        for (index, scale) in config
            .channel_scales
            .into_iter()
            .enumerate()
            .take(self.channel_scales.len())
        {
            self.channel_scales[index] = Self::sanitize_channel_scale(scale);
        }
        if channel_count > 0 {
            self.fft_channel = config.fft_channel.min(channel_count - 1);
        }
        self.wheel_zoom_sensitivity = config
            .wheel_zoom_sensitivity
            .clamp(MIN_WHEEL_ZOOM_SENSITIVITY, MAX_WHEEL_ZOOM_SENSITIVITY);
        self.sample_rate_hz = config.sample_rate_hz.clamp(1.0, 10_000_000.0);
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
        let compare_cloud_path = (self.compare_source_kind == Some(SourceKind::Cloud))
            .then(|| self.compare_loaded_path.clone())
            .flatten();
        if main_cloud_path.is_none() && compare_cloud_path.is_none() {
            self.needs_fft_reload = true;
            return;
        }
        let config = self.current_config();
        if let Some(path) = main_cloud_path {
            match CloudCsvDataSource::open_with_sample_rate(&path, self.sample_rate_hz) {
                Ok(source) => {
                    self.set_source(Box::new(source), path, SourceKind::Cloud);
                    self.apply_config(config);
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
        if let Some(path) = compare_cloud_path {
            match CloudCsvDataSource::open_with_sample_rate(&path, self.sample_rate_hz) {
                Ok(source) => self.set_compare_source(Box::new(source), path, SourceKind::Cloud),
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
    }

    fn export_config(&mut self) {
        if self.display_names.is_empty() {
            self.last_error = Some(
                self.tr(
                    "请先打开波形 CSV，再导出配置。",
                    "Open a waveform CSV before exporting config.",
                )
                .to_owned(),
            );
            return;
        }
        let filter_name = self.tr("Scope 配置", "Scope config");
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
                        Language::Zh => format!("导出配置失败: {error}"),
                        Language::En => format!("Failed to export config: {error}"),
                    });
                }
            }
            Err(error) => {
                self.last_error = Some(match self.language {
                    Language::Zh => format!("序列化配置失败: {error}"),
                    Language::En => format!("Failed to serialize config: {error}"),
                });
            }
        }
    }

    fn import_config(&mut self) {
        if self.display_names.is_empty() {
            self.last_error = Some(
                self.tr(
                    "请先打开波形 CSV，再导入配置。",
                    "Open a waveform CSV before importing config.",
                )
                .to_owned(),
            );
            return;
        }
        let filter_name = self.tr("Scope 配置", "Scope config");
        let Some(path) = rfd::FileDialog::new()
            .add_filter(filter_name, &["json"])
            .pick_file()
        else {
            return;
        };
        let old_sample_rate = self.sample_rate_hz;
        match std::fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|text| serde_json::from_str::<AppConfig>(&text).map_err(|error| error.to_string()))
        {
            Ok(config) => {
                self.apply_config(config);
                if self.source_kind == Some(SourceKind::Cloud)
                    && (self.sample_rate_hz - old_sample_rate).abs() > f64::EPSILON
                {
                    self.reload_cloud_with_current_sample_rate();
                }
            }
            Err(error) => {
                self.last_error = Some(match self.language {
                    Language::Zh => format!("导入配置失败: {error}"),
                    Language::En => format!("Failed to import config: {error}"),
                });
            }
        }
    }

    fn selected_channels(&self) -> Vec<usize> {
        self.visible
            .iter()
            .enumerate()
            .filter_map(|(index, visible)| visible.then_some(index))
            .collect()
    }

    fn selected_compare_channels(&self) -> Vec<usize> {
        let Some(compare_meta) = self.compare_meta() else {
            return Vec::new();
        };
        let compare_count = compare_meta.channels.len();
        self.selected_channels()
            .into_iter()
            .filter(|channel| *channel < compare_count)
            .collect()
    }

    fn channel_name(&self, index: usize) -> String {
        self.display_names
            .get(index)
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .or_else(|| {
                self.meta()
                    .and_then(|meta| meta.channels.get(index))
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
        let estimated_points =
            ((self.view_end - self.view_start) * source.metadata().nominal_sample_rate_hz)
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
        let Some(source) = &self.compare_source else {
            self.compare_plot_cache = SampleBlock::default();
            self.compare_plot_summary = None;
            self.needs_compare_plot_reload = false;
            return;
        };
        let channels = self.selected_compare_channels();
        if channels.is_empty() {
            self.compare_plot_cache = SampleBlock::default();
            self.compare_plot_summary = None;
            self.needs_compare_plot_reload = false;
            return;
        }
        let max_points = Self::draw_points_per_channel(channels.len());
        let summary_bins = Self::summary_bins_for_channels(channels.len());
        let estimated_points =
            ((self.view_end - self.view_start) * source.metadata().nominal_sample_rate_hz)
                .max(0.0) as usize;
        if estimated_points > max_points * 2 {
            match source.summarize_range(self.view_start, self.view_end, &channels, summary_bins) {
                Ok(summary) => {
                    self.compare_plot_cache = SampleBlock::default();
                    self.compare_plot_summary = Some(summary);
                    self.needs_compare_plot_reload = false;
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        } else {
            match source.read_range(self.view_start, self.view_end, &channels, max_points) {
                Ok(block) => {
                    self.compare_plot_cache = block;
                    self.compare_plot_summary = None;
                    self.needs_compare_plot_reload = false;
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
    }

    fn visible_time_span(&self) -> f64 {
        (self.view_end - self.view_start).max(f64::EPSILON)
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

    fn zoom_y(&mut self, center: f64, factor: f64) {
        let (current_min, current_max) = self.current_y_bounds();
        let old_span = (current_max - current_min).abs().max(f64::EPSILON);
        let new_span = (old_span * factor).max(f64::EPSILON);
        let ratio = ((center - current_min) / old_span).clamp(0.0, 1.0);
        self.y_min = Some(center - ratio * new_span);
        self.y_max = Some(center + (1.0 - ratio) * new_span);
    }

    fn current_y_bounds(&self) -> (f64, f64) {
        if let (Some(min), Some(max)) = (self.y_min, self.y_max) {
            if max > min {
                return (min, max);
            }
        }

        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let selected = self.selected_channels();
        if let Some(summary) = &self.plot_summary {
            for (out_index, channel_index) in selected.iter().enumerate() {
                if out_index >= summary.min.len() || out_index >= summary.max.len() {
                    continue;
                }
                for i in 0..summary.min[out_index].len().min(summary.max[out_index].len()) {
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
        let compare_selected = self.selected_compare_channels();
        if let Some(summary) = &self.compare_plot_summary {
            for (out_index, channel_index) in compare_selected.iter().enumerate() {
                if out_index >= summary.min.len() || out_index >= summary.max.len() {
                    continue;
                }
                for i in 0..summary.min[out_index].len().min(summary.max[out_index].len()) {
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
            let Some(values) = self.compare_plot_cache.channels.get(out_index) else {
                continue;
            };
            for value in values {
                let value = self.scaled_value(*channel_index, *value);
                min = min.min(value);
                max = max.max(value);
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
            CursorId::A => "A",
            CursorId::B => "B",
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
        let Some(meta) = self.meta().cloned() else {
            return;
        };
        let channel_count = meta.channels.len();
        if channel_count == 0 {
            return;
        }
        let fft_channel = self.fft_channel.min(channel_count - 1);
        self.fft_channel = fft_channel;
        let start = self.cursor_a.min(self.cursor_b);
        let end = self.cursor_a.max(self.cursor_b);
        let channel_name = self.channel_name(fft_channel);
        let sample_rate_hz = self.sample_rate_hz.max(1.0);
        let sequence_group = self.sequence_group_for_channel(fft_channel);

        let Some(source) = &self.source else {
            return;
        };
        let mut next_fft = None;
        let mut next_sequence = None;
        let mut next_error = None;

        match source.read_range(start, end, &[fft_channel], MAX_FFT_POINTS) {
            Ok(block) => {
                next_fft = block
                    .channels
                    .first()
                    .map(|samples| self.scaled_samples(fft_channel, samples))
                    .and_then(|samples| fft::analyze(channel_name, &samples, sample_rate_hz, 10));

                if let Some((group_name, group_channels)) = sequence_group {
                    if let Ok(group_block) =
                        source.read_range(start, end, &group_channels, MAX_FFT_POINTS)
                    {
                        if group_block.channels.len() == 3 {
                            let phase_a =
                                self.scaled_samples(group_channels[0], &group_block.channels[0]);
                            let phase_b =
                                self.scaled_samples(group_channels[1], &group_block.channels[1]);
                            let phase_c =
                                self.scaled_samples(group_channels[2], &group_block.channels[2]);
                            next_sequence = fft::analyze_sequence(
                                group_name,
                                &phase_a,
                                &phase_b,
                                &phase_c,
                                sample_rate_hz,
                            );
                        }
                    }
                }

                if next_fft.is_none() {
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

        self.fft_result = next_fft;
        self.sequence_result = next_sequence;
        self.needs_fft_reload = false;
        if let Some(error) = next_error {
            self.last_error = Some(error);
        } else if self
            .last_error
            .as_deref()
            .is_some_and(|error| {
                error.starts_with("FFT needs") || error.starts_with("FFT 需要")
            })
        {
            self.last_error = None;
        }
    }

    fn sequence_group_for_channel(&self, channel: usize) -> Option<(String, [usize; 3])> {
        let meta = self.meta()?;
        let groups = [
            (
                self.tr("电网电压", "Grid Voltage"),
                ["stVg_0.iA", "stVg_0.iB", "stVg_0.iC"],
            ),
            (
                self.tr("电网电流", "Grid Current"),
                ["stIg_0.iA", "stIg_0.iB", "stIg_0.iC"],
            ),
            (
                self.tr("逆变电压", "Inverter Voltage"),
                ["stVinv_0.iA", "stVinv_0.iB", "stVinv_0.iC"],
            ),
        ];

        for (label, names) in groups {
            let mut indexes = [usize::MAX; 3];
            let mut complete = true;
            for (slot, name) in names.iter().enumerate() {
                if let Some(index) = meta
                    .channels
                    .iter()
                    .position(|candidate| candidate.name == *name)
                {
                    indexes[slot] = index;
                } else {
                    complete = false;
                    break;
                }
            }
            if complete && indexes.contains(&channel) {
                return Some((label.to_owned(), indexes));
            }
        }
        None
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

    fn channel_line_width(&self, index: usize) -> f32 {
        self.line_widths
            .get(index)
            .copied()
            .unwrap_or(DEFAULT_CHANNEL_LINE_WIDTH)
            .clamp(MIN_CHANNEL_LINE_WIDTH, MAX_CHANNEL_LINE_WIDTH)
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

    fn channel_group(&self, index: usize, source_name: &str, display_name: &str) -> ChannelGroup {
        let source = source_name.to_ascii_lowercase();
        let display = display_name.to_ascii_lowercase();
        let name = format!("{source} {display}");

        if name.contains("fault")
            || name.contains("ocp")
            || name.contains("vbusov")
            || name.contains("ovboost")
        {
            ChannelGroup::FaultStatus
        } else if name.starts_with("stvg_0.") || name.starts_with("stvinv_0.")
        {
            ChannelGroup::ThreePhaseVoltage
        } else if name.starts_with("stig_0.")
            && (name.ends_with(".ia") || name.ends_with(".ib") || name.ends_with(".ic"))
        {
            ChannelGroup::ThreePhaseCurrent
        } else if index < 30
            || name.starts_with("stv")
            || name.starts_with("sti")
            || name.contains("vbus")
            || name.contains("boost")
            || name.contains("battery")
            || name.contains("ref")
        {
            ChannelGroup::Analog
        } else if name.contains("logic")
            || name.contains("relay")
            || name.contains("flag")
            || name.contains("ready")
            || name.contains("ok")
        {
            ChannelGroup::DigitalStatus
        } else {
            ChannelGroup::Other
        }
    }

    fn channel_group_label(&self, group: ChannelGroup) -> &'static str {
        match group {
            ChannelGroup::ThreePhaseVoltage => self.tr("三相电压", "Three-phase Voltage"),
            ChannelGroup::ThreePhaseCurrent => self.tr("三相电流", "Three-phase Current"),
            ChannelGroup::Analog => self.tr("模拟量", "Analog"),
            ChannelGroup::DigitalStatus => self.tr("数字量", "Digital"),
            ChannelGroup::FaultStatus => self.tr("故障状态", "Fault Status"),
            ChannelGroup::Other => self.tr("其他", "Other"),
        }
    }

    fn set_all_channels_visible(&mut self, visible: bool) {
        if self.visible.iter().any(|current| *current != visible) {
            self.visible.fill(visible);
            self.needs_plot_reload = true;
            self.needs_compare_plot_reload = true;
            self.measurement_cache = None;
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
                    input.key_pressed(egui::Key::R),
                    input.key_pressed(egui::Key::F),
                    input.key_pressed(egui::Key::H),
                    input.modifiers.ctrl && input.key_pressed(egui::Key::A),
                    input.modifiers.ctrl && input.key_pressed(egui::Key::D),
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

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button(self.tr("打开 A", "Open A")).clicked() {
                let filter_name = self.tr("波形 CSV", "Waveform CSV");
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(filter_name, &["csv"])
                    .pick_file()
                {
                    self.open_auto_csv(path);
                }
            }
            if ui.button(self.tr("打开 B 对比", "Open B Compare")).clicked() {
                let filter_name = self.tr("波形 CSV", "Waveform CSV");
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(filter_name, &["csv"])
                    .pick_file()
                {
                    self.open_auto_compare_csv(path);
                }
            }
            if self.compare_source.is_some()
                && ui.button(self.tr("清除 B", "Clear B")).clicked()
            {
                self.clear_compare_source();
            }
            let recent_title = self.tr("最近文件", "Recent Files");
            ui.menu_button(recent_title, |ui| {
                if self.recent_files.is_empty() {
                    ui.label(self.tr("暂无最近文件", "No recent files"));
                    return;
                }

                let recent_files = self.recent_files.clone();
                for path in recent_files {
                    ui.horizontal(|ui| {
                        let exists = path.exists();
                        if ui
                            .button("A")
                            .on_hover_text(self.tr("打开为 A", "Open as A"))
                            .clicked()
                        {
                            self.open_auto_csv(path.clone());
                            ui.close_menu();
                        }
                        if ui
                            .button("B")
                            .on_hover_text(self.tr("打开为 B", "Open as B"))
                            .clicked()
                        {
                            self.open_auto_compare_csv(path.clone());
                            ui.close_menu();
                        }
                        let label = Self::recent_file_label(&path);
                        if exists {
                            ui.label(label);
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
                    });
                }
                ui.separator();
                if ui.button(self.tr("清空最近文件", "Clear Recent Files")).clicked() {
                    self.clear_recent_files();
                    ui.close_menu();
                }
            });
            if ui.button(self.tr("重置视图", "Reset View")).clicked() {
                self.reset_view();
            }
            if ui.button(self.tr("适配光标", "Fit Cursors")).clicked() {
                self.fit_to_cursors();
            }
            if ui.button(self.tr("导入配置", "Import Config")).clicked() {
                self.import_config();
            }
            if ui.button(self.tr("导出配置", "Export Config")).clicked() {
                self.export_config();
            }
            if ui.button(self.tr("帮助", "Help")).clicked() {
                self.show_help = true;
            }
            if ui.button(self.tr("选项", "Options")).clicked() {
                self.show_options = true;
            }
            ui.separator();
            if let Some(meta) = self.meta() {
                if self.language == Language::Zh {
                    let compare_status = self
                        .compare_meta()
                        .map(|compare| format!(" | B: {} 点", compare.sample_count))
                        .unwrap_or_default();
                    ui.label(format!(
                        "A: {} | {} 点 | {:.3}s | 数据 {:.1} Hz | FFT Fs {:.1} Hz{}",
                        meta.source_name,
                        meta.sample_count,
                        meta.duration(),
                        meta.nominal_sample_rate_hz,
                        self.sample_rate_hz,
                        compare_status
                    ));
                } else {
                    let compare_status = self
                        .compare_meta()
                        .map(|compare| format!(" | B: {} samples", compare.sample_count))
                        .unwrap_or_default();
                    ui.label(format!(
                        "A: {} | {} samples | {:.3}s | data {:.1} Hz | FFT Fs {:.1} Hz{}",
                        meta.source_name,
                        meta.sample_count,
                        meta.duration(),
                        meta.nominal_sample_rate_hz,
                        self.sample_rate_hz,
                        compare_status
                    ));
                }
            } else {
                ui.label(self.tr(
                    "打开 A 文件开始分析；需要对比时再打开 B。软件会自动识别云端 Content 或本地数值 CSV。",
                    "Open A to begin; open B when comparison is needed. Content files are detected automatically.",
                ));
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
                        ui.label("Windows 离线波形分析工具，支持通道勾选、示波器式缩放、双光标测量、FFT、THD 和三相序分量分析。");

                        ui.separator();
                        ui.heading("支持的 CSV 格式");
                        ui.label("使用顶部“打开 A”载入主数据；使用“打开 B 对比”载入第二组数据。软件读取第一行表头后，会自动选择云端 Content 解析器或本地数值 CSV 解析器。");
                        ui.label("A 是主数据源，通道列表、变量名、颜色、线宽和 FFT 通道以 A 为准；B 作为对比数据，按相同通道序号叠加显示。");
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
                        ui.label("打开 A：载入主波形文件。打开 B 对比：载入第二组数据并以虚线叠加。清除 B：移除对比数据。");
                        ui.label("最近文件：打开成功的 A/B CSV 会自动加入列表，可从顶部“最近文件”菜单选择作为 A 或 B 重新载入，也可以清空列表。列表保存为程序目录下的 scope-recent-files.json。");
                        ui.label("选项：设置 FFT Fs，默认 1000 Hz。云端 Content CSV 同时用它生成秒级时间轴；FFT 频率轴明确使用该设置值。");
                        ui.label("鼠标滚轮：以鼠标位置为中心缩放纵轴幅值范围。");
                        ui.label("Ctrl + 鼠标滚轮/触控板滚动：以鼠标位置为中心缩放横轴时间范围；未按 Ctrl 时始终缩放纵轴。");
                        ui.label("选项：可调整滚轮缩放敏感度，也可切换中文/英文界面和浅色/深色主题。");
                        ui.label("左侧变量栏：按三相电压、三相电流、模拟量、数字量、故障状态和其他自动分组；每组可单独全选/全不选，也可编辑显示名、设置颜色、线宽和倍率系数。搜索支持多个关键词，并会匹配显示名、原始名和分组名。载入 B 后，A 用实线显示，B 用同一通道颜色、线宽和倍率按相同通道序号叠加。");
                        ui.label("鼠标悬停左侧变量：对应波形会加粗高亮。");
                        ui.label("导入/导出配置：保存和恢复变量名、通道显示、通道颜色、线宽、倍率系数、FFT 通道、FFT Fs、缩放敏感度、界面语言和主题。");
                        ui.label("左键单击波形：移动距离最近的光标。");
                        ui.label("左键拖拽波形：框选时间区域并放大。");
                        ui.label("右键单击波形：打开光标菜单。");
                        ui.label("放置光标 A/B：显示红色虚线预览光标，左键确认，Esc 取消。");
                        ui.label("隐藏/显示光标 A/B：只切换显示状态，不改变光标位置和测量结果。");
                        ui.label("右键拖拽波形：平移当前视图。");
                        ui.label("适配光标：缩放到光标 A/B 的时间范围。");
                        ui.label("快捷键：R 复位视图，F 适配光标，H 隐藏/显示 A/B 光标，Ctrl+A 全选通道，Ctrl+D 取消全选。");
                        ui.label("自动测量：右侧光标面板会对 A/B 区间内的已选通道显示 yA/yB/dy、峰峰值、RMS、平均值、最大/最小值和频率估算；这些数值使用倍率后的通道值。");
                        ui.label("频率估算使用均值上升穿越点计算周期，适合周期波形；噪声大、直流量或非周期信号会显示 -- 或仅作估算。");

                        ui.separator();
                        ui.heading("FFT、THD 和序分量");
                        ui.label("FFT 面板会自动分析光标 A/B 之间选中 FFT 通道的波形，使用倍率后的通道值。");
                        ui.label("计算前会去除直流均值并使用 Hann 窗，FFT 点数取当前选区样本数的 next power of two，最多读取 262144 点。");
                        ui.label("基波默认取正频率频谱中幅值最大的频点；谐波表显示 1-10 次谐波的频率、幅值、相位、dBc 和 THD。");
                        ui.label("THD = 2 次及以上谐波平方和开根号 / 基波幅值。若选区太短或基波不明显，结果需要结合波形判断。");
                        ui.label("当 FFT 通道属于 stVg_0.iA/iB/iC、stIg_0.iA/iB/iC 或 stVinv_0.iA/iB/iC 时，软件同时显示零序、正序和负序分量。");
                        ui.label("单通道 FFT 相位会随光标起点变化；序分量按 A-B-C 正序约定计算，重点看相对相位和正/负/零序幅值比例。");

                        ui.separator();
                        ui.heading("构建 / 打包");
                        ui.label("本地运行需要 Rust 工具链；最终交付目标是 Windows 10/11 x64 便携版 zip，不要求用户安装 Python 或额外算法包。");
                        ui.label("本地调试运行：");
                        ui.monospace("cargo run --release");
                        ui.label("在 Windows 机器创建便携包：");
                        ui.monospace("powershell -ExecutionPolicy Bypass -File scripts/package-windows.ps1");
                        ui.label("输出：dist/ScopeAnalyzer-0.1.0-win-x64.zip");
                        ui.label("Rust crate 会编译进程序；zip 内包含可执行文件、README 和示例/辅助脚本。后续可在此基础上再做安装包。");
                    } else {
                        ui.heading("Scope Analyzer");
                        ui.label("Windows offline waveform analyzer with channel selection, oscilloscope-style zooming, cursor measurement, FFT, THD, and sequence components.");

                        ui.separator();
                        ui.heading("Supported CSV Formats");
                        ui.label("Use Open A for the main dataset and Open B Compare for the second dataset. The software reads the first CSV header and automatically chooses the cloud Content parser or the local numeric CSV parser.");
                        ui.label("A is the primary dataset. Channel list, display names, colors, line widths, and FFT channel follow A. B is overlaid by matching channel index.");
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
                        ui.label("Open A loads the main waveform. Open B Compare loads a second waveform as dashed overlays. Clear B removes the comparison dataset.");
                        ui.label("Recent Files: successfully opened A/B CSV files are added automatically. Use the top Recent Files menu to reopen an item as A or B, or clear the list. The list is stored as scope-recent-files.json next to the executable.");
                        ui.label("Options: set FFT Fs. Default is 1000 Hz. Cloud Content CSV also uses it to convert sample index to seconds; the FFT frequency axis explicitly uses this setting.");
                        ui.label("Mouse wheel: zoom vertical amplitude range around the pointer.");
                        ui.label("Ctrl + mouse wheel / touchpad scroll: zoom horizontal time range around the pointer; without Ctrl, pointer zoom always changes the vertical axis.");
                        ui.label("Options: adjust mouse wheel zoom sensitivity and choose Chinese/English UI language plus light/dark theme.");
                        ui.label("Left channel list: channels are grouped automatically as three-phase voltage, three-phase current, analog, digital, fault status, and other. Each group has its own All/None controls, plus display-name editing, color, line width, and scale factor. Search supports multiple keywords and matches display name, original name, and group name. After B is loaded, A is solid and B is dashed with the same channel style, scale, and matching channel index.");
                        ui.label("Hover a variable in the left list: the corresponding waveform becomes thicker.");
                        ui.label("Import/Export Config: save and restore display names, channel visibility, channel colors, line widths, scale factors, FFT channel, FFT Fs, wheel zoom sensitivity, UI language, and theme.");
                        ui.label("Left click plot: move the nearest cursor to the clicked position.");
                        ui.label("Left drag plot: box-select a time range and zoom in.");
                        ui.label("Right click plot: open cursor menu.");
                        ui.label("Place Cursor A/B: shows a red dashed preview cursor; left click confirms, Esc cancels.");
                        ui.label("Hide/Show Cursor A/B: toggles cursor visibility without changing cursor position or measurements.");
                        ui.label("Right drag plot: pan the current view.");
                        ui.label("Fit Cursors: zoom to the time range between cursor A and cursor B.");
                        ui.label("Shortcuts: R resets view, F fits cursors, H hides/shows A/B cursors, Ctrl+A selects all channels, Ctrl+D deselects all channels.");
                        ui.label("Auto measurements: the right cursor panel shows yA/yB/dy, peak-to-peak, RMS, average, min/max, and estimated frequency for selected channels in the A-B range. Values use the channel scale factor.");
                        ui.label("Frequency estimation uses rising crossings through the mean value. It works best for periodic waveforms; noisy, DC, or non-periodic signals may show -- or only an estimate.");

                        ui.separator();
                        ui.heading("FFT, THD, and Sequence");
                        ui.label("The FFT panel automatically analyzes the selected FFT channel between cursor A and cursor B using the scaled channel values.");
                        ui.label("Before FFT, the DC mean is removed and a Hann window is applied. FFT length is the next power of two for the selected samples, with up to 262144 points read.");
                        ui.label("The fundamental defaults to the strongest positive-frequency bin. The harmonic table shows 1st-10th harmonic frequency, amplitude, phase, dBc, and THD.");
                        ui.label("THD is sqrt(sum of harmonic powers from the 2nd harmonic upward) divided by the fundamental amplitude. Short selections or unclear fundamentals should be interpreted with the waveform.");
                        ui.label("If the FFT channel belongs to stVg_0.iA/iB/iC, stIg_0.iA/iB/iC, or stVinv_0.iA/iB/iC, the software also shows zero, positive, and negative sequence components.");
                        ui.label("Single-channel FFT phase depends on cursor start time. Sequence analysis uses the A-B-C positive-sequence convention; focus on relative phase and positive/negative/zero sequence magnitude ratios.");

                        ui.separator();
                        ui.heading("Build / Packaging");
                        ui.label("Local development requires the Rust toolchain. The delivery target is a Windows 10/11 x64 portable zip without Python or external algorithm package installs.");
                        ui.label("Run locally with:");
                        ui.monospace("cargo run --release");
                        ui.label("Create a Windows portable package on Windows with:");
                        ui.monospace("powershell -ExecutionPolicy Bypass -File scripts/package-windows.ps1");
                        ui.label("Output: dist/ScopeAnalyzer-0.1.0-win-x64.zip");
                        ui.label("Rust crates are compiled into the executable. The zip includes the executable, README, and sample/helper scripts. An installer can be added later.");
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
                    ui.label(format!("当前: 每格滚轮 {:.0}%", self.wheel_zoom_sensitivity * 100.0));
                } else {
                    ui.label(format!(
                        "Current: {:.0}% per wheel step",
                        self.wheel_zoom_sensitivity * 100.0
                    ));
                }
                if ui.button(self.tr("重置敏感度", "Reset Sensitivity")).clicked() {
                    self.wheel_zoom_sensitivity = DEFAULT_WHEEL_ZOOM_SENSITIVITY;
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
        let mut sum = 0.0_f64;
        let mut sum_squares = 0.0_f64;

        for &sample in samples {
            min = min.min(sample);
            max = max.max(sample);
            let sample = sample as f64;
            sum += sample;
            sum_squares += sample * sample;
        }

        if !min.is_finite() || !max.is_finite() {
            return None;
        }

        let mean = sum / sample_count as f64;
        let rms = (sum_squares / sample_count as f64).sqrt();
        let frequency_hz = Self::estimate_frequency(&times[..sample_count], samples, mean as f32);

        Some(AutoMeasurement {
            first: samples[0],
            last: samples[sample_count - 1],
            min,
            max,
            peak_to_peak: max - min,
            mean: mean as f32,
            rms: rms as f32,
            frequency_hz,
        })
    }

    fn estimate_frequency(times: &[f64], samples: &[f32], threshold: f32) -> Option<f64> {
        if times.len() < 3 || samples.len() < 3 {
            return None;
        }

        let amplitude = samples
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), &sample| {
                (min.min(sample), max.max(sample))
            });
        if amplitude.1 - amplitude.0 <= f32::EPSILON {
            return None;
        }

        let mut crossings = Vec::new();
        for index in 1..times.len().min(samples.len()) {
            let previous = samples[index - 1];
            let current = samples[index];
            if previous < threshold && current >= threshold && current > previous {
                let fraction = ((threshold - previous) / (current - previous)).clamp(0.0, 1.0);
                let time = times[index - 1] + (times[index] - times[index - 1]) * fraction as f64;
                let is_new_crossing = match crossings.last() {
                    Some(last) => (time - *last).abs() > f64::EPSILON,
                    None => true,
                };
                if is_new_crossing {
                    crossings.push(time);
                }
            }
        }

        if crossings.len() < 2 {
            return None;
        }
        let mut period_sum = 0.0_f64;
        let mut period_count = 0_usize;
        for pair in crossings.windows(2) {
            let period = pair[1] - pair[0];
            if period.is_finite() && period > 0.0 {
                period_sum += period;
                period_count += 1;
            }
        }
        if period_count == 0 {
            return None;
        }
        let average_period = period_sum / period_count as f64;
        if average_period > 0.0 {
            Some(1.0 / average_period)
        } else {
            None
        }
    }

    fn channel_row_ui(
        &mut self,
        ui: &mut egui::Ui,
        channel: &crate::data::ChannelMeta,
        display_name: &str,
        source_label: &str,
        width_prefix: &str,
        scale_prefix: &str,
    ) -> bool {
        ui.push_id(("channel_row", channel.index), |ui| {
            let row_response = ui.horizontal(|ui| {
                let mut row_hovered = false;
                let mut color = self.channel_color(channel.index);
                let color_response = egui::color_picker::color_edit_button_srgba(
                    ui,
                    &mut color,
                    egui::color_picker::Alpha::Opaque,
                );
                row_hovered |= color_response.hovered();
                if color_response.changed() {
                    if let Some(stored_color) = self.channel_colors.get_mut(channel.index) {
                        *stored_color = color;
                    }
                }
                if let Some(width) = self.line_widths.get_mut(channel.index) {
                    let width_response = ui.add(
                        egui::DragValue::new(width)
                            .speed(0.1)
                            .clamp_range(MIN_CHANNEL_LINE_WIDTH..=MAX_CHANNEL_LINE_WIDTH)
                            .prefix(width_prefix),
                    );
                    row_hovered |= width_response.hovered() || width_response.has_focus();
                    *width = (*width).clamp(MIN_CHANNEL_LINE_WIDTH, MAX_CHANNEL_LINE_WIDTH);
                }
                if let Some(scale) = self.channel_scales.get_mut(channel.index) {
                    let old_scale = *scale;
                    let scale_response = ui.add(
                        egui::DragValue::new(scale)
                            .speed(0.01)
                            .clamp_range(MIN_CHANNEL_SCALE..=MAX_CHANNEL_SCALE)
                            .prefix(scale_prefix),
                    );
                    row_hovered |= scale_response.hovered() || scale_response.has_focus();
                    *scale = Self::sanitize_channel_scale(*scale);
                    if scale_response.changed() && (*scale - old_scale).abs() > f32::EPSILON {
                        self.y_min = None;
                        self.y_max = None;
                        self.measurement_cache = None;
                        self.fft_result = None;
                        self.sequence_result = None;
                        self.needs_fft_reload = true;
                    }
                }
                let checkbox_response = ui.checkbox(&mut self.visible[channel.index], "");
                row_hovered |= checkbox_response.hovered();
                if checkbox_response.changed() {
                    self.needs_plot_reload = true;
                    self.needs_compare_plot_reload = true;
                    self.measurement_cache = None;
                }
                if let Some(name) = self.display_names.get_mut(channel.index) {
                    let name_response =
                        ui.add(egui::TextEdit::singleline(name).desired_width(150.0));
                    row_hovered |= name_response.hovered() || name_response.has_focus();
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
            row_response.response.hovered() || row_response.inner
        })
        .inner
    }

    fn channel_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading(self.tr("变量", "Channels"));
        ui.horizontal(|ui| {
            if ui.button(self.tr("全选", "All")).clicked() {
                self.set_all_channels_visible(true);
            }
            if ui.button(self.tr("全不选", "None")).clicked() {
                self.set_all_channels_visible(false);
            }
        });
        let filter_hint = self.tr("筛选变量，支持多关键词", "Filter channels, multiple keywords");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.channel_filter)
                    .hint_text(filter_hint)
                    .desired_width(f32::INFINITY),
            );
            if !self.channel_filter.is_empty()
                && ui.button(self.tr("清除", "Clear")).clicked()
            {
                self.channel_filter.clear();
            }
        });
        ui.separator();

        let Some(meta) = self.meta().cloned() else {
            ui.label(self.tr("未加载数据。", "No data loaded."));
            return;
        };
        if self.compare_source.is_some() {
            ui.label(self.tr(
                "A=实线，B=虚线；B 按相同通道序号对比。",
                "A=solid, B=dashed; B follows matching channel indexes.",
            ));
            ui.separator();
        }
        let filter_terms = self
            .channel_filter
            .split_whitespace()
            .map(|term| term.to_lowercase())
            .collect::<Vec<_>>();
        let mut hovered_channel = None;
        let source_label = self.tr("原始", "src");
        let width_prefix = self.tr("线宽 ", "W ");
        let scale_prefix = self.tr("倍率 ", "Scale ");
        let groups = [
            ChannelGroup::ThreePhaseVoltage,
            ChannelGroup::ThreePhaseCurrent,
            ChannelGroup::Analog,
            ChannelGroup::DigitalStatus,
            ChannelGroup::FaultStatus,
            ChannelGroup::Other,
        ];
        let mut any_entries = false;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for group in groups {
                let group_label = self.channel_group_label(group);
                let entries = meta
                    .channels
                    .iter()
                    .filter_map(|channel| {
                        let display_name = self.channel_name(channel.index);
                        if self.channel_group(channel.index, &channel.name, &display_name) != group {
                            return None;
                        }
                        let searchable = format!(
                            "{} {} {}",
                            display_name, channel.name, group_label
                        )
                        .to_lowercase();
                        if !filter_terms
                            .iter()
                            .all(|term| searchable.contains(term))
                        {
                            return None;
                        }
                        Some((channel.clone(), display_name))
                    })
                    .collect::<Vec<_>>();
                if entries.is_empty() {
                    continue;
                }
                any_entries = true;

                let selected_count = entries
                    .iter()
                    .filter(|(channel, _)| self.visible.get(channel.index).copied().unwrap_or(false))
                    .count();
                let title = format!(
                    "{} ({}/{})",
                    group_label,
                    selected_count,
                    entries.len()
                );
                egui::CollapsingHeader::new(title)
                    .default_open(matches!(
                        group,
                        ChannelGroup::ThreePhaseVoltage
                            | ChannelGroup::ThreePhaseCurrent
                            | ChannelGroup::Analog
                    ))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.small_button(self.tr("全选", "All")).clicked() {
                                for (channel, _) in &entries {
                                    if let Some(visible) = self.visible.get_mut(channel.index) {
                                        *visible = true;
                                    }
                                }
                                self.needs_plot_reload = true;
                                self.needs_compare_plot_reload = true;
                                self.measurement_cache = None;
                            }
                            if ui.small_button(self.tr("全不选", "None")).clicked() {
                                for (channel, _) in &entries {
                                    if let Some(visible) = self.visible.get_mut(channel.index) {
                                        *visible = false;
                                    }
                                }
                                self.needs_plot_reload = true;
                                self.needs_compare_plot_reload = true;
                                self.measurement_cache = None;
                            }
                        });
                        for (channel, display_name) in &entries {
                            if self.channel_row_ui(
                                ui,
                                channel,
                                display_name,
                                source_label,
                                width_prefix,
                                scale_prefix,
                            ) {
                                hovered_channel = Some(channel.index);
                            }
                        }
                    });
            }
        });
        if !any_entries {
            ui.label(self.tr("没有匹配的变量。", "No matching channels."));
        }
        self.hovered_channel = hovered_channel;
    }

    fn measurements_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading(self.tr("光标", "Cursors"));
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.active_cursor, CursorId::A, "A");
            ui.radio_value(&mut self.active_cursor, CursorId::B, "B");
        });
        let hidden_label = self.tr("（隐藏）", " (hidden)");
        ui.label(format!(
            "A: {:.9}s{}",
            self.cursor_a,
            if self.show_cursor_a { "" } else { hidden_label }
        ));
        ui.label(format!(
            "B: {:.9}s{}",
            self.cursor_b,
            if self.show_cursor_b { "" } else { hidden_label }
        ));
        let dt = (self.cursor_b - self.cursor_a).abs();
        ui.label(format!("dt: {:.9}s", dt));
        if dt > 0.0 {
            ui.label(format!("1/dt: {:.3} Hz", 1.0 / dt));
        }
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
        ui.separator();

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
        ui.strong(self.tr("自动测量（A-B）", "Auto Measurements (A-B)"));

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
                        if let Some(measurement) = Self::auto_measure(&block.times, &scaled_values) {
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
        for (channel_index, measurement) in &cache.rows {
            let name = self.channel_name(*channel_index);
            let cursor_text = format!(
                "{}  yA={:.5}  yB={:.5}  dy={:.5}",
                name,
                measurement.first,
                measurement.last,
                measurement.last - measurement.first
            );
            let frequency_text = measurement
                .frequency_hz
                .map(|frequency| format!("{frequency:.3} Hz"))
                .unwrap_or_else(|| "--".to_owned());
            let stats_text = if self.language == Language::Zh {
                format!(
                    "峰峰={:.5}  RMS={:.5}  平均={:.5}  最小={:.5}  最大={:.5}  频率={}",
                    measurement.peak_to_peak,
                    measurement.rms,
                    measurement.mean,
                    measurement.min,
                    measurement.max,
                    frequency_text
                )
            } else {
                format!(
                    "pp={:.5}  rms={:.5}  avg={:.5}  min={:.5}  max={:.5}  f={}",
                    measurement.peak_to_peak,
                    measurement.rms,
                    measurement.mean,
                    measurement.min,
                    measurement.max,
                    frequency_text
                )
            };
            if self.hovered_channel == Some(*channel_index) {
                let color = self.channel_color(*channel_index);
                let background = Color32::from_rgba_premultiplied(255, 240, 160, 80);
                ui.label(
                    RichText::new(cursor_text)
                        .strong()
                        .color(color)
                        .background_color(background),
                );
                ui.label(
                    RichText::new(stats_text)
                        .strong()
                        .color(color)
                        .background_color(background),
                );
            } else {
                ui.label(cursor_text);
                ui.label(RichText::new(stats_text).small());
            }
        }
    }

    fn fft_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("FFT");
        let Some(meta) = self.meta().cloned() else {
            ui.label(self.tr("未加载数据。", "No data loaded."));
            return;
        };

        let mut fft_channel_changed = false;
        let channel_label = self.tr("通道", "Channel");
        egui::ComboBox::from_label(channel_label)
            .selected_text(
                meta.channels
                    .get(self.fft_channel)
                    .map(|channel| self.channel_name(channel.index))
                    .unwrap_or_else(|| "CH1".to_owned()),
            )
            .show_ui(ui, |ui| {
                for channel in &meta.channels {
                    if ui
                        .selectable_value(
                            &mut self.fft_channel,
                            channel.index,
                            self.channel_name(channel.index),
                        )
                        .changed()
                    {
                        fft_channel_changed = true;
                    }
                }
            });

        if fft_channel_changed {
            self.needs_fft_reload = true;
        }

        ui.label(self.tr(
            "自动分析光标 A-B 区间；FFT Fs 使用“选项”中的用户设置值。",
            "Auto analyzes the cursor A-B range; FFT Fs uses the user setting in Options.",
        ));
        ui.label(format!("FFT Fs: {:.3} Hz", self.sample_rate_hz.max(1.0)));
        if self.needs_fft_reload {
            self.run_fft();
        }

        if let Some(result) = &self.fft_result {
            ui.separator();
            ui.strong(self.tr("频谱概要", "Spectrum Summary"));
            if self.language == Language::Zh {
                egui::Grid::new("fft_summary_zh").num_columns(2).show(ui, |ui| {
                    ui.label("通道");
                    ui.strong(&result.channel_name);
                    ui.end_row();
                    ui.label("样本数");
                    ui.label(result.sample_count.to_string());
                    ui.end_row();
                    ui.label("FFT Fs");
                    ui.label(format!("{:.3} Hz", result.sample_rate_hz));
                    ui.end_row();
                    ui.label("基波频率");
                    ui.strong(format!("{:.3} Hz", result.fundamental_hz));
                    ui.end_row();
                    ui.label("THD");
                    ui.strong(RichText::new(format!("{:.3}%", result.thd_percent)).color(Color32::LIGHT_RED));
                    ui.end_row();
                });
            } else {
                egui::Grid::new("fft_summary_en").num_columns(2).show(ui, |ui| {
                    ui.label("Channel");
                    ui.strong(&result.channel_name);
                    ui.end_row();
                    ui.label("Samples");
                    ui.label(result.sample_count.to_string());
                    ui.end_row();
                    ui.label("FFT Fs");
                    ui.label(format!("{:.3} Hz", result.sample_rate_hz));
                    ui.end_row();
                    ui.label("Fundamental");
                    ui.strong(format!("{:.3} Hz", result.fundamental_hz));
                    ui.end_row();
                    ui.label("THD");
                    ui.strong(RichText::new(format!("{:.3}%", result.thd_percent)).color(Color32::LIGHT_RED));
                    ui.end_row();
                });
            }
            ui.add_space(6.0);
            ui.strong(self.tr("谐波明细", "Harmonics"));
            egui::Grid::new("harmonics").striped(true).num_columns(5).show(ui, |ui| {
                ui.strong(self.tr("次数", "Order"));
                ui.strong(self.tr("频率 Hz", "Freq Hz"));
                ui.strong(self.tr("幅值", "Amplitude"));
                ui.strong(self.tr("相位 deg", "Phase deg"));
                ui.strong(self.tr("相对 dBc", "Rel dBc"));
                ui.end_row();
                for row in &result.harmonics {
                    let order_text = if self.language == Language::Zh {
                        format!("{}次", row.order)
                    } else {
                        row.order.to_string()
                    };
                    if row.order == 1 {
                        ui.strong(order_text);
                        ui.strong(format!("{:.3}", row.frequency_hz));
                        ui.strong(format!("{:.6}", row.amplitude));
                        ui.strong(format!("{:.2}", row.phase_deg));
                        ui.strong(format!("{:.2}", row.relative_db));
                    } else {
                        ui.label(order_text);
                        ui.label(format!("{:.3}", row.frequency_hz));
                        ui.label(format!("{:.6}", row.amplitude));
                        ui.label(format!("{:.2}", row.phase_deg));
                        ui.label(format!("{:.2}", row.relative_db));
                    }
                    ui.end_row();
                }
            });
        }

        if let Some(sequence) = &self.sequence_result {
            ui.separator();
            ui.heading(self.tr("序分量", "Sequence"));
            if self.language == Language::Zh {
                egui::Grid::new("sequence_summary_zh").num_columns(2).show(ui, |ui| {
                    ui.label("三相组");
                    ui.strong(&sequence.group_name);
                    ui.end_row();
                    ui.label("基波频率");
                    ui.label(format!("{:.3} Hz", sequence.fundamental_hz));
                    ui.end_row();
                    ui.label("A/B/C 相位");
                    ui.label(format!(
                        "{:.2} / {:.2} / {:.2} deg",
                        sequence.phase_a_deg, sequence.phase_b_deg, sequence.phase_c_deg
                    ));
                    ui.end_row();
                });
            } else {
                egui::Grid::new("sequence_summary_en").num_columns(2).show(ui, |ui| {
                    ui.label("Group");
                    ui.strong(&sequence.group_name);
                    ui.end_row();
                    ui.label("Fundamental");
                    ui.label(format!("{:.3} Hz", sequence.fundamental_hz));
                    ui.end_row();
                    ui.label("Phase A/B/C");
                    ui.label(format!(
                        "{:.2} / {:.2} / {:.2} deg",
                        sequence.phase_a_deg, sequence.phase_b_deg, sequence.phase_c_deg
                    ));
                    ui.end_row();
                });
            }
            ui.add_space(6.0);
            egui::Grid::new("sequence_components").striped(true).num_columns(4).show(ui, |ui| {
                ui.strong(self.tr("分量", "Component"));
                ui.strong(self.tr("幅值", "Amplitude"));
                ui.strong(self.tr("相位 deg", "Phase deg"));
                ui.strong(self.tr("占正序 %", "% Positive"));
                ui.end_row();
                for component in [
                    &sequence.zero,
                    &sequence.positive,
                    &sequence.negative,
                ] {
                    let component_name = if self.language == Language::Zh {
                        match component.name {
                            "Zero" => "零序",
                            "Positive" => "正序",
                            "Negative" => "负序",
                            name => name,
                        }
                    } else {
                        component.name
                    };
                    if component.name == "Positive" {
                        ui.strong(component_name);
                        ui.strong(format!("{:.6}", component.amplitude));
                        ui.strong(format!("{:.2}", component.phase_deg));
                        ui.strong(format!("{:.2}", component.percent_of_positive));
                    } else {
                        ui.label(component_name);
                        ui.label(format!("{:.6}", component.amplitude));
                        ui.label(format!("{:.2}", component.phase_deg));
                        ui.label(format!("{:.2}", component.percent_of_positive));
                    }
                    ui.end_row();
                }
            });
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
        let compare_selected = self.selected_compare_channels();
        let (plot_y_min, plot_y_max) = self.current_y_bounds();
        let response = Plot::new("scope_plot")
            .allow_drag(false)
            .allow_scroll(false)
            .allow_zoom(false)
            .show(ui, |plot_ui| {
                plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                    [self.view_start, plot_y_min],
                    [self.view_end, plot_y_max],
                ));

                for (out_index, channel_index) in selected.iter().enumerate() {
                    if out_index >= self.plot_cache.channels.len() {
                        continue;
                    }
                    let raw_points = self
                        .plot_cache
                        .times
                        .iter()
                        .zip(self.plot_cache.channels[out_index].iter())
                        .map(|(time, value)| [*time, self.scaled_value(*channel_index, *value)])
                        .collect::<Vec<_>>();
                    plot_ui.line(
                        Line::new(PlotPoints::from(raw_points))
                            .name(self.channel_name(*channel_index))
                            .color(self.channel_color(*channel_index))
                            .width(self.visible_line_width(*channel_index)),
                    );
                }

                if let Some(summary) = &self.plot_summary {
                    for (out_index, channel_index) in selected.iter().enumerate() {
                        if out_index >= summary.min.len() || out_index >= summary.max.len() {
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
                        plot_ui.line(
                            Line::new(PlotPoints::from(envelope))
                                .name(format!("{} min/max", self.channel_name(*channel_index)))
                                .color(self.channel_color(*channel_index))
                                .width(self.visible_line_width(*channel_index)),
                        );
                    }
                }

                for (out_index, channel_index) in compare_selected.iter().enumerate() {
                    if out_index >= self.compare_plot_cache.channels.len() {
                        continue;
                    }
                    let raw_points = self
                        .compare_plot_cache
                        .times
                        .iter()
                        .zip(self.compare_plot_cache.channels[out_index].iter())
                        .map(|(time, value)| [*time, self.scaled_value(*channel_index, *value)])
                        .collect::<Vec<_>>();
                    plot_ui.line(
                        Line::new(PlotPoints::from(raw_points))
                            .name(format!("B: {}", self.channel_name(*channel_index)))
                            .color(self.channel_color(*channel_index))
                            .style(LineStyle::Dashed { length: 8.0 })
                            .width(self.compare_line_width(*channel_index)),
                    );
                }

                if let Some(summary) = &self.compare_plot_summary {
                    for (out_index, channel_index) in compare_selected.iter().enumerate() {
                        if out_index >= summary.min.len() || out_index >= summary.max.len() {
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
                        plot_ui.line(
                            Line::new(PlotPoints::from(envelope))
                                .name(format!("B: {} min/max", self.channel_name(*channel_index)))
                                .color(self.channel_color(*channel_index))
                                .style(LineStyle::Dashed { length: 8.0 })
                                .width(self.compare_line_width(*channel_index)),
                        );
                    }
                }

                let cursor_label_y = plot_y_max - (plot_y_max - plot_y_min) * 0.05;
                if self.show_cursor_a {
                    let color = Self::cursor_color(CursorId::A);
                    plot_ui.vline(
                        VLine::new(self.cursor_a)
                            .name("A")
                            .color(color)
                            .width(2.5),
                    );
                    plot_ui.text(
                        Text::new(
                            PlotPoint::new(self.cursor_a, cursor_label_y),
                            RichText::new("A").strong().color(Color32::BLACK).background_color(color),
                        )
                        .anchor(egui::Align2::CENTER_TOP),
                    );
                }
                if self.show_cursor_b {
                    let color = Self::cursor_color(CursorId::B);
                    plot_ui.vline(
                        VLine::new(self.cursor_b)
                            .name("B")
                            .color(color)
                            .width(2.5),
                    );
                    plot_ui.text(
                        Text::new(
                            PlotPoint::new(self.cursor_b, cursor_label_y),
                            RichText::new("B").strong().color(Color32::BLACK).background_color(color),
                        )
                        .anchor(egui::Align2::CENTER_TOP),
                    );
                }

                if let (Some(cursor), Some(pointer)) =
                    (self.cursor_place_mode, plot_ui.pointer_coordinate())
                {
                    let place_name = match self.language {
                        Language::Zh => format!("放置 {}", Self::cursor_label(cursor)),
                        Language::En => format!("Place {}", Self::cursor_label(cursor)),
                    };
                    plot_ui.vline(
                        VLine::new(pointer.x)
                            .name(place_name)
                            .color(Self::cursor_color(cursor))
                            .style(LineStyle::Dashed { length: 6.0 })
                            .width(2.5),
                    );
                }

            });

        let hover_time = response
            .response
            .hover_pos()
            .map(|pos| response.transform.value_from_position(pos).x);

        response.response.context_menu(|ui| {
            if ui.button(self.tr("放置光标 A", "Place Cursor A")).clicked() {
                self.cursor_place_mode = Some(CursorId::A);
                self.zoom_box_start = None;
                self.zoom_box_current = None;
                ui.close_menu();
            }
            if ui.button(self.tr("放置光标 B", "Place Cursor B")).clicked() {
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
                if ui.button(self.tr("隐藏光标 A", "Hide Cursor A")).clicked() {
                    self.show_cursor_a = false;
                    ui.close_menu();
                }
            } else if ui.button(self.tr("显示光标 A", "Show Cursor A")).clicked() {
                self.show_cursor_a = true;
                ui.close_menu();
            }
            if self.show_cursor_b {
                if ui.button(self.tr("隐藏光标 B", "Hide Cursor B")).clicked() {
                    self.show_cursor_b = false;
                    ui.close_menu();
                }
            } else if ui.button(self.tr("显示光标 B", "Show Cursor B")).clicked() {
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
                    self.zoom_y(center_y, factor);
                }
                ui.ctx().request_repaint();
            }

            let drag_delta = response.response.drag_delta();
            if response.response.dragged_by(PointerButton::Secondary) && drag_delta.x.abs() > 0.0 {
                let time_per_pixel = self.visible_time_span() / response.response.rect.width() as f64;
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

        if self.cursor_place_mode.is_none() && response.response.drag_started_by(PointerButton::Primary) {
            self.zoom_box_start = response.response.interact_pointer_pos();
            self.zoom_box_current = self.zoom_box_start;
        }

        if self.cursor_place_mode.is_none() && response.response.dragged_by(PointerButton::Primary) {
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

        if self.cursor_place_mode.is_none() && response.response.drag_stopped_by(PointerButton::Primary) {
            if let (Some(start), Some(end)) = (self.zoom_box_start.take(), self.zoom_box_current.take()) {
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
}

impl eframe::App for ScopeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_theme(ctx);
        self.handle_shortcuts(ctx);

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| self.top_bar(ui));
        self.help_window(ctx);
        self.options_window(ctx);

        egui::SidePanel::left("channels")
            .resizable(true)
            .default_width(230.0)
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

            if let Some(result) = &self.fft_result {
                ui.separator();
                let spectrum_label = self.tr("频谱", "Spectrum");
                Plot::new("fft_plot")
                    .height(180.0)
                    .include_y(0.0)
                    .show(ui, |plot_ui| {
                        let points = result
                            .spectrum
                            .iter()
                            .copied()
                            .collect::<Vec<_>>();
                        plot_ui.line(Line::new(PlotPoints::from(points)).name(spectrum_label).color(Color32::LIGHT_GREEN));
                    });
            }
        });
    }
}
