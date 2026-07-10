use super::*;
use scope_analyzer::live::{
    session::ConnectionState,
    state::WorkspaceMode,
    transport::TransportConfig,
    trigger::{TriggerEdge, TriggerMode},
};

impl ScopeApp {
    pub(super) fn workspace_selector(&mut self, ui: &mut egui::Ui) {
        let offline_label = self.live_text("离线", "Offline");
        let live_label = self.live_text("实时", "Live");
        ui.selectable_value(
            &mut self.live.workspace_mode,
            WorkspaceMode::Offline,
            offline_label,
        );
        ui.selectable_value(
            &mut self.live.workspace_mode,
            WorkspaceMode::Live,
            live_label,
        );
    }

    pub(super) fn live_toolbar(&mut self, ui: &mut egui::Ui) {
        let disconnected = self.live.connection_state == ConnectionState::Disconnected;
        let mut use_tcp = matches!(self.live.transport, TransportConfig::Tcp { .. });
        let previous_use_tcp = use_tcp;
        ui.radio_value(&mut use_tcp, true, "TCP");
        ui.radio_value(&mut use_tcp, false, self.live_text("串口", "Serial"));
        if use_tcp != previous_use_tcp && disconnected {
            self.live.transport = if use_tcp {
                TransportConfig::Tcp {
                    address: "127.0.0.1:19090".to_owned(),
                }
            } else {
                TransportConfig::Serial {
                    port: self.live.serial_ports.first().cloned().unwrap_or_default(),
                    baud: 921_600,
                }
            };
        }

        let select_port_label = self.live_text("选择串口", "Select port");
        let serial_ports = self.live.serial_ports.clone();
        match &mut self.live.transport {
            TransportConfig::Tcp { address } => {
                ui.add_enabled(
                    disconnected,
                    egui::TextEdit::singleline(address).desired_width(150.0),
                );
            }
            TransportConfig::Serial { port, baud } => {
                egui::ComboBox::from_id_source("live_serial_port")
                    .selected_text(if port.is_empty() {
                        select_port_label
                    } else {
                        port.as_str()
                    })
                    .show_ui(ui, |ui| {
                        for candidate in &serial_ports {
                            ui.selectable_value(port, candidate.clone(), candidate);
                        }
                    });
                ui.add_enabled(
                    disconnected,
                    egui::DragValue::new(baud)
                        .clamp_range(1..=4_000_000)
                        .speed(115_200),
                );
                if ui
                    .add_enabled(disconnected, egui::Button::new("↻"))
                    .on_hover_text(self.live_text("刷新串口", "Refresh serial ports"))
                    .clicked()
                {
                    if let Err(error) = self.live.refresh_serial_ports() {
                        self.live.last_error = Some(error);
                    }
                }
            }
        }

        let connect_clicked = ui
            .add_enabled(
                disconnected,
                egui::Button::new(self.live_text("连接", "Connect")),
            )
            .clicked();
        let disconnect_clicked = ui
            .add_enabled(
                !disconnected,
                egui::Button::new(self.live_text("断开", "Disconnect")),
            )
            .clicked();
        let ready = self.live.connection_state == ConnectionState::Ready;
        let streaming = self.live.connection_state == ConnectionState::Streaming;
        let configure_clicked = ui
            .add_enabled(
                ready,
                egui::Button::new(self.live_text("应用采集", "Configure")),
            )
            .clicked();
        let start_clicked = ui
            .add_enabled(ready, egui::Button::new(self.live_text("开始", "Start")))
            .clicked();
        let stop_clicked = ui
            .add_enabled(streaming, egui::Button::new(self.live_text("停止", "Stop")))
            .clicked();
        let mut paused = self.live.display_paused;
        if ui
            .checkbox(&mut paused, self.live_text("暂停显示", "Pause display"))
            .changed()
        {
            self.live.set_display_paused(paused);
        }

        let record_clicked = ui
            .add_enabled(
                !disconnected && !self.live.is_recording(),
                egui::Button::new(self.live_text("录波", "Record")),
            )
            .clicked();
        let stop_record_clicked = ui
            .add_enabled(
                self.live.is_recording(),
                egui::Button::new(self.live_text("结束录波", "Finish recording")),
            )
            .clicked();
        let open_recording_clicked = ui
            .button(self.live_text("打开录波", "Open recording"))
            .clicked();

        if connect_clicked {
            let result = self.live.connect();
            self.apply_live_result(result);
        }
        if disconnect_clicked {
            let result = self.live.disconnect();
            self.apply_live_result(result);
        }
        if configure_clicked {
            let configure = self.live.acquisition.clone();
            let result = self.live.configure(configure);
            self.apply_live_result(result);
        }
        if start_clicked {
            let result = self.live.start();
            self.apply_live_result(result);
        }
        if stop_clicked {
            let result = self.live.stop();
            self.apply_live_result(result);
        }
        if record_clicked {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Scope recording", &["scope"])
                .set_file_name("capture.scope")
                .save_file()
            {
                let result = self.live.start_recording(&path);
                self.apply_live_result(result);
            }
        }
        if stop_record_clicked {
            let result = self.live.stop_recording();
            self.apply_live_result(result);
        }
        if open_recording_clicked {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Scope recording", &["scope"])
                .pick_file()
            {
                self.open_scope_recording(path);
            }
        }

        ui.separator();
        ui.label(format!("{:?}", self.live.connection_state));
        if self.live.is_recording() {
            ui.colored_label(Color32::from_rgb(210, 45, 45), "● REC");
        }
    }

    pub(super) fn live_workspace(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("live_channels")
            .resizable(true)
            .default_width(230.0)
            .show(ctx, |ui| self.live_channel_panel(ui));
        egui::SidePanel::right("live_trigger")
            .resizable(true)
            .default_width(280.0)
            .show(ctx, |ui| self.live_trigger_panel(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.live_plot_panel(ui));
    }

    fn live_channel_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading(self.live_text("实时通道", "Live channels"));
        ui.horizontal(|ui| {
            ui.label(self.live_text("采样率", "Sample rate"));
            ui.add(
                egui::DragValue::new(&mut self.live.acquisition.sample_rate_hz)
                    .clamp_range(1..=1_000_000)
                    .suffix(" Hz"),
            );
        });
        ui.horizontal(|ui| {
            ui.label(self.live_text("每帧点数", "Batch samples"));
            ui.add(
                egui::DragValue::new(&mut self.live.acquisition.batch_samples)
                    .clamp_range(1..=4096),
            );
        });
        ui.horizontal(|ui| {
            ui.label(self.live_text("历史", "History"));
            ui.add(
                egui::DragValue::new(&mut self.live.history_seconds)
                    .clamp_range(1..=300)
                    .suffix(" s"),
            );
        });
        ui.separator();
        let channels = self
            .live
            .channel_table
            .as_ref()
            .map(|table| table.channels.clone())
            .unwrap_or_default();
        if channels.is_empty() {
            ui.label(self.live_text(
                "连接后显示设备通道。",
                "Device channels appear after connection.",
            ));
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for channel in channels {
                ui.horizontal(|ui| {
                    let visible = self
                        .live
                        .channel_visibility
                        .entry(channel.channel_id)
                        .or_insert(true);
                    ui.checkbox(visible, &channel.name);
                    let rgba = self
                        .live
                        .channel_colors
                        .entry(channel.channel_id)
                        .or_insert([80, 140, 220, 255]);
                    let mut color =
                        Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
                    if egui::color_picker::color_edit_button_srgba(
                        ui,
                        &mut color,
                        egui::color_picker::Alpha::Opaque,
                    )
                    .changed()
                    {
                        *rgba = color.to_array();
                    }
                    if !channel.unit.is_empty() {
                        ui.label(RichText::new(channel.unit).small().color(Color32::GRAY));
                    }
                });
            }
        });
    }

    fn live_trigger_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading(self.live_text("触发", "Trigger"));
        let mut config = self.live.trigger.config().clone();
        let mut changed = false;
        egui::ComboBox::from_id_source("live_trigger_mode")
            .selected_text(format!("{:?}", config.mode))
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(&mut config.mode, TriggerMode::Auto, "Auto")
                    .changed();
                changed |= ui
                    .selectable_value(&mut config.mode, TriggerMode::Normal, "Normal")
                    .changed();
                changed |= ui
                    .selectable_value(&mut config.mode, TriggerMode::Single, "Single")
                    .changed();
            });
        egui::ComboBox::from_id_source("live_trigger_edge")
            .selected_text(format!("{:?}", config.edge))
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(&mut config.edge, TriggerEdge::Rising, "Rising")
                    .changed();
                changed |= ui
                    .selectable_value(&mut config.edge, TriggerEdge::Falling, "Falling")
                    .changed();
                changed |= ui
                    .selectable_value(&mut config.edge, TriggerEdge::Either, "Either")
                    .changed();
            });
        let source_name = self
            .live
            .channel_table
            .as_ref()
            .and_then(|table| table.channel(config.source_channel))
            .map(|channel| channel.name.as_str())
            .unwrap_or("-");
        egui::ComboBox::from_id_source("live_trigger_source")
            .selected_text(source_name)
            .show_ui(ui, |ui| {
                if let Some(table) = &self.live.channel_table {
                    for channel in &table.channels {
                        if self.live.acquisition.channel_mask & (1_u64 << channel.channel_id) != 0 {
                            changed |= ui
                                .selectable_value(
                                    &mut config.source_channel,
                                    channel.channel_id,
                                    &channel.name,
                                )
                                .changed();
                        }
                    }
                }
            });
        changed |= ui
            .add(
                egui::DragValue::new(&mut config.level)
                    .speed(0.01)
                    .prefix("Level "),
            )
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut config.hysteresis)
                    .clamp_range(0.0..=f32::MAX)
                    .speed(0.01)
                    .prefix("Hys "),
            )
            .changed();
        changed |= ui
            .add(egui::DragValue::new(&mut config.pre_samples).prefix("Pre "))
            .changed();
        changed |= ui
            .add(egui::DragValue::new(&mut config.post_samples).prefix("Post "))
            .changed();
        if changed {
            if let Err(error) = self.live.trigger.set_config(config) {
                self.live.last_error = Some(error.to_string());
            }
        }
        ui.horizontal(|ui| {
            if ui.button(self.live_text("重新布防", "Arm")).clicked() {
                self.live.trigger.arm();
            }
            ui.label(if self.live.trigger.is_armed() {
                self.live_text("已布防", "Armed")
            } else {
                self.live_text("已停止", "Disarmed")
            });
        });
        ui.separator();
        ui.heading(self.live_text("链路统计", "Link statistics"));
        egui::Grid::new("live_stats").show(ui, |ui| {
            ui.label(self.live_text("帧", "Frames"));
            ui.label(self.live.stats.received_frames.to_string());
            ui.end_row();
            ui.label(self.live_text("采样批次", "Batches"));
            ui.label(self.live.stats.received_batches.to_string());
            ui.end_row();
            ui.label(self.live_text("样点", "Samples"));
            ui.label(self.live.stats.received_samples.to_string());
            ui.end_row();
            ui.label(self.live_text("序号缺口", "Sequence gaps"));
            ui.label(self.live.stats.sequence_gaps.to_string());
            ui.end_row();
            ui.label(self.live_text("主机丢批", "Host drops"));
            ui.label(self.live.stats.host_dropped_batches.to_string());
            ui.end_row();
            ui.label(self.live_text("协议错误", "Protocol errors"));
            ui.label(self.live.stats.protocol_errors.to_string());
            ui.end_row();
        });
        if let Some(error) = &self.live.last_error {
            ui.separator();
            ui.colored_label(Color32::from_rgb(200, 50, 55), error);
        }
    }

    fn live_plot_panel(&mut self, ui: &mut egui::Ui) {
        let Some(snapshot) = self.live.display_snapshot(8_000) else {
            ui.centered_and_justified(|ui| {
                ui.label(self.live_text(
                    "连接并开始采集后显示实时波形。",
                    "Connect and start acquisition to display live waveforms.",
                ));
            });
            return;
        };
        let table = self.live.channel_table.clone();
        Plot::new("live_scope_plot")
            .legend(Legend::default())
            .allow_drag(true)
            .allow_zoom(true)
            .show(ui, |plot_ui| {
                for (channel_position, channel_id) in snapshot.channel_ids.iter().enumerate() {
                    if !self
                        .live
                        .channel_visibility
                        .get(channel_id)
                        .copied()
                        .unwrap_or(true)
                    {
                        continue;
                    }
                    let name = table
                        .as_ref()
                        .and_then(|table| table.channel(*channel_id))
                        .map(|channel| channel.name.clone())
                        .unwrap_or_else(|| format!("CH{channel_id}"));
                    let rgba = self
                        .live
                        .channel_colors
                        .get(channel_id)
                        .copied()
                        .unwrap_or([80, 140, 220, 255]);
                    let color = Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
                    for (segment_index, segment) in snapshot.segments.iter().enumerate() {
                        let Some(values) = segment.channels.get(channel_position) else {
                            continue;
                        };
                        let points = segment
                            .times
                            .iter()
                            .zip(values)
                            .map(|(time, value)| [*time, f64::from(*value)])
                            .collect::<Vec<_>>();
                        if points.is_empty() {
                            continue;
                        }
                        let line = Line::new(points).color(color).width(1.2);
                        if segment_index == 0 {
                            plot_ui.line(line.name(name.clone()));
                        } else {
                            plot_ui.line(line);
                        }
                    }
                }
            });
    }

    fn open_scope_recording(&mut self, path: PathBuf) {
        match scope_analyzer::live::scope_source::ScopeRecordingDataSource::open(&path) {
            Ok(source) => {
                let recent_path = path.clone();
                self.set_source(Arc::new(source), path, SourceKind::Scope);
                self.remember_recent_file(&recent_path);
                self.live.workspace_mode = WorkspaceMode::Offline;
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn apply_live_result(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.live.last_error = Some(error);
        }
    }

    fn live_text<'a>(&self, zh: &'a str, en: &'a str) -> &'a str {
        match self.language {
            Language::Zh => zh,
            Language::En => en,
        }
    }
}
