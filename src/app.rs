use std::path::PathBuf;

use eframe::egui::{self, Color32, PointerButton, RichText};
use egui_plot::{Legend, Line, Plot, PlotPoints, VLine};

use crate::{
    data::{CloudCsvDataSource, CsvDataSource, DataSource, DatasetMeta, RangeSummary, SampleBlock},
    fft::{self, FftResult},
};

const MAX_DRAW_POINTS: usize = 20_000;
const MAX_FFT_POINTS: usize = 262_144;

pub struct ScopeApp {
    source: Option<Box<dyn DataSource>>,
    visible: Vec<bool>,
    view_start: f64,
    view_end: f64,
    cursor_a: f64,
    cursor_b: f64,
    active_cursor: CursorId,
    channel_filter: String,
    last_error: Option<String>,
    loaded_path: Option<PathBuf>,
    plot_cache: SampleBlock,
    plot_summary: Option<RangeSummary>,
    fft_result: Option<FftResult>,
    fft_channel: usize,
    needs_plot_reload: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CursorId {
    A,
    B,
}

impl ScopeApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            source: None,
            visible: Vec::new(),
            view_start: 0.0,
            view_end: 1.0,
            cursor_a: 0.25,
            cursor_b: 0.75,
            active_cursor: CursorId::A,
            channel_filter: String::new(),
            last_error: None,
            loaded_path: None,
            plot_cache: SampleBlock::default(),
            plot_summary: None,
            fft_result: None,
            fft_channel: 0,
            needs_plot_reload: false,
        }
    }

    fn meta(&self) -> Option<&DatasetMeta> {
        self.source.as_ref().map(|source| source.metadata())
    }

    fn set_source(&mut self, source: Box<dyn DataSource>, path: PathBuf) {
        let meta = source.metadata().clone();
        self.visible = meta
            .channels
            .iter()
            .map(|channel| channel.default_visible)
            .collect();
        self.view_start = meta.start_time;
        self.view_end = meta.end_time;
        let span = meta.duration();
        self.cursor_a = meta.start_time + span * 0.33;
        self.cursor_b = meta.start_time + span * 0.66;
        self.fft_channel = 0;
        self.fft_result = None;
        self.plot_cache = SampleBlock::default();
        self.plot_summary = None;
        self.loaded_path = Some(path);
        self.source = Some(source);
        self.last_error = None;
        self.needs_plot_reload = true;
    }

    fn open_standard_csv(&mut self, path: PathBuf) {
        match CsvDataSource::open(&path) {
            Ok(source) => self.set_source(Box::new(source), path),
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn open_cloud_csv(&mut self, path: PathBuf) {
        match CloudCsvDataSource::open(&path) {
            Ok(source) => self.set_source(Box::new(source), path),
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn selected_channels(&self) -> Vec<usize> {
        self.visible
            .iter()
            .enumerate()
            .filter_map(|(index, visible)| visible.then_some(index))
            .collect()
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
            self.needs_plot_reload = true;
        }
    }

    fn move_active_cursor(&mut self, time: f64) {
        let Some(meta) = self.meta() else {
            return;
        };
        let clamped = time.clamp(meta.start_time, meta.end_time);
        match self.active_cursor {
            CursorId::A => self.cursor_a = clamped,
            CursorId::B => self.cursor_b = clamped,
        }
    }

    fn run_fft(&mut self) {
        let Some(source) = &self.source else {
            return;
        };
        let channel_count = source.metadata().channels.len();
        if channel_count == 0 {
            return;
        }
        let fft_channel = self.fft_channel.min(channel_count - 1);
        self.fft_channel = fft_channel;
        let start = self.cursor_a.min(self.cursor_b);
        let end = self.cursor_a.max(self.cursor_b);
        let channel_name = source.metadata().channels[fft_channel].name.clone();
        let sample_rate_hz = source.metadata().channels[fft_channel].sample_rate_hz;
        match source.read_range(start, end, &[fft_channel], MAX_FFT_POINTS) {
            Ok(block) => {
                self.fft_result = block.channels.first().and_then(|samples| {
                    fft::analyze(channel_name, samples, sample_rate_hz, 10)
                });
                if self.fft_result.is_none() {
                    self.last_error = Some("FFT needs at least 16 samples in the cursor range.".to_owned());
                }
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
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

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Open Cloud CSV").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Cloud Content CSV", &["csv"])
                    .pick_file()
                {
                    self.open_cloud_csv(path);
                }
            }
            if ui.button("Open Local CSV").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Numeric CSV", &["csv"])
                    .pick_file()
                {
                    self.open_standard_csv(path);
                }
            }
            if ui.button("Reset View").clicked() {
                self.reset_view();
            }
            if ui.button("Fit Cursors").clicked() {
                self.view_start = self.cursor_a.min(self.cursor_b);
                self.view_end = self.cursor_a.max(self.cursor_b);
                self.needs_plot_reload = true;
            }
            ui.separator();
            if let Some(meta) = self.meta() {
                ui.label(format!(
                    "{} | {} samples | {:.3}s | {:.1} Hz",
                    meta.source_name,
                    meta.sample_count,
                    meta.duration(),
                    meta.nominal_sample_rate_hz
                ));
            } else {
                ui.label("Open a cloud Content CSV or a local numeric CSV to begin.");
            }
        });
    }

    fn channel_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Channels");
        ui.horizontal(|ui| {
            if ui.button("All").clicked() {
                self.visible.fill(true);
                self.needs_plot_reload = true;
            }
            if ui.button("None").clicked() {
                self.visible.fill(false);
                self.needs_plot_reload = true;
            }
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.channel_filter)
                .hint_text("Filter")
                .desired_width(f32::INFINITY),
        );
        ui.separator();

        let Some(meta) = self.meta().cloned() else {
            ui.label("No data loaded.");
            return;
        };
        let filter = self.channel_filter.to_lowercase();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for channel in &meta.channels {
                if !filter.is_empty() && !channel.name.to_lowercase().contains(&filter) {
                    continue;
                }
                ui.horizontal(|ui| {
                    let color = Self::channel_color(channel.index);
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 2.0, color);
                    let changed = ui
                        .checkbox(
                            &mut self.visible[channel.index],
                            format!(
                                "{}{}",
                                channel.name,
                                if channel.unit.is_empty() {
                                    String::new()
                                } else {
                                    format!(" ({})", channel.unit)
                                }
                            ),
                        )
                        .changed();
                    if changed {
                        self.needs_plot_reload = true;
                    }
                });
            }
        });
    }

    fn measurements_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Cursors");
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.active_cursor, CursorId::A, "A");
            ui.radio_value(&mut self.active_cursor, CursorId::B, "B");
        });
        ui.label(format!("A: {:.9}s", self.cursor_a));
        ui.label(format!("B: {:.9}s", self.cursor_b));
        let dt = (self.cursor_b - self.cursor_a).abs();
        ui.label(format!("dt: {:.9}s", dt));
        if dt > 0.0 {
            ui.label(format!("1/dt: {:.3} Hz", 1.0 / dt));
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
                    let name = &source.metadata().channels[channel_index].name;
                    ui.label(format!(
                        "{}  yA={:.5}  yB={:.5}  dy={:.5}",
                        name,
                        first,
                        last,
                        last - first
                    ));
                }
            }
        }
    }

    fn fft_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("FFT");
        let Some(meta) = self.meta().cloned() else {
            ui.label("No data loaded.");
            return;
        };

        egui::ComboBox::from_label("Channel")
            .selected_text(
                meta.channels
                    .get(self.fft_channel)
                    .map(|channel| channel.name.as_str())
                    .unwrap_or("CH1"),
            )
            .show_ui(ui, |ui| {
                for channel in &meta.channels {
                    ui.selectable_value(&mut self.fft_channel, channel.index, &channel.name);
                }
            });

        if ui.button("Analyze Cursor Range").clicked() {
            self.run_fft();
        }

        if let Some(result) = &self.fft_result {
            ui.separator();
            ui.label(format!("Channel: {}", result.channel_name));
            ui.label(format!("Samples: {}", result.sample_count));
            ui.label(format!("Fundamental: {:.3} Hz", result.fundamental_hz));
            ui.label(format!("THD: {:.3}%", result.thd_percent));
            egui::Grid::new("harmonics").striped(true).show(ui, |ui| {
                ui.strong("N");
                ui.strong("Hz");
                ui.strong("Amp");
                ui.strong("dBc");
                ui.end_row();
                for row in &result.harmonics {
                    ui.label(row.order.to_string());
                    ui.label(format!("{:.2}", row.frequency_hz));
                    ui.label(format!("{:.5}", row.amplitude));
                    ui.label(format!("{:.2}", row.relative_db));
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
        let response = Plot::new("scope_plot")
            .legend(Legend::default())
            .allow_drag(false)
            .allow_scroll(false)
            .allow_zoom(false)
            .include_x(self.view_start)
            .include_x(self.view_end)
            .show(ui, |plot_ui| {
                for (out_index, channel_index) in selected.iter().enumerate() {
                    if out_index >= self.plot_cache.channels.len() {
                        continue;
                    }
                    let Some(meta) = self.meta() else {
                        continue;
                    };
                    let raw_points = self
                        .plot_cache
                        .times
                        .iter()
                        .zip(self.plot_cache.channels[out_index].iter())
                        .map(|(time, value)| [*time, *value as f64])
                        .collect::<Vec<_>>();
                    plot_ui.line(
                        Line::new(PlotPoints::from(raw_points))
                            .name(meta.channels[*channel_index].name.clone())
                            .color(Self::channel_color(*channel_index)),
                    );
                }

                if let Some(summary) = &self.plot_summary {
                    for (out_index, channel_index) in selected.iter().enumerate() {
                        if out_index >= summary.min.len() || out_index >= summary.max.len() {
                            continue;
                        }
                        let Some(meta) = self.meta() else {
                            continue;
                        };
                        let mut envelope = Vec::with_capacity(summary.bin_start.len() * 2);
                        for i in 0..summary.bin_start.len() {
                            let mid = (summary.bin_start[i] + summary.bin_end[i]) * 0.5;
                            envelope.push([mid, summary.min[out_index][i] as f64]);
                            envelope.push([mid, summary.max[out_index][i] as f64]);
                        }
                        plot_ui.line(
                            Line::new(PlotPoints::from(envelope))
                                .name(format!("{} min/max", meta.channels[*channel_index].name))
                                .color(Self::channel_color(*channel_index)),
                        );
                    }
                }

                plot_ui.vline(VLine::new(self.cursor_a).name("A").color(Color32::WHITE));
                plot_ui.vline(VLine::new(self.cursor_b).name("B").color(Color32::LIGHT_BLUE));

                if let Some(pointer) = plot_ui.pointer_coordinate() {
                    let (clicked, down) = plot_ui.ctx().input(|input| {
                        (
                            input.pointer.primary_clicked(),
                            input.pointer.primary_down(),
                        )
                    });
                    if clicked {
                        let distance_a = (pointer.x - self.cursor_a).abs();
                        let distance_b = (pointer.x - self.cursor_b).abs();
                        self.active_cursor = if distance_a <= distance_b {
                            CursorId::A
                        } else {
                            CursorId::B
                        };
                    }
                    if down {
                        self.move_active_cursor(pointer.x);
                    }
                }
            });

        if response.response.hovered() {
            let scroll = ui.ctx().input(|input| input.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                let center = response
                    .response
                    .hover_pos()
                    .map(|pos| response.transform.value_from_position(pos).x)
                    .unwrap_or((self.view_start + self.view_end) * 0.5);
                let factor = if scroll > 0.0 { 0.80 } else { 1.25 };
                self.zoom(center, factor);
            }

            let drag_delta = response.response.drag_delta();
            if response.response.dragged_by(PointerButton::Secondary) && drag_delta.x.abs() > 0.0 {
                let time_per_pixel = self.visible_time_span() / response.response.rect.width() as f64;
                self.pan(-(drag_delta.x as f64) * time_per_pixel);
                ui.ctx().request_repaint();
            }
        }
    }
}

impl eframe::App for ScopeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| self.top_bar(ui));

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
                Plot::new("fft_plot")
                    .height(180.0)
                    .include_y(0.0)
                    .show(ui, |plot_ui| {
                        let points = result
                            .spectrum
                            .iter()
                            .copied()
                            .collect::<Vec<_>>();
                        plot_ui.line(Line::new(PlotPoints::from(points)).name("Spectrum").color(Color32::LIGHT_GREEN));
                    });
            }
        });
    }
}
