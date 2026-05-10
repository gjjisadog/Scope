use std::{
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

const MAX_DRAW_POINTS: usize = 20_000;
const MAX_FFT_POINTS: usize = 262_144;
const ZOOM_BOX_MIN_PIXELS: f32 = 8.0;
const CONFIG_VERSION: u32 = 1;
const DEFAULT_WHEEL_ZOOM_SENSITIVITY: f64 = 0.125;
const MIN_WHEEL_ZOOM_SENSITIVITY: f64 = 0.025;
const MAX_WHEEL_ZOOM_SENSITIVITY: f64 = 0.40;

fn default_sample_rate_hz() -> f64 {
    1000.0
}

fn default_wheel_zoom_sensitivity() -> f64 {
    DEFAULT_WHEEL_ZOOM_SENSITIVITY
}

fn default_language() -> Language {
    Language::Zh
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum Language {
    Zh,
    En,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AppConfig {
    version: u32,
    display_names: Vec<String>,
    visible: Vec<bool>,
    fft_channel: usize,
    #[serde(default = "default_wheel_zoom_sensitivity")]
    wheel_zoom_sensitivity: f64,
    #[serde(default = "default_sample_rate_hz")]
    sample_rate_hz: f64,
    #[serde(default = "default_language")]
    language: Language,
}

pub struct ScopeApp {
    source: Option<Box<dyn DataSource>>,
    source_kind: Option<SourceKind>,
    visible: Vec<bool>,
    display_names: Vec<String>,
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
    last_error: Option<String>,
    loaded_path: Option<PathBuf>,
    plot_cache: SampleBlock,
    plot_summary: Option<RangeSummary>,
    fft_result: Option<FftResult>,
    sequence_result: Option<SequenceResult>,
    fft_channel: usize,
    needs_fft_reload: bool,
    needs_plot_reload: bool,
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

impl Language {
    fn label(self) -> &'static str {
        match self {
            Language::Zh => "中文",
            Language::En => "English",
        }
    }
}

impl ScopeApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            source: None,
            source_kind: None,
            visible: Vec::new(),
            display_names: Vec::new(),
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
            last_error: None,
            loaded_path: None,
            plot_cache: SampleBlock::default(),
            plot_summary: None,
            fft_result: None,
            sequence_result: None,
            fft_channel: 0,
            needs_fft_reload: false,
            needs_plot_reload: false,
            cursor_place_mode: None,
            zoom_box_start: None,
            zoom_box_current: None,
        }
    }

    fn meta(&self) -> Option<&DatasetMeta> {
        self.source.as_ref().map(|source| source.metadata())
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
        self.needs_fft_reload = true;
        self.plot_cache = SampleBlock::default();
        self.plot_summary = None;
        self.loaded_path = Some(path);
        self.source = Some(source);
        self.source_kind = Some(kind);
        self.last_error = None;
        self.needs_plot_reload = true;
        self.cursor_place_mode = None;
    }

    fn open_standard_csv(&mut self, path: PathBuf) {
        match CsvDataSource::open(&path) {
            Ok(source) => self.set_source(Box::new(source), path, SourceKind::Local),
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn open_cloud_csv(&mut self, path: PathBuf) {
        match CloudCsvDataSource::open_with_sample_rate(&path, self.sample_rate_hz) {
            Ok(source) => self.set_source(Box::new(source), path, SourceKind::Cloud),
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn open_auto_csv(&mut self, path: PathBuf) {
        match Self::looks_like_cloud_csv(&path) {
            Ok(true) => self.open_cloud_csv(path),
            Ok(false) => self.open_standard_csv(path),
            Err(error) => self.last_error = Some(error),
        }
    }

    fn looks_like_cloud_csv(path: &Path) -> Result<bool, String> {
        let file = File::open(path).map_err(|error| error.to_string())?;
        let mut reader = BufReader::new(file);
        let mut header = String::new();
        let bytes = reader
            .read_line(&mut header)
            .map_err(|error| error.to_string())?;
        if bytes == 0 {
            return Err("CSV file is empty.".to_owned());
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
            fft_channel: self.fft_channel,
            wheel_zoom_sensitivity: self.wheel_zoom_sensitivity,
            sample_rate_hz: self.sample_rate_hz,
            language: self.language,
        }
    }

    fn apply_config(&mut self, config: AppConfig) {
        self.language = config.language;
        let channel_count = self.display_names.len();
        for (index, name) in config.display_names.into_iter().enumerate().take(channel_count) {
            self.display_names[index] = name;
        }
        for (index, visible) in config.visible.into_iter().enumerate().take(self.visible.len()) {
            self.visible[index] = visible;
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
        self.needs_fft_reload = true;
    }

    fn reload_cloud_with_current_sample_rate(&mut self) {
        if self.source_kind != Some(SourceKind::Cloud) {
            self.needs_fft_reload = true;
            return;
        }
        let Some(path) = self.loaded_path.clone() else {
            return;
        };
        let config = self.current_config();
        match CloudCsvDataSource::open_with_sample_rate(&path, self.sample_rate_hz) {
            Ok(source) => {
                self.set_source(Box::new(source), path, SourceKind::Cloud);
                self.apply_config(config);
            }
            Err(error) => self.last_error = Some(error.to_string()),
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

    fn reload_plot_cache(&mut self) {
        let Some(source) = &self.source else {
            return;
        };
        let channels = self.selected_channels();
        let estimated_points =
            ((self.view_end - self.view_start) * source.metadata().nominal_sample_rate_hz)
                .max(0.0) as usize;
        if estimated_points > MAX_DRAW_POINTS * 2 {
            match source.summarize_range(self.view_start, self.view_end, &channels, MAX_DRAW_POINTS / 2) {
                Ok(summary) => {
                    self.plot_cache = SampleBlock::default();
                    self.plot_summary = Some(summary);
                    self.needs_plot_reload = false;
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        } else {
            match source.read_range(self.view_start, self.view_end, &channels, MAX_DRAW_POINTS) {
                Ok(block) => {
                    self.plot_cache = block;
                    self.plot_summary = None;
                    self.needs_plot_reload = false;
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
        if let Some(summary) = &self.plot_summary {
            for values in &summary.min {
                for value in values {
                    min = min.min(*value as f64);
                }
            }
            for values in &summary.max {
                for value in values {
                    max = max.max(*value as f64);
                }
            }
        } else {
            for values in &self.plot_cache.channels {
                for value in values {
                    min = min.min(*value as f64);
                    max = max.max(*value as f64);
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
                    .and_then(|samples| fft::analyze(channel_name, samples, sample_rate_hz, 10));

                if let Some((group_name, group_channels)) = sequence_group {
                    if let Ok(group_block) =
                        source.read_range(start, end, &group_channels, MAX_FFT_POINTS)
                    {
                        if group_block.channels.len() == 3 {
                            next_sequence = fft::analyze_sequence(
                                group_name,
                                &group_block.channels[0],
                                &group_block.channels[1],
                                &group_block.channels[2],
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

    fn channel_color(index: usize) -> Color32 {
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

    fn tr(&self, zh: &'static str, en: &'static str) -> &'static str {
        match self.language {
            Language::Zh => zh,
            Language::En => en,
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button(self.tr("打开 CSV", "Open CSV")).clicked() {
                let filter_name = self.tr("波形 CSV", "Waveform CSV");
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(filter_name, &["csv"])
                    .pick_file()
                {
                    self.open_auto_csv(path);
                }
            }
            if ui.button(self.tr("重置视图", "Reset View")).clicked() {
                self.reset_view();
            }
            if ui.button(self.tr("适配光标", "Fit Cursors")).clicked() {
                self.view_start = self.cursor_a.min(self.cursor_b);
                self.view_end = self.cursor_a.max(self.cursor_b);
                self.needs_plot_reload = true;
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
                    ui.label(format!(
                        "{} | {} 点 | {:.3}s | 数据 {:.1} Hz | 设置 Fs {:.1} Hz",
                        meta.source_name,
                        meta.sample_count,
                        meta.duration(),
                        meta.nominal_sample_rate_hz,
                        self.sample_rate_hz
                    ));
                } else {
                    ui.label(format!(
                        "{} | {} samples | {:.3}s | data {:.1} Hz | Fs set {:.1} Hz",
                        meta.source_name,
                        meta.sample_count,
                        meta.duration(),
                        meta.nominal_sample_rate_hz,
                        self.sample_rate_hz
                    ));
                }
            } else {
                ui.label(self.tr(
                    "打开 CSV 文件开始分析；软件会自动识别云端 Content 或本地数值 CSV。",
                    "Open a waveform CSV to begin. Content files are detected automatically.",
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
                        ui.label("使用顶部“打开 CSV”入口。软件读取第一行表头后，会自动选择云端 Content 解析器或本地数值 CSV 解析器。");
                        ui.strong("云端 Content CSV");
                        ui.label("第一行为 Content。后续每行是一条十六进制报文，每条报文解析为 2 个采样点。每个采样点包含 30 个模拟量通道和 30 个数字/状态通道。模拟量按 little-endian int16 解析，第 31/32 个 raw word 按原 MATLAB 脚本规则拆成数字/状态通道。");
                        ui.add_space(6.0);
                        ui.strong("本地/数值 CSV");
                        ui.label("第一列为时间，单位秒；后续列为通道值，最多读取 128 个数值通道。文件打开时建立分块索引和 min/max 摘要，绘图只读取当前视窗或摘要。");
                        ui.monospace("time,CH1,CH2,CH3\n0.000000,0.0,0.1,0.2\n0.000010,0.1,0.2,0.3");

                        ui.separator();
                        ui.heading("波形操作");
                        ui.label("选项：设置采样频率，默认 1000 Hz。云端 Content CSV 用它生成秒级时间轴，FFT 也使用该设置。");
                        ui.label("鼠标滚轮：以鼠标位置为中心缩放纵轴幅值范围。");
                        ui.label("Ctrl + 鼠标滚轮：以鼠标位置为中心缩放横轴时间范围。");
                        ui.label("选项：可调整滚轮缩放敏感度，也可切换中文/英文界面。");
                        ui.label("左侧变量栏：勾选通道、编辑显示名、搜索变量，并作为图例使用。");
                        ui.label("鼠标悬停左侧变量：对应波形会加粗高亮。");
                        ui.label("导入/导出配置：保存和恢复变量名、通道显示、FFT 通道、采样频率、缩放敏感度和界面语言。");
                        ui.label("左键单击波形：移动距离最近的光标。");
                        ui.label("左键拖拽波形：框选时间区域并放大。");
                        ui.label("右键单击波形：打开光标菜单。");
                        ui.label("放置光标 A/B：显示红色虚线预览光标，左键确认，Esc 取消。");
                        ui.label("隐藏/显示光标 A/B：只切换显示状态，不改变光标位置和测量结果。");
                        ui.label("右键拖拽波形：平移当前视图。");
                        ui.label("适配光标：缩放到光标 A/B 的时间范围。");

                        ui.separator();
                        ui.heading("FFT、THD 和序分量");
                        ui.label("FFT 面板会自动分析光标 A/B 之间选中 FFT 通道的波形。");
                        ui.label("谐波表显示频率、幅值、相位、dBc 和 THD。");
                        ui.label("当 FFT 通道属于 stVg_0.iA/iB/iC、stIg_0.iA/iB/iC 或 stVinv_0.iA/iB/iC 时，软件同时显示零序、正序和负序分量。");
                        ui.label("单通道 FFT 相位会随光标起点变化；序分量按 A-B-C 正序约定计算，重点看相对相位和正/负/零序幅值比例。");

                        ui.separator();
                        ui.heading("构建 / 打包");
                        ui.label("本地运行：");
                        ui.monospace("cargo run --release");
                        ui.label("创建 Windows 便携包：");
                        ui.monospace("powershell -ExecutionPolicy Bypass -File scripts/package-windows.ps1");
                        ui.label("输出：dist/ScopeAnalyzer-0.1.0-win-x64.zip");
                    } else {
                        ui.heading("Scope Analyzer");
                        ui.label("Windows offline waveform analyzer with channel selection, oscilloscope-style zooming, cursor measurement, FFT, THD, and sequence components.");

                        ui.separator();
                        ui.heading("Supported CSV Formats");
                        ui.label("Use Open CSV. The software reads the first CSV header and automatically chooses the cloud Content parser or the local numeric CSV parser.");
                        ui.strong("Cloud Content CSV");
                        ui.label("The first row is Content. Each following row is a hexadecimal record. Each record is decoded into two samples. Each sample contains 30 analog channels plus 30 digital/status channels. Analog channels use little-endian int16. The 31st and 32nd raw words are expanded into digital/status channels according to the original MATLAB script.");
                        ui.add_space(6.0);
                        ui.strong("Local / Numeric CSV");
                        ui.label("The first column is time in seconds. Remaining columns are channel values. Up to 128 numeric channels are loaded. The file is indexed in blocks and the plot reads only the current view or min/max summaries.");
                        ui.monospace("time,CH1,CH2,CH3\n0.000000,0.0,0.1,0.2\n0.000010,0.1,0.2,0.3");

                        ui.separator();
                        ui.heading("Waveform Controls");
                        ui.label("Options: set sample rate. Default is 1000 Hz. Cloud Content CSV uses it to convert sample index to seconds; FFT also uses this setting.");
                        ui.label("Mouse wheel: zoom vertical amplitude range around the pointer.");
                        ui.label("Ctrl + mouse wheel: zoom horizontal time range around the pointer.");
                        ui.label("Options: adjust mouse wheel zoom sensitivity and choose Chinese or English UI language.");
                        ui.label("Left channel list: select channels, edit display names, search variables, and use it as the legend.");
                        ui.label("Hover a variable in the left list: the corresponding waveform becomes thicker.");
                        ui.label("Import/Export Config: save and restore display names, channel visibility, FFT channel, sample rate, wheel zoom sensitivity, and UI language.");
                        ui.label("Left click plot: move the nearest cursor to the clicked position.");
                        ui.label("Left drag plot: box-select a time range and zoom in.");
                        ui.label("Right click plot: open cursor menu.");
                        ui.label("Place Cursor A/B: shows a red dashed preview cursor; left click confirms, Esc cancels.");
                        ui.label("Hide/Show Cursor A/B: toggles cursor visibility without changing cursor position or measurements.");
                        ui.label("Right drag plot: pan the current view.");
                        ui.label("Fit Cursors: zoom to the time range between cursor A and cursor B.");

                        ui.separator();
                        ui.heading("FFT, THD, and Sequence");
                        ui.label("The FFT panel automatically analyzes the selected FFT channel between cursor A and cursor B.");
                        ui.label("The harmonic table shows frequency, amplitude, phase, dBc, and THD.");
                        ui.label("If the FFT channel belongs to stVg_0.iA/iB/iC, stIg_0.iA/iB/iC, or stVinv_0.iA/iB/iC, the software also shows zero, positive, and negative sequence components.");
                        ui.label("Single-channel FFT phase depends on cursor start time. Sequence analysis uses the A-B-C positive-sequence convention; focus on relative phase and positive/negative/zero sequence magnitude ratios.");

                        ui.separator();
                        ui.heading("Build / Packaging");
                        ui.label("Run locally with:");
                        ui.monospace("cargo run --release");
                        ui.label("Create a Windows portable package with:");
                        ui.monospace("powershell -ExecutionPolicy Bypass -File scripts/package-windows.ps1");
                        ui.label("Output: dist/ScopeAnalyzer-0.1.0-win-x64.zip");
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
                ui.separator();
                let old_sample_rate = self.sample_rate_hz;
                let sample_rate_prefix = self.tr("采样频率: ", "Sample rate: ");
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
                    "默认采样频率为 1000 Hz。云端 Content CSV 会用该值生成时间轴，FFT 也使用该值。",
                    "Default sample rate is 1000 Hz. Cloud Content CSV uses this value for the time axis and FFT.",
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
                    "Ctrl + 鼠标滚轮缩放横轴。",
                    "Ctrl + mouse wheel zooms the horizontal axis.",
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

    fn channel_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading(self.tr("变量", "Channels"));
        ui.horizontal(|ui| {
            if ui.button(self.tr("全选", "All")).clicked() {
                self.visible.fill(true);
                self.needs_plot_reload = true;
            }
            if ui.button(self.tr("全不选", "None")).clicked() {
                self.visible.fill(false);
                self.needs_plot_reload = true;
            }
        });
        let filter_hint = self.tr("筛选", "Filter");
        ui.add(
            egui::TextEdit::singleline(&mut self.channel_filter)
                .hint_text(filter_hint)
                .desired_width(f32::INFINITY),
        );
        ui.separator();

        let Some(meta) = self.meta().cloned() else {
            ui.label(self.tr("未加载数据。", "No data loaded."));
            return;
        };
        let filter = self.channel_filter.to_lowercase();
        let mut hovered_channel = None;
        let source_label = self.tr("原始", "src");
        egui::ScrollArea::vertical().show(ui, |ui| {
            for channel in &meta.channels {
                let display_name = self.channel_name(channel.index);
                if !filter.is_empty()
                    && !display_name.to_lowercase().contains(&filter)
                    && !channel.name.to_lowercase().contains(&filter)
                {
                    continue;
                }
                let row_response = ui
                    .horizontal(|ui| {
                        let mut row_hovered = false;
                        let color = Self::channel_color(channel.index);
                        let (rect, color_response) =
                            ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        row_hovered |= color_response.hovered();
                        ui.painter().rect_filled(rect, 2.0, color);
                        let checkbox_response = ui.checkbox(&mut self.visible[channel.index], "");
                        row_hovered |= checkbox_response.hovered();
                        if checkbox_response.changed() {
                            self.needs_plot_reload = true;
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
                                .label(
                                    RichText::new(format!("{source_label}: {}", channel.name))
                                        .small(),
                                )
                                .hovered();
                        }
                        row_hovered
                    });
                if row_response.response.hovered() || row_response.inner {
                    hovered_channel = Some(channel.index);
                }
            }
        });
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

        let Some(source) = &self.source else {
            return;
        };
        let channels = self.selected_channels();
        if channels.is_empty() {
            return;
        }
        let start = self.cursor_a.min(self.cursor_b);
        let end = self.cursor_a.max(self.cursor_b);
        if let Ok(block) = source.read_range(start, end, &channels, 2) {
            for (out_index, &channel_index) in channels.iter().enumerate().take(12) {
                let values = &block.channels[out_index];
                if let (Some(first), Some(last)) = (values.first(), values.last()) {
                    let name = self.channel_name(channel_index);
                    let text = format!(
                        "{}  yA={:.5}  yB={:.5}  dy={:.5}",
                        name,
                        first,
                        last,
                        last - first
                    );
                    if self.hovered_channel == Some(channel_index) {
                        ui.label(
                            RichText::new(text)
                                .strong()
                                .color(Self::channel_color(channel_index))
                                .background_color(Color32::from_rgba_premultiplied(255, 240, 160, 80)),
                        );
                    } else {
                        ui.label(text);
                    }
                }
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
            "自动分析光标 A-B 区间。",
            "Auto analyzes the cursor A-B range.",
        ));
        if self.needs_fft_reload {
            self.run_fft();
        }

        if let Some(result) = &self.fft_result {
            ui.separator();
            if self.language == Language::Zh {
                ui.label(format!("通道: {}", result.channel_name));
                ui.label(format!("样本数: {}", result.sample_count));
                ui.label(format!("基波: {:.3} Hz", result.fundamental_hz));
            } else {
                ui.label(format!("Channel: {}", result.channel_name));
                ui.label(format!("Samples: {}", result.sample_count));
                ui.label(format!("Fundamental: {:.3} Hz", result.fundamental_hz));
            }
            ui.label(format!("THD: {:.3}%", result.thd_percent));
            egui::Grid::new("harmonics").striped(true).show(ui, |ui| {
                ui.strong("N");
                ui.strong("Hz");
                ui.strong(self.tr("幅值", "Amp"));
                ui.strong(self.tr("相位", "Phase"));
                ui.strong("dBc");
                ui.end_row();
                for row in &result.harmonics {
                    ui.label(row.order.to_string());
                    ui.label(format!("{:.2}", row.frequency_hz));
                    ui.label(format!("{:.5}", row.amplitude));
                    ui.label(format!("{:.2} deg", row.phase_deg));
                    ui.label(format!("{:.2}", row.relative_db));
                    ui.end_row();
                }
            });
        }

        if let Some(sequence) = &self.sequence_result {
            ui.separator();
            ui.heading(self.tr("序分量", "Sequence"));
            if self.language == Language::Zh {
                ui.label(format!(
                    "{} | 基波: {:.3} Hz",
                    sequence.group_name, sequence.fundamental_hz
                ));
                ui.label(format!(
                    "A/B/C 相位: {:.2} deg / {:.2} deg / {:.2} deg",
                    sequence.phase_a_deg, sequence.phase_b_deg, sequence.phase_c_deg
                ));
            } else {
                ui.label(format!(
                    "{} | Fundamental: {:.3} Hz",
                    sequence.group_name, sequence.fundamental_hz
                ));
                ui.label(format!(
                    "Phase A/B/C: {:.2} deg / {:.2} deg / {:.2} deg",
                    sequence.phase_a_deg, sequence.phase_b_deg, sequence.phase_c_deg
                ));
            }
            egui::Grid::new("sequence_components").striped(true).show(ui, |ui| {
                ui.strong(self.tr("序", "Seq"));
                ui.strong(self.tr("幅值", "Amp"));
                ui.strong(self.tr("相位", "Phase"));
                ui.strong(self.tr("占正序", "% Pos"));
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
                    ui.label(component_name);
                    ui.label(format!("{:.5}", component.amplitude));
                    ui.label(format!("{:.2} deg", component.phase_deg));
                    ui.label(format!("{:.2}%", component.percent_of_positive));
                    ui.end_row();
                }
            });
        }
    }

    fn plot_panel(&mut self, ui: &mut egui::Ui) {
        if self.needs_plot_reload {
            self.reload_plot_cache();
        }

        let selected = self.selected_channels();
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
                        .map(|(time, value)| [*time, *value as f64])
                        .collect::<Vec<_>>();
                    plot_ui.line(
                        Line::new(PlotPoints::from(raw_points))
                            .name(self.channel_name(*channel_index))
                            .color(Self::channel_color(*channel_index))
                            .width(if self.hovered_channel == Some(*channel_index) {
                                4.0
                            } else {
                                1.4
                            }),
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
                            envelope.push([mid, summary.min[out_index][i] as f64]);
                            envelope.push([mid, summary.max[out_index][i] as f64]);
                        }
                        plot_ui.line(
                            Line::new(PlotPoints::from(envelope))
                                .name(format!("{} min/max", self.channel_name(*channel_index)))
                                .color(Self::channel_color(*channel_index))
                                .width(if self.hovered_channel == Some(*channel_index) {
                                    4.0
                                } else {
                                    1.4
                                }),
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
            if (zoom_delta - 1.0).abs() > f32::EPSILON {
                self.zoom(center_x, self.ctrl_zoom_factor(zoom_delta));
                ui.ctx().request_repaint();
            } else if scroll.abs() > 0.0 {
                let factor = self.wheel_zoom_factor(scroll);
                if ctrl_down {
                    self.zoom(center_x, factor);
                    ui.ctx().request_repaint();
                } else {
                    let center_y = response
                        .response
                        .hover_pos()
                        .map(|pos| response.transform.value_from_position(pos).y)
                        .unwrap_or((plot_y_min + plot_y_max) * 0.5);
                    self.zoom_y(center_y, factor);
                    ui.ctx().request_repaint();
                }
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
