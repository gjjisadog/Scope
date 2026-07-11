use super::*;
use scope_analyzer::live::{
    protocol::{ChannelDescriptor, ChannelKind},
    session::ConnectionState,
    state::WorkspaceMode,
    transport::TransportConfig,
    trigger::{TriggerEdge, TriggerMode},
};

const LIVE_SIGNAL_PANEL_WIDTH: f32 = 250.0;
const LIVE_INSPECTOR_PANEL_WIDTH: f32 = 300.0;
const LIVE_BOTTOM_PANEL_HEIGHT: f32 = 168.0;
const LIVE_CHANNEL_LABEL_WIDTH: f32 = 92.0;
const LIVE_PLOT_ROW_MIN_HEIGHT: f32 = 92.0;
const LIVE_PLOT_ROW_MAX_HEIGHT: f32 = 180.0;

impl ScopeApp {
    pub(super) fn workspace_selector(&mut self, ui: &mut egui::Ui) {
        let offline_label = self.live_text("离线分析", "Offline analysis");
        let live_label = self.live_text("实时采集", "Live capture");
        let mut changed = ui
            .selectable_value(
                &mut self.live.workspace_mode,
                WorkspaceMode::Offline,
                offline_label,
            )
            .changed();
        changed |= ui
            .selectable_value(
                &mut self.live.workspace_mode,
                WorkspaceMode::Live,
                live_label,
            )
            .changed();
        if changed
            && self.live.workspace_mode == WorkspaceMode::Live
            && self.live.serial_ports.is_empty()
        {
            if let Err(error) = self.live.refresh_serial_ports() {
                self.live.last_error = Some(error);
            }
        }
    }

    pub(super) fn live_toolbar(&mut self, ui: &mut egui::Ui) {
        let disconnected = self.live.connection_state == ConnectionState::Disconnected;
        let ready = self.live.connection_state == ConnectionState::Ready;
        let streaming = self.live.connection_state == ConnectionState::Streaming;
        let recording = self.live.is_recording();

        let connection_label = match &self.live.transport {
            TransportConfig::Tcp { address } => format!("TCP · {address}"),
            TransportConfig::Serial { port, baud } => {
                let port = if port.is_empty() { "-" } else { port.as_str() };
                format!("Serial · {port} · {baud}")
            }
        };
        ui.menu_button(connection_label, |ui| self.live_connection_menu(ui));

        let state_color = match self.live.connection_state {
            ConnectionState::Ready | ConnectionState::Streaming => Color32::from_rgb(67, 190, 115),
            ConnectionState::Connecting
            | ConnectionState::Handshaking
            | ConnectionState::Configuring => Color32::from_rgb(226, 170, 55),
            ConnectionState::Disconnected => Color32::from_gray(130),
        };
        ui.colored_label(
            state_color,
            format!("● {}", self.live_connection_state_text()),
        );

        ui.separator();
        let acquisition_summary = format!(
            "{} kS/s · {}/frame · {} s",
            Self::compact_rate(self.live.acquisition.sample_rate_hz),
            self.live.acquisition.batch_samples,
            self.live.history_seconds
        );
        if ui.button(acquisition_summary).clicked() {
            self.live_show_inspector_panel = true;
            self.live_inspector_tab = 1;
        }

        ui.separator();
        let start_label = if self.live.configuration_applied {
            self.live_text("开始", "Start")
        } else {
            self.live_text("应用并开始", "Configure & Start")
        };
        if ui
            .add_enabled(ready, egui::Button::new(start_label))
            .on_hover_text(self.live_text(
                "应用当前采集参数并开始实时采集",
                "Apply the current acquisition settings and start streaming",
            ))
            .clicked()
        {
            let configure = self.live.acquisition.clone();
            let result = self.live.start_with_configuration(configure);
            self.apply_live_result(result);
        }
        if ui
            .add_enabled(streaming, egui::Button::new(self.live_text("停止", "Stop")))
            .clicked()
        {
            let result = self.live.stop();
            self.apply_live_result(result);
        }

        let mut paused = self.live.display_paused;
        if ui
            .add_enabled(
                streaming,
                egui::Checkbox::new(&mut paused, self.live_text("暂停显示", "Pause display")),
            )
            .changed()
        {
            self.live.set_display_paused(paused);
        }

        let record_label = if recording {
            self.live_text("结束录波", "Finish recording")
        } else {
            self.live_text("录波", "Record")
        };
        let record_button = egui::Button::new(record_label).fill(if recording {
            Color32::from_rgb(176, 48, 52)
        } else {
            ui.visuals().widgets.inactive.bg_fill
        });
        if ui
            .add_enabled(
                recording || (streaming && self.live.configuration_applied),
                record_button,
            )
            .clicked()
        {
            if recording {
                let result = self.live.stop_recording();
                self.apply_live_result(result);
            } else if let Some(path) = rfd::FileDialog::new()
                .add_filter("Scope recording", &["scope"])
                .set_file_name("capture.scope")
                .save_file()
            {
                let result = self.live.start_recording(&path);
                self.apply_live_result(result);
            }
        }

        if ui
            .selectable_label(
                self.live_show_signal_panel,
                self.live_text("信号", "Signals"),
            )
            .clicked()
        {
            self.live_show_signal_panel = !self.live_show_signal_panel;
        }
        if ui
            .selectable_label(
                self.live_show_inspector_panel,
                self.live_text("检查器", "Inspector"),
            )
            .clicked()
        {
            self.live_show_inspector_panel = !self.live_show_inspector_panel;
        }
        if ui
            .selectable_label(
                self.live_show_bottom_panel,
                self.live_text("事件", "Events"),
            )
            .clicked()
        {
            self.live_show_bottom_panel = !self.live_show_bottom_panel;
        }

        if disconnected && ui.button(self.live_text("连接", "Connect")).clicked() {
            let result = self.live.connect();
            self.apply_live_result(result);
        } else if !disconnected && ui.button(self.live_text("断开", "Disconnect")).clicked() {
            let result = self.live.disconnect();
            self.apply_live_result(result);
        }
    }

    fn live_connection_menu(&mut self, ui: &mut egui::Ui) {
        let disconnected = self.live.connection_state == ConnectionState::Disconnected;
        ui.set_min_width(300.0);
        ui.label(RichText::new(self.live_text("连接设置", "Connection settings")).strong());
        ui.add_space(4.0);

        let mut use_tcp = matches!(self.live.transport, TransportConfig::Tcp { .. });
        let previous_use_tcp = use_tcp;
        ui.horizontal(|ui| {
            ui.radio_value(&mut use_tcp, true, "TCP");
            ui.radio_value(&mut use_tcp, false, self.live_text("串口", "Serial"));
        });
        if use_tcp != previous_use_tcp && disconnected {
            self.live.transport = if use_tcp {
                TransportConfig::Tcp {
                    address: "127.0.0.1:19090".to_owned(),
                }
            } else {
                TransportConfig::Serial {
                    port: self.live.serial_ports.first().cloned().unwrap_or_default(),
                    baud: 115_200,
                }
            };
        }

        let serial_ports = self.live.serial_ports.clone();
        let select_port_label = self.live_text("选择串口", "Select port");
        match &mut self.live.transport {
            TransportConfig::Tcp { address } => {
                ui.add_enabled(
                    disconnected,
                    egui::TextEdit::singleline(address).desired_width(f32::INFINITY),
                );
            }
            TransportConfig::Serial { port, baud } => {
                ui.add_enabled_ui(disconnected, |ui| {
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
                    ui.horizontal(|ui| {
                        ui.label("Baud");
                        ui.add(
                            egui::DragValue::new(baud)
                                .clamp_range(1..=4_000_000)
                                .speed(115_200),
                        );
                    });
                });
                if ui
                    .add_enabled(
                        disconnected,
                        egui::Button::new(self.live_text("刷新串口", "Refresh ports")),
                    )
                    .clicked()
                {
                    if let Err(error) = self.live.refresh_serial_ports() {
                        self.live.last_error = Some(error);
                    }
                }
            }
        }
    }

    pub(super) fn live_workspace(&mut self, ctx: &egui::Context) {
        self.apply_live_workspace_visuals(ctx);

        egui::TopBottomPanel::top("live_document_tabs")
            .exact_height(34.0)
            .frame(egui::Frame::none().fill(Color32::from_rgb(25, 31, 36)))
            .show(ctx, |ui| self.live_document_tabs(ui));

        if self.live_show_bottom_panel {
            egui::TopBottomPanel::bottom("live_bottom_dock")
                .resizable(true)
                .default_height(LIVE_BOTTOM_PANEL_HEIGHT)
                .height_range(108.0..=300.0)
                .show(ctx, |ui| self.live_bottom_dock(ui));
        }
        if self.live_show_signal_panel {
            egui::SidePanel::left("live_signals_dock")
                .resizable(true)
                .default_width(LIVE_SIGNAL_PANEL_WIDTH)
                .width_range(190.0..=360.0)
                .show(ctx, |ui| self.live_signal_panel(ui));
        }
        if self.live_show_inspector_panel {
            egui::SidePanel::right("live_inspector_dock")
                .resizable(true)
                .default_width(LIVE_INSPECTOR_PANEL_WIDTH)
                .width_range(250.0..=420.0)
                .show(ctx, |ui| self.live_inspector_panel(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::from_rgb(13, 18, 22)))
            .show(ctx, |ui| self.live_plot_panel(ui));
    }

    fn apply_live_workspace_visuals(&self, ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(22, 28, 33);
        visuals.window_fill = Color32::from_rgb(22, 28, 33);
        visuals.extreme_bg_color = Color32::from_rgb(12, 17, 21);
        visuals.faint_bg_color = Color32::from_rgb(28, 35, 41);
        visuals.selection.bg_fill = Color32::from_rgb(22, 126, 143);
        visuals.selection.stroke = Stroke::new(1.0_f32, Color32::from_rgb(98, 210, 219));
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(31, 39, 45);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(39, 51, 58);
        visuals.widgets.active.bg_fill = Color32::from_rgb(24, 111, 126);
        visuals.widgets.noninteractive.bg_stroke =
            Stroke::new(1.0_f32, Color32::from_rgb(52, 62, 69));
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(55, 66, 73));
        ctx.set_visuals(visuals);
    }

    fn live_document_tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let device = self
                .live
                .hello_ack
                .as_ref()
                .map(|hello| hello.firmware_name.as_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("Live Scope");
            let _ = ui.selectable_label(true, format!("Live — {device}"));
            ui.separator();
            if ui
                .button("+")
                .on_hover_text(self.live_text("打开录波", "Open recording"))
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Scope recording", &["scope"])
                    .pick_file()
                {
                    self.open_scope_recording(path);
                }
            }

            let latest_recording = self.live.recording_path.clone();
            if let Some(path) = latest_recording {
                let label = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Recording");
                if ui
                    .selectable_label(false, label)
                    .on_hover_text(self.live_text(
                        "在离线分析工作区打开此录波",
                        "Open this recording in the offline analysis workspace",
                    ))
                    .clicked()
                    && path.is_file()
                {
                    self.open_scope_recording(path);
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(self.live_text("布局：Power Debug", "Layout: Power Debug"));
            });
        });
    }

    fn live_signal_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(self.live_text("信号", "Signals"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("×")
                    .on_hover_text(self.live_text("隐藏信号面板", "Hide signals panel"))
                    .clicked()
                {
                    self.live_show_signal_panel = false;
                }
            });
        });
        let search_hint = self.live_text("搜索信号", "Search signals");
        ui.add(
            egui::TextEdit::singleline(&mut self.live_channel_filter)
                .hint_text(search_hint)
                .desired_width(f32::INFINITY),
        );

        let channels = self
            .live
            .channel_table
            .as_ref()
            .map(|table| table.channels.clone())
            .unwrap_or_default();
        let acquired = channels
            .iter()
            .filter(|channel| {
                self.live.acquisition.channel_mask & (1_u64 << channel.channel_id) != 0
            })
            .count();
        let visible = channels
            .iter()
            .filter(|channel| {
                self.live
                    .channel_visibility
                    .get(&channel.channel_id)
                    .copied()
                    .unwrap_or(true)
            })
            .count();
        ui.label(
            RichText::new(format!(
                "{} {acquired} / {} {visible}",
                self.live_text("采集", "Acquire"),
                self.live_text("显示", "Visible")
            ))
            .small()
            .color(Color32::from_gray(160)),
        );
        ui.separator();

        if channels.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.label(self.live_text(
                    "连接设备后，这里会显示通道表。",
                    "Connect a device to load its channel table.",
                ));
            });
            return;
        }

        let filter = self.live_channel_filter.trim().to_lowercase();
        let mut analog = Vec::new();
        let mut digital = Vec::new();
        for channel in channels {
            if !filter.is_empty()
                && !channel.name.to_lowercase().contains(&filter)
                && !channel.unit.to_lowercase().contains(&filter)
            {
                continue;
            }
            match channel.kind {
                ChannelKind::Analog => analog.push(channel),
                ChannelKind::Digital => digital.push(channel),
            }
        }
        let latest = self.live_latest_values();
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.live_channel_group(
                ui,
                self.live_text("模拟量", "Analog"),
                analog,
                &latest,
                true,
            );
            self.live_channel_group(
                ui,
                self.live_text("数字量", "Digital"),
                digital,
                &latest,
                false,
            );
        });
    }

    fn live_channel_group(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        channels: Vec<ChannelDescriptor>,
        latest: &std::collections::BTreeMap<u16, f32>,
        default_open: bool,
    ) {
        let shown = channels
            .iter()
            .filter(|channel| {
                self.live
                    .channel_visibility
                    .get(&channel.channel_id)
                    .copied()
                    .unwrap_or(true)
            })
            .count();
        egui::CollapsingHeader::new(format!("{title} ({shown}/{})", channels.len()))
            .default_open(default_open)
            .show(ui, |ui| {
                if channels.is_empty() {
                    ui.label(
                        RichText::new(self.live_text("无匹配通道", "No matching signals")).small(),
                    );
                }
                for channel in channels {
                    self.live_channel_row(ui, &channel, latest.get(&channel.channel_id).copied());
                }
            });
    }

    fn live_channel_row(
        &mut self,
        ui: &mut egui::Ui,
        channel: &ChannelDescriptor,
        latest: Option<f32>,
    ) {
        let can_edit_acquisition = matches!(
            self.live.connection_state,
            ConnectionState::Disconnected | ConnectionState::Ready
        );
        let bit = 1_u64 << channel.channel_id;
        let mut acquired = self.live.acquisition.channel_mask & bit != 0;
        let mut visible = self
            .live
            .channel_visibility
            .get(&channel.channel_id)
            .copied()
            .unwrap_or(true);
        let mut scale = self
            .live
            .channel_scales
            .get(&channel.channel_id)
            .copied()
            .unwrap_or(1.0);
        let rgba = self
            .live
            .channel_colors
            .get(&channel.channel_id)
            .copied()
            .unwrap_or([80, 140, 220, 255]);
        let mut color = Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);

        ui.push_id(("live_channel_row", channel.channel_id), |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        can_edit_acquisition,
                        egui::Checkbox::without_text(&mut acquired),
                    )
                    .on_hover_text(self.live_text("从设备采集", "Acquire from device"))
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
                if ui
                    .checkbox(&mut visible, "")
                    .on_hover_text(self.live_text("显示波形", "Show waveform"))
                    .changed()
                {
                    self.live
                        .channel_visibility
                        .insert(channel.channel_id, visible);
                }
                if egui::color_picker::color_edit_button_srgba(
                    ui,
                    &mut color,
                    egui::color_picker::Alpha::Opaque,
                )
                .changed()
                {
                    self.live
                        .channel_colors
                        .insert(channel.channel_id, color.to_array());
                }
                ui.label(RichText::new(&channel.name).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let value = latest
                        .map(|value| format!("{value:.3} {}", channel.unit))
                        .unwrap_or_else(|| channel.unit.clone());
                    ui.label(RichText::new(value).small().color(Color32::from_gray(170)));
                });
            });
            ui.horizontal(|ui| {
                ui.add_space(50.0);
                ui.label(RichText::new(self.live_text("倍率", "Scale")).small());
                if ui
                    .add(
                        egui::DragValue::new(&mut scale)
                            .clamp_range(-1_000_000.0..=1_000_000.0)
                            .speed(0.1)
                            .prefix("×"),
                    )
                    .changed()
                {
                    self.live.channel_scales.insert(channel.channel_id, scale);
                }
            });
            ui.separator();
        });
    }

    fn live_inspector_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (index, zh, en) in [
                (0, "触发", "Trigger"),
                (1, "显示", "Display"),
                (2, "诊断", "Diagnostics"),
            ] {
                let label = match self.language {
                    Language::Zh => zh,
                    Language::En => en,
                };
                ui.selectable_value(&mut self.live_inspector_tab, index, label);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("×")
                    .on_hover_text(self.live_text("隐藏检查器", "Hide inspector"))
                    .clicked()
                {
                    self.live_show_inspector_panel = false;
                }
            });
        });
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| match self.live_inspector_tab {
            1 => self.live_display_inspector(ui),
            2 => self.live_diagnostics_inspector(ui),
            _ => self.live_trigger_inspector(ui),
        });
    }

    fn live_trigger_inspector(&mut self, ui: &mut egui::Ui) {
        let mut config = self.live.trigger.config().clone();
        let mut changed = false;
        ui.label(RichText::new(self.live_text("触发模式", "Trigger mode")).strong());
        ui.horizontal(|ui| {
            changed |= ui
                .selectable_value(
                    &mut config.mode,
                    TriggerMode::Auto,
                    self.live_text("自动", "Auto"),
                )
                .changed();
            changed |= ui
                .selectable_value(
                    &mut config.mode,
                    TriggerMode::Normal,
                    self.live_text("普通", "Normal"),
                )
                .changed();
            changed |= ui
                .selectable_value(
                    &mut config.mode,
                    TriggerMode::Single,
                    self.live_text("单次", "Single"),
                )
                .changed();
        });
        ui.add_space(8.0);

        egui::Grid::new("live_trigger_form")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label(self.live_text("源", "Source"));
                let source_name = self
                    .live
                    .channel_table
                    .as_ref()
                    .and_then(|table| table.channel(config.source_channel))
                    .map(|channel| channel.name.as_str())
                    .unwrap_or("-");
                egui::ComboBox::from_id_source("live_trigger_source")
                    .selected_text(source_name)
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        if let Some(table) = &self.live.channel_table {
                            for channel in &table.channels {
                                if self.live.acquisition.channel_mask
                                    & (1_u64 << channel.channel_id)
                                    != 0
                                {
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
                ui.end_row();

                ui.label(self.live_text("边沿", "Edge"));
                egui::ComboBox::from_id_source("live_trigger_edge")
                    .selected_text(match config.edge {
                        TriggerEdge::Rising => self.live_text("上升沿", "Rising"),
                        TriggerEdge::Falling => self.live_text("下降沿", "Falling"),
                        TriggerEdge::Either => self.live_text("双边沿", "Either"),
                    })
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        changed |= ui
                            .selectable_value(
                                &mut config.edge,
                                TriggerEdge::Rising,
                                self.live_text("上升沿", "Rising"),
                            )
                            .changed();
                        changed |= ui
                            .selectable_value(
                                &mut config.edge,
                                TriggerEdge::Falling,
                                self.live_text("下降沿", "Falling"),
                            )
                            .changed();
                        changed |= ui
                            .selectable_value(
                                &mut config.edge,
                                TriggerEdge::Either,
                                self.live_text("双边沿", "Either"),
                            )
                            .changed();
                    });
                ui.end_row();

                ui.label(self.live_text("电平", "Level"));
                changed |= ui
                    .add(egui::DragValue::new(&mut config.level).speed(0.01))
                    .changed();
                ui.end_row();

                ui.label(self.live_text("回差", "Hysteresis"));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut config.hysteresis)
                            .clamp_range(0.0..=f32::MAX)
                            .speed(0.01),
                    )
                    .changed();
                ui.end_row();

                ui.label(self.live_text("前置样点", "Pre samples"));
                changed |= ui
                    .add(egui::DragValue::new(&mut config.pre_samples))
                    .changed();
                ui.end_row();

                ui.label(self.live_text("后置样点", "Post samples"));
                changed |= ui
                    .add(egui::DragValue::new(&mut config.post_samples))
                    .changed();
                ui.end_row();

                ui.label(self.live_text("Auto 超时", "Auto timeout"));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut config.auto_timeout_samples)
                            .clamp_range(1..=usize::MAX),
                    )
                    .changed();
                ui.end_row();
            });

        if changed {
            if let Err(error) = self.live.set_trigger_config(config) {
                self.live.last_error = Some(error);
            }
        }
        ui.add_space(12.0);
        let armed = self.live.trigger.is_armed();
        ui.horizontal(|ui| {
            ui.colored_label(
                if armed {
                    Color32::from_rgb(67, 190, 115)
                } else {
                    Color32::from_gray(145)
                },
                if armed {
                    self.live_text("● 已布防", "● Armed")
                } else {
                    self.live_text("● 未布防", "● Disarmed")
                },
            );
        });
        if ui
            .add_sized(
                [ui.available_width(), 34.0],
                egui::Button::new(self.live_text("Arm 触发", "Arm trigger"))
                    .fill(Color32::from_rgb(20, 116, 132)),
            )
            .clicked()
        {
            self.live.arm_trigger();
        }
        if let Some(capture) = &self.live.last_capture {
            ui.add_space(8.0);
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
    }

    fn live_display_inspector(&mut self, ui: &mut egui::Ui) {
        let can_edit = matches!(
            self.live.connection_state,
            ConnectionState::Disconnected | ConnectionState::Ready
        );
        let mut changed = false;
        ui.label(RichText::new(self.live_text("采集设置", "Acquisition")).strong());
        egui::Grid::new("live_acquisition_form")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label(self.live_text("采样率", "Sample rate"));
                let maximum = self
                    .live
                    .hello_ack
                    .as_ref()
                    .and_then(|hello| u32::try_from(hello.tick_hz).ok())
                    .unwrap_or(1_000_000)
                    .max(1);
                changed |= ui
                    .add_enabled(
                        can_edit,
                        egui::DragValue::new(&mut self.live.acquisition.sample_rate_hz)
                            .clamp_range(1..=maximum)
                            .suffix(" Hz"),
                    )
                    .changed();
                ui.end_row();

                ui.label(self.live_text("每帧点数", "Batch samples"));
                let maximum = self
                    .live
                    .hello_ack
                    .as_ref()
                    .map(|hello| hello.max_batch_samples)
                    .unwrap_or(4096)
                    .max(1);
                changed |= ui
                    .add_enabled(
                        can_edit,
                        egui::DragValue::new(&mut self.live.acquisition.batch_samples)
                            .clamp_range(1..=maximum),
                    )
                    .changed();
                ui.end_row();

                ui.label(self.live_text("历史", "History"));
                changed |= ui
                    .add_enabled(
                        can_edit,
                        egui::DragValue::new(&mut self.live.history_seconds)
                            .clamp_range(1..=300)
                            .suffix(" s"),
                    )
                    .changed();
                ui.end_row();
            });
        if changed && self.live.connection_state != ConnectionState::Disconnected {
            self.live.configuration_applied = false;
        }

        ui.add_space(12.0);
        ui.label(RichText::new(self.live_text("工作区", "Workspace")).strong());
        let signal_panel_label = self.live_text("显示信号面板", "Show signals panel");
        ui.checkbox(&mut self.live_show_signal_panel, signal_panel_label);
        let bottom_panel_label = self.live_text("显示事件面板", "Show events panel");
        ui.checkbox(&mut self.live_show_bottom_panel, bottom_panel_label);
        let mut paused = self.live.display_paused;
        if ui
            .checkbox(
                &mut paused,
                self.live_text("暂停波形显示", "Pause waveform display"),
            )
            .changed()
        {
            self.live.set_display_paused(paused);
        }
    }

    fn live_diagnostics_inspector(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(self.live_text("链路健康", "Link health")).strong());
        self.live_stats_grid(ui, false);
        ui.separator();
        ui.label(RichText::new(self.live_text("录波", "Recording")).strong());
        let recording = self.live.recording_stats();
        egui::Grid::new("live_recording_stats_compact")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                for (label, value) in [
                    (
                        self.live_text("已写记录", "Written records"),
                        recording.written_records,
                    ),
                    (
                        self.live_text("采样帧", "Sample frames"),
                        recording.sample_frames,
                    ),
                    (self.live_text("Gap", "Gaps"), recording.gap_records),
                    (
                        self.live_text("触发", "Triggers"),
                        recording.trigger_records,
                    ),
                ] {
                    ui.label(label);
                    ui.label(value.to_string());
                    ui.end_row();
                }
                ui.label(self.live_text("排队记录", "Pending records"));
                ui.label(self.live.recording_pending_records().to_string());
                ui.end_row();
            });
        if let Some(path) = &self.live.recording_path {
            ui.label(
                RichText::new(path.display().to_string())
                    .small()
                    .color(Color32::from_gray(160)),
            );
        }
        if let Some(error) = &self.live.last_error {
            ui.separator();
            ui.colored_label(Color32::from_rgb(235, 90, 96), error);
        }
    }

    fn live_bottom_dock(&mut self, ui: &mut egui::Ui) {
        let events_label = self.live_text("事件", "Events");
        let link_label = self.live_text("链路", "Link");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.live_bottom_tab, 0, events_label);
            ui.selectable_value(&mut self.live_bottom_tab, 1, link_label);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("×")
                    .on_hover_text(self.live_text("隐藏底部面板", "Hide bottom panel"))
                    .clicked()
                {
                    self.live_show_bottom_panel = false;
                }
            });
        });
        ui.separator();
        if self.live_bottom_tab == 1 {
            self.live_stats_grid(ui, true);
        } else {
            self.live_event_table(ui);
        }
    }

    fn live_event_table(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("live_event_table")
                .num_columns(3)
                .striped(true)
                .min_col_width(110.0)
                .spacing([18.0, 6.0])
                .show(ui, |ui| {
                    ui.strong(self.live_text("阶段", "Stage"));
                    ui.strong(self.live_text("状态", "Status"));
                    ui.strong(self.live_text("详情", "Details"));
                    ui.end_row();

                    ui.label(self.live_text("连接", "Connection"));
                    ui.label(self.live_connection_state_text());
                    ui.label(match &self.live.transport {
                        TransportConfig::Tcp { address } => format!("TCP {address}"),
                        TransportConfig::Serial { port, baud } => format!("{port} · {baud} baud"),
                    });
                    ui.end_row();

                    ui.label(self.live_text("采集配置", "Acquisition"));
                    ui.label(if self.live.configuration_applied {
                        self.live_text("已应用", "Applied")
                    } else {
                        self.live_text("待应用", "Pending")
                    });
                    ui.label(format!(
                        "{} Hz · {}/frame · {} s",
                        self.live.acquisition.sample_rate_hz,
                        self.live.acquisition.batch_samples,
                        self.live.history_seconds
                    ));
                    ui.end_row();

                    ui.label(self.live_text("触发", "Trigger"));
                    ui.label(if self.live.trigger.is_armed() {
                        self.live_text("已布防", "Armed")
                    } else {
                        self.live_text("未布防", "Disarmed")
                    });
                    let trigger = self.live.trigger.config();
                    ui.label(format!(
                        "{:?} · CH{} · {:?}",
                        trigger.mode, trigger.source_channel, trigger.edge
                    ));
                    ui.end_row();

                    ui.label(self.live_text("录波", "Recording"));
                    ui.label(if self.live.is_recording() {
                        "REC"
                    } else {
                        self.live_text("未录制", "Idle")
                    });
                    ui.label(
                        self.live
                            .recording_path
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "-".to_owned()),
                    );
                    ui.end_row();
                });
        });
    }

    fn live_stats_grid(&self, ui: &mut egui::Ui, wide: bool) {
        let stats = self.live.stats;
        let metrics = [
            (self.live_text("帧", "Frames"), stats.received_frames),
            (
                self.live_text("采样批次", "Batches"),
                stats.received_batches,
            ),
            (self.live_text("样点", "Samples"), stats.received_samples),
            ("CRC", stats.crc_errors),
            (
                self.live_text("序号缺口", "Sequence gaps"),
                stats.sequence_gaps,
            ),
            (
                self.live_text("主机丢批", "Host drops"),
                stats.host_dropped_batches,
            ),
            (
                self.live_text("协议错误", "Protocol errors"),
                stats.protocol_errors,
            ),
            (
                self.live_text("设备丢样", "Device drops"),
                stats.device_dropped_samples,
            ),
            (
                self.live_text("发送溢出", "TX overruns"),
                stats.device_tx_overruns,
            ),
        ];
        let columns = if wide { 6 } else { 2 };
        egui::Grid::new(("live_link_stats", wide))
            .num_columns(columns)
            .spacing([14.0, 8.0])
            .show(ui, |ui| {
                for (index, (label, value)) in metrics.into_iter().enumerate() {
                    ui.label(RichText::new(label).small().color(Color32::from_gray(160)));
                    let bad = matches!(index, 3..=8) && value > 0;
                    ui.colored_label(
                        if bad {
                            Color32::from_rgb(232, 170, 58)
                        } else {
                            Color32::from_rgb(90, 204, 137)
                        },
                        value.to_string(),
                    );
                    if !wide || (index + 1) % 3 == 0 {
                        ui.end_row();
                    }
                }
            });
    }

    fn live_plot_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(format!(
                "{}  {} Hz · {} s",
                self.live_text("时基", "Timebase"),
                self.live.acquisition.sample_rate_hz,
                self.live.history_seconds
            ));
            ui.separator();
            ui.label(format!(
                "{}: {:?}",
                self.live_text("触发", "Trigger"),
                self.live.trigger.config().mode
            ));
            if self.live.display_paused {
                ui.colored_label(
                    Color32::from_rgb(232, 170, 58),
                    self.live_text("显示已暂停", "Display paused"),
                );
            }
        });
        ui.separator();

        let Some(snapshot) = self.live.display_snapshot(8_000) else {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading(self.live_text("等待实时波形", "Waiting for live waveforms"));
                    ui.label(self.live_text(
                        "连接设备、应用采集参数并开始后，通道会在此分轨显示。",
                        "Connect, apply acquisition settings, and start to show linked signal lanes.",
                    ));
                    ui.add_space(8.0);
                    if self.live.connection_state == ConnectionState::Disconnected
                        && ui.button(self.live_text("连接设备", "Connect device")).clicked()
                    {
                        let result = self.live.connect();
                        self.apply_live_result(result);
                    }
                });
            });
            return;
        };

        let table = self.live.channel_table.clone();
        let visible_channels = snapshot
            .channel_ids
            .iter()
            .copied()
            .filter(|channel_id| {
                self.live
                    .channel_visibility
                    .get(channel_id)
                    .copied()
                    .unwrap_or(true)
            })
            .collect::<Vec<_>>();
        if visible_channels.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(self.live_text(
                    "没有显示中的通道。请在信号面板中启用通道。",
                    "No visible signals. Enable one in the Signals panel.",
                ));
            });
            return;
        }

        let trigger_time = self.live_trigger_time();
        let rows = visible_channels.len().max(1) as f32;
        let row_height = ((ui.available_height() - 18.0) / rows)
            .clamp(LIVE_PLOT_ROW_MIN_HEIGHT, LIVE_PLOT_ROW_MAX_HEIGHT);
        let last_visible = visible_channels.last().copied();
        let background = Color32::from_rgb(13, 18, 22);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for channel_id in visible_channels {
                    let channel_position = snapshot
                        .channel_ids
                        .iter()
                        .position(|candidate| *candidate == channel_id);
                    let Some(channel_position) = channel_position else {
                        continue;
                    };
                    let descriptor = table
                        .as_ref()
                        .and_then(|table| table.channel(channel_id))
                        .cloned();
                    let name = descriptor
                        .as_ref()
                        .map(|channel| channel.name.as_str())
                        .unwrap_or("-");
                    let unit = descriptor
                        .as_ref()
                        .map(|channel| channel.unit.as_str())
                        .unwrap_or("");
                    let rgba = self
                        .live
                        .channel_colors
                        .get(&channel_id)
                        .copied()
                        .unwrap_or([80, 140, 220, 255]);
                    let color = Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
                    let latest = snapshot
                        .segments
                        .last()
                        .and_then(|segment| segment.channels.get(channel_position))
                        .and_then(|values| values.last())
                        .copied()
                        .map(|value| self.live.scaled_display_value(channel_id, value));

                    egui::Frame::none().fill(background).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(LIVE_CHANNEL_LABEL_WIDTH, row_height),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.colored_label(color, RichText::new(name).strong());
                                    if !unit.is_empty() {
                                        ui.label(
                                            RichText::new(unit)
                                                .small()
                                                .color(Color32::from_gray(150)),
                                        );
                                    }
                                    ui.label(
                                        RichText::new(format!(
                                            "×{:.3}",
                                            self.live
                                                .channel_scales
                                                .get(&channel_id)
                                                .copied()
                                                .unwrap_or(1.0)
                                        ))
                                        .small()
                                        .color(Color32::from_gray(150)),
                                    );
                                    if let Some(value) = latest {
                                        ui.label(
                                            RichText::new(format!("{value:.3}"))
                                                .small()
                                                .color(color),
                                        );
                                    }
                                },
                            );
                            let is_last = Some(channel_id) == last_visible;
                            Plot::new(("live_scope_lane", channel_id))
                                .height(row_height)
                                .allow_drag(true)
                                .allow_zoom(true)
                                .allow_scroll(false)
                                .show_x(is_last)
                                .show_y(true)
                                .show_grid([true, true])
                                .link_axis("live_scope_linked_x", true, false)
                                .link_cursor("live_scope_linked_cursor", true, false)
                                .legend(Legend::default().position(egui_plot::Corner::LeftTop))
                                .show(ui, |plot_ui| {
                                    for (segment_index, segment) in
                                        snapshot.segments.iter().enumerate()
                                    {
                                        let Some(values) = segment.channels.get(channel_position)
                                        else {
                                            continue;
                                        };
                                        let points =
                                            segment
                                                .times
                                                .iter()
                                                .zip(values)
                                                .map(|(time, value)| {
                                                    [
                                                        *time,
                                                        f64::from(self.live.scaled_display_value(
                                                            channel_id, *value,
                                                        )),
                                                    ]
                                                })
                                                .collect::<Vec<_>>();
                                        if points.is_empty() {
                                            continue;
                                        }
                                        let line = Line::new(points).color(color).width(1.4_f32);
                                        if segment_index == 0 {
                                            plot_ui.line(line.name(name));
                                        } else {
                                            plot_ui.line(line);
                                        }
                                    }
                                    if let Some(trigger_time) = trigger_time {
                                        plot_ui.vline(
                                            VLine::new(trigger_time)
                                                .color(Color32::from_rgb(232, 170, 58))
                                                .width(1.5_f32),
                                        );
                                    }
                                });
                        });
                    });
                }
            });
    }

    fn live_latest_values(&self) -> std::collections::BTreeMap<u16, f32> {
        let mut values = std::collections::BTreeMap::new();
        let Some(snapshot) = self.live.display_snapshot(64) else {
            return values;
        };
        let Some(segment) = snapshot.segments.last() else {
            return values;
        };
        for (position, channel_id) in snapshot.channel_ids.iter().copied().enumerate() {
            if let Some(value) = segment
                .channels
                .get(position)
                .and_then(|channel| channel.last())
                .copied()
            {
                values.insert(
                    channel_id,
                    self.live.scaled_display_value(channel_id, value),
                );
            }
        }
        values
    }

    fn live_trigger_time(&self) -> Option<f64> {
        let capture = self.live.last_capture.as_ref()?;
        let timestamp = capture.timestamps.get(capture.trigger_position).copied()?;
        let tick_hz = self.live.hello_ack.as_ref()?.tick_hz;
        Some(timestamp as f64 / tick_hz as f64)
    }

    fn compact_rate(rate_hz: u32) -> String {
        if rate_hz >= 1_000_000 && rate_hz.is_multiple_of(1_000_000) {
            format!("{} M", rate_hz / 1_000_000)
        } else if rate_hz >= 1_000 && rate_hz.is_multiple_of(1_000) {
            format!("{}", rate_hz / 1_000)
        } else {
            format!("{:.1}", f64::from(rate_hz) / 1_000.0)
        }
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

    fn live_connection_state_text(&self) -> &'static str {
        match self.live.connection_state {
            ConnectionState::Disconnected => self.live_text("未连接", "Disconnected"),
            ConnectionState::Connecting => self.live_text("正在连接", "Connecting"),
            ConnectionState::Handshaking => self.live_text("正在握手", "Handshaking"),
            ConnectionState::Configuring => self.live_text("正在应用采集", "Configuring"),
            ConnectionState::Ready => self.live_text("已就绪", "Ready"),
            ConnectionState::Streaming => self.live_text("正在采集", "Streaming"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ScopeApp;

    #[test]
    fn compact_rate_keeps_toolbar_summary_short() {
        assert_eq!(ScopeApp::compact_rate(500), "0.5");
        assert_eq!(ScopeApp::compact_rate(100_000), "100");
        assert_eq!(ScopeApp::compact_rate(1_000_000), "1 M");
    }
}
