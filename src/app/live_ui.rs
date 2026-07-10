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
        let custom_baud_help = self
            .live_text(
                "可手工输入系统支持的波特率",
                "Enter any baud rate supported by the system",
            )
            .to_owned();
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
                ui.add_enabled_ui(disconnected, |ui| {
                    egui::ComboBox::from_id_source("live_serial_baud_preset")
                        .selected_text(format!("{baud} baud"))
                        .show_ui(ui, |ui| {
                            for preset in [
                                115_200_u32,
                                230_400,
                                460_800,
                                921_600,
                                1_500_000,
                                2_000_000,
                                3_000_000,
                            ] {
                                ui.selectable_value(baud, preset, preset.to_string());
                            }
                        });
                    ui.add(
                        egui::DragValue::new(baud)
                            .clamp_range(1..=4_000_000)
                            .speed(115_200),
                    )
                    .on_hover_text(&custom_baud_help);
                });
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
            .add_enabled(
                ready && self.live.configuration_applied,
                egui::Button::new(self.live_text("开始", "Start")),
            )
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
                self.live.configuration_applied
                    && (ready || streaming)
                    && !self.live.is_recording(),
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
        let can_edit_acquisition = matches!(
            self.live.connection_state,
            ConnectionState::Disconnected | ConnectionState::Ready
        );
        let mut acquisition_changed = false;
        ui.horizontal(|ui| {
            ui.label(self.live_text("采样率", "Sample rate"));
            let maximum = self
                .live
                .hello_ack
                .as_ref()
                .and_then(|hello| u32::try_from(hello.tick_hz).ok())
                .unwrap_or(1_000_000)
                .max(1);
            acquisition_changed |= ui
                .add_enabled(
                    can_edit_acquisition,
                    egui::DragValue::new(&mut self.live.acquisition.sample_rate_hz)
                        .clamp_range(1..=maximum)
                        .suffix(" Hz"),
                )
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label(self.live_text("每帧点数", "Batch samples"));
            let maximum = self
                .live
                .hello_ack
                .as_ref()
                .map(|hello| hello.max_batch_samples)
                .unwrap_or(4096)
                .max(1);
            acquisition_changed |= ui
                .add_enabled(
                    can_edit_acquisition,
                    egui::DragValue::new(&mut self.live.acquisition.batch_samples)
                        .clamp_range(1..=maximum),
                )
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label(self.live_text("历史", "History"));
            acquisition_changed |= ui
                .add_enabled(
                    can_edit_acquisition,
                    egui::DragValue::new(&mut self.live.history_seconds)
                        .clamp_range(1..=300)
                        .suffix(" s"),
                )
                .changed();
        });
        if acquisition_changed
            && !matches!(self.live.connection_state, ConnectionState::Disconnected)
        {
            self.live.configuration_applied = false;
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.live_text("采", "Acq")).small());
            ui.label(RichText::new(self.live_text("显示 / 名称", "Show / Name")).small());
            ui.label(RichText::new(self.live_text("倍率", "Scale")).small());
        });
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
                    let bit = 1_u64 << channel.channel_id;
                    let mut acquired = self.live.acquisition.channel_mask & bit != 0;
                    if ui
                        .add_enabled(
                            can_edit_acquisition,
                            egui::Checkbox::without_text(&mut acquired),
                        )
                        .on_hover_text(self.live_text(
                            "是否从设备采集该通道",
                            "Acquire this channel from the device",
                        ))
                        .changed()
                    {
                        let next_mask = if acquired {
                            self.live.acquisition.channel_mask | bit
                        } else {
                            self.live.acquisition.channel_mask & !bit
                        };
                        if next_mask == 0 {
                            self.live.last_error = Some(
                                self.live_text(
                                    "至少保留一个采集通道。",
                                    "At least one acquisition channel is required.",
                                )
                                .to_owned(),
                            );
                        } else {
                            self.live.acquisition.channel_mask = next_mask;
                            self.live.configuration_applied = false;
                        }
                    }
                    let visible = self
                        .live
                        .channel_visibility
                        .entry(channel.channel_id)
                        .or_insert(true);
                    ui.checkbox(visible, &channel.name);
                    let scale = self
                        .live
                        .channel_scales
                        .entry(channel.channel_id)
                        .or_insert(1.0);
                    ui.add(
                        egui::DragValue::new(scale)
                            .clamp_range(-1_000_000.0..=1_000_000.0)
                            .speed(0.1)
                            .prefix("×"),
                    );
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
        changed |= ui
            .add(
                egui::DragValue::new(&mut config.auto_timeout_samples)
                    .clamp_range(1..=usize::MAX)
                    .prefix("Auto timeout "),
            )
            .changed();
        if changed {
            if let Err(error) = self.live.set_trigger_config(config) {
                self.live.last_error = Some(error);
            }
        }
        ui.horizontal(|ui| {
            if ui.button(self.live_text("重新布防", "Arm")).clicked() {
                self.live.arm_trigger();
            }
            ui.label(if self.live.trigger.is_armed() {
                self.live_text("已布防", "Armed")
            } else {
                self.live_text("已停止", "Disarmed")
            });
        });
        if let Some(capture) = &self.live.last_capture {
            ui.label(format!(
                "{}: {}{}",
                self.live_text("触发样点", "Trigger sample"),
                capture
                    .sample_indices
                    .get(capture.trigger_position)
                    .copied()
                    .unwrap_or_default(),
                if capture.auto_timeout {
                    self.live_text("（Auto 超时）", " (Auto timeout)")
                } else {
                    ""
                }
            ));
        }
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
            ui.label("CRC");
            ui.label(self.live.stats.crc_errors.to_string());
            ui.end_row();
            ui.label(self.live_text("丢弃字节", "Discarded bytes"));
            ui.label(self.live.stats.discarded_bytes.to_string());
            ui.end_row();
            ui.label(self.live_text("未知消息", "Unknown messages"));
            ui.label(self.live.stats.unknown_messages.to_string());
            ui.end_row();
            ui.label(self.live_text("设备丢样", "Device drops"));
            ui.label(self.live.stats.device_dropped_samples.to_string());
            ui.end_row();
            ui.label(self.live_text("设备发送溢出", "Device TX overruns"));
            ui.label(self.live.stats.device_tx_overruns.to_string());
            ui.end_row();
        });
        ui.separator();
        ui.heading(self.live_text("录波统计", "Recording statistics"));
        let recording = self.live.recording_stats();
        egui::Grid::new("live_recording_stats").show(ui, |ui| {
            ui.label(self.live_text("已写记录", "Written records"));
            ui.label(recording.written_records.to_string());
            ui.end_row();
            ui.label(self.live_text("采样帧", "Sample frames"));
            ui.label(recording.sample_frames.to_string());
            ui.end_row();
            ui.label(self.live_text("Gap", "Gaps"));
            ui.label(recording.gap_records.to_string());
            ui.end_row();
            ui.label(self.live_text("触发", "Triggers"));
            ui.label(recording.trigger_records.to_string());
            ui.end_row();
            ui.label(self.live_text("排队记录", "Pending records"));
            ui.label(self.live.recording_pending_records().to_string());
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
                            .map(|(time, value)| {
                                [
                                    *time,
                                    f64::from(self.live.scaled_display_value(*channel_id, *value)),
                                ]
                            })
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
