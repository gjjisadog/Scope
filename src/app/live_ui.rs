use super::*;
use scope_analyzer::live::{
    bandwidth::{calculate_link_budget, BudgetSeverity, LinkBudgetResult, LinkBudgetTransport},
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
    fn consume_bandwidth_expert_override(critical: bool, enabled: &mut bool) -> bool {
        let used = critical && *enabled;
        *enabled = false;
        used
    }

    fn live_link_budget(&self) -> Result<LinkBudgetResult, String> {
        let table = self.live.channel_table.as_ref().ok_or_else(|| {
            self.live_text("等待通道表", "Waiting for channel table")
                .to_owned()
        })?;
        let maximum_payload = self
            .live
            .hello_ack
            .as_ref()
            .and_then(|hello| usize::try_from(hello.max_payload).ok())
            .unwrap_or(scope_analyzer::live::protocol::MAX_PAYLOAD_LEN);
        let transport = match self.live.transport {
            TransportConfig::Serial { baud, .. } => LinkBudgetTransport::Serial { baud },
            TransportConfig::Tcp { .. } => LinkBudgetTransport::Tcp {
                expected_bits_per_second: None,
            },
        };
        calculate_link_budget(table, &self.live.acquisition, transport, maximum_payload)
            .map_err(|error| error.to_string())
    }

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
        let budget_critical = self
            .live_link_budget()
            .is_ok_and(|budget| budget.severity == BudgetSeverity::Critical);
        if ui
            .add_enabled(
                ready && (!budget_critical || self.live_bandwidth_expert_override),
                egui::Button::new(start_label),
            )
            .on_hover_text(self.live_text(
                "应用当前采集参数并开始实时采集",
                "Apply the current acquisition settings and start streaming",
            ))
            .clicked()
        {
            let used_override = Self::consume_bandwidth_expert_override(
                budget_critical,
                &mut self.live_bandwidth_expert_override,
            );
            if used_override {
                self.live_bandwidth_override_notice = Some(
                    self.live_text(
                        "已使用一次性专家覆盖启动临界带宽配置",
                        "Critical bandwidth configuration started with one-shot expert override",
                    )
                    .to_owned(),
                );
            }
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
            let analyze_label = self.live_text("分析捕获", "Analyze capture");
            let analyze_hint = self.live_text(
                "冻结当前触发捕获或实时历史并进入离线分析",
                "Freeze the current trigger capture or live history and open offline analysis",
            );
            if ui
                .add_enabled(
                    self.live.has_analysis_snapshot()
                        && self.live.channel_table.is_some()
                        && self.live.acquisition.sample_rate_hz > 0,
                    egui::Button::new(analyze_label),
                )
                .on_hover_text(analyze_hint)
                .clicked()
            {
                self.analyze_live_capture();
            }
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
                    .channel_presentations
                    .get(&channel.channel_id)
                    .map(|presentation| presentation.visible)
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
            let display_name = self
                .live
                .channel_presentations
                .get(&channel.channel_id)
                .map(|presentation| presentation.display_name.as_str())
                .unwrap_or(&channel.name);
            if !filter.is_empty()
                && !display_name.to_lowercase().contains(&filter)
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
                    .channel_presentations
                    .get(&channel.channel_id)
                    .map(|presentation| presentation.visible)
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
            .channel_presentations
            .get(&channel.channel_id)
            .map(|presentation| presentation.visible)
            .unwrap_or(true);
        let mut scale = self
            .live
            .channel_presentations
            .get(&channel.channel_id)
            .map(|presentation| presentation.scale)
            .unwrap_or(1.0);
        let rgba = self
            .live
            .channel_presentations
            .get(&channel.channel_id)
            .map(|presentation| presentation.color)
            .unwrap_or([80, 140, 220, 255]);
        let mut color = Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
        let mut display_name = self
            .live
            .channel_presentations
            .get(&channel.channel_id)
            .map(|presentation| presentation.display_name.clone())
            .unwrap_or_else(|| channel.name.clone());
        let mut pane = self
            .live
            .channel_presentations
            .get(&channel.channel_id)
            .map(|presentation| presentation.pane)
            .unwrap_or(0)
            .min(self.scope_pane_count().saturating_sub(1));

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
                    if let Some(presentation) =
                        self.live.channel_presentations.get_mut(&channel.channel_id)
                    {
                        presentation.visible = visible;
                    }
                }
                if egui::color_picker::color_edit_button_srgba(
                    ui,
                    &mut color,
                    egui::color_picker::Alpha::Opaque,
                )
                .changed()
                {
                    if let Some(presentation) =
                        self.live.channel_presentations.get_mut(&channel.channel_id)
                    {
                        presentation.color = color.to_array();
                    }
                }
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut display_name)
                            .desired_width(92.0)
                            .hint_text(&channel.name),
                    )
                    .on_hover_text(format!(
                        "{}: {}",
                        self.live_text("原始名", "Source"),
                        channel.name
                    ))
                    .changed()
                {
                    if let Some(presentation) =
                        self.live.channel_presentations.get_mut(&channel.channel_id)
                    {
                        presentation.display_name = if display_name.trim().is_empty() {
                            channel.name.clone()
                        } else {
                            display_name.clone()
                        };
                    }
                }
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
                    if let Some(presentation) =
                        self.live.channel_presentations.get_mut(&channel.channel_id)
                    {
                        presentation.scale = scale;
                        presentation.sanitize();
                    }
                }
                ui.label(RichText::new(self.live_text("窗格", "Pane")).small());
                egui::ComboBox::from_id_source(("live_channel_pane", channel.channel_id))
                    .selected_text(format!("{}", pane + 1))
                    .width(48.0)
                    .show_ui(ui, |ui| {
                        for candidate in 0..self.scope_pane_count() {
                            ui.selectable_value(&mut pane, candidate, format!("{}", candidate + 1));
                        }
                    });
                if let Some(presentation) =
                    self.live.channel_presentations.get_mut(&channel.channel_id)
                {
                    presentation.pane = pane;
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
        if let Some(capture) = self.live.selected_trigger_capture() {
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

        ui.add_space(10.0);
        ui.label(RichText::new(self.live_text("采集带宽", "Acquisition bandwidth")).strong());
        match self.live_link_budget() {
            Ok(budget) => {
                let (status, color) = match budget.severity {
                    BudgetSeverity::Safe => (
                        self.live_text("安全", "Safe"),
                        Color32::from_rgb(67, 190, 115),
                    ),
                    BudgetSeverity::Warning => (
                        self.live_text("警告", "Warning"),
                        Color32::from_rgb(226, 170, 55),
                    ),
                    BudgetSeverity::Critical => (
                        self.live_text("超出预算", "Critical"),
                        Color32::from_rgb(220, 85, 75),
                    ),
                    BudgetSeverity::Unknown => (
                        self.live_text("仅供参考", "Advisory"),
                        Color32::from_gray(150),
                    ),
                };
                ui.colored_label(color, RichText::new(status).strong());
                egui::Grid::new("live_bandwidth_budget")
                    .num_columns(2)
                    .spacing([12.0, 5.0])
                    .show(ui, |ui| {
                        ui.label(self.live_text("帧大小", "Frame size"));
                        ui.label(format!("{} B", budget.frame_bytes));
                        ui.end_row();
                        ui.label(self.live_text("链路流量", "Link traffic"));
                        ui.label(format!("{:.1} KiB/s", budget.bytes_per_second / 1024.0));
                        ui.end_row();
                        ui.label(self.live_text("批次延迟", "Batch latency"));
                        ui.label(format!("{:.2} ms", budget.batch_latency_seconds * 1000.0));
                        ui.end_row();
                        if let Some(utilization) = budget.utilization {
                            ui.label(self.live_text("利用率", "Utilization"));
                            ui.label(format!("{:.1}%", utilization * 100.0));
                            ui.end_row();
                        }
                    });
                if budget.severity != BudgetSeverity::Safe {
                    if let Some(suggestion) = budget.suggested_batch_samples {
                        if suggestion != self.live.acquisition.batch_samples
                            && ui
                                .add_enabled(
                                    can_edit,
                                    egui::Button::new(format!(
                                        "{} {suggestion}",
                                        self.live_text("采用安全批次", "Use safe batch")
                                    )),
                                )
                                .clicked()
                        {
                            self.live.acquisition.batch_samples = suggestion;
                            self.live.configuration_applied = false;
                            self.live_bandwidth_expert_override = false;
                        }
                    }
                }
                if budget.severity == BudgetSeverity::Critical {
                    let override_label =
                        self.live_text("专家模式：仍允许开始", "Expert override: allow start");
                    let override_help = self.live_text(
                        "仅绕过主机侧带宽策略，不绕过设备或协议限制",
                        "Bypasses only the host bandwidth policy, never device or protocol limits",
                    );
                    ui.checkbox(&mut self.live_bandwidth_expert_override, override_label)
                        .on_hover_text(override_help);
                } else {
                    self.live_bandwidth_expert_override = false;
                }
            }
            Err(message) => {
                ui.label(RichText::new(message).color(Color32::from_gray(145)));
                self.live_bandwidth_expert_override = false;
            }
        }

        self.live_power_bindings_ui(ui, can_edit);

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

    fn live_power_bindings_ui(&mut self, ui: &mut egui::Ui, can_edit: bool) {
        let channels = self
            .live
            .channel_table
            .as_ref()
            .map(|table| {
                table
                    .channels
                    .iter()
                    .filter(|channel| {
                        channel.kind == ChannelKind::Analog
                            && self.live.acquisition.channel_mask & (1_u64 << channel.channel_id)
                                != 0
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        ui.add_space(10.0);
        ui.label(RichText::new(self.live_text("三相功率", "Three-phase power")).strong());
        let mut changed = ui
            .add_enabled(
                can_edit && channels.len() >= 6,
                egui::Checkbox::new(&mut self.power_enabled, "P / Q₁ / S / PF"),
            )
            .changed();
        if !self.power_enabled || channels.len() < 6 {
            if channels.len() < 6 {
                ui.label(self.live_text(
                    "至少选择 6 个模拟通道",
                    "Select at least six analog channels",
                ));
            }
            if changed {
                self.live_measurement_cache = None;
            }
            return;
        }
        let available = channels
            .iter()
            .map(|channel| usize::from(channel.channel_id))
            .collect::<Vec<_>>();
        if self
            .power_voltage_channels
            .iter()
            .chain(&self.power_current_channels)
            .any(|channel| !available.contains(channel))
        {
            self.power_voltage_channels.copy_from_slice(&available[..3]);
            self.power_current_channels
                .copy_from_slice(&available[3..6]);
            changed = true;
        }
        for (label, binding, id) in [
            (
                "Vabc",
                &mut self.power_voltage_channels,
                "live_power_voltage",
            ),
            (
                "Iabc",
                &mut self.power_current_channels,
                "live_power_current",
            ),
        ] {
            ui.horizontal(|ui| {
                ui.label(label);
                for (phase, selected) in binding.iter_mut().enumerate() {
                    let selected_name = channels
                        .iter()
                        .find(|channel| usize::from(channel.channel_id) == *selected)
                        .map(|channel| channel.name.as_str())
                        .unwrap_or("-");
                    egui::ComboBox::from_id_source((id, phase))
                        .selected_text(selected_name)
                        .width(68.0)
                        .show_ui(ui, |ui| {
                            for channel in &channels {
                                changed |= ui
                                    .selectable_value(
                                        selected,
                                        usize::from(channel.channel_id),
                                        &channel.name,
                                    )
                                    .changed();
                            }
                        });
                }
            });
        }
        if changed {
            self.live_measurement_cache = None;
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
        let measurements_label = self.live_text("测量", "Measurements");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.live_bottom_tab, 0, events_label);
            ui.selectable_value(&mut self.live_bottom_tab, 1, link_label);
            ui.selectable_value(&mut self.live_bottom_tab, 2, measurements_label);
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
        match self.live_bottom_tab {
            1 => self.live_stats_grid(ui, true),
            2 => self.live_measurement_table(ui),
            _ => self.live_event_table(ui),
        }
    }

    fn live_measurement_input(&self) -> Result<LiveMeasurementInput, String> {
        let snapshot = self
            .live
            .measurement_snapshot(MAX_AUTO_MEASURE_POINTS)
            .ok_or_else(|| self.live_text("暂无实时样点", "No live samples").to_owned())?;
        let end_time = snapshot
            .segments
            .last()
            .and_then(|segment| segment.times.last())
            .copied();
        let table = self.live.channel_table.as_ref().ok_or_else(|| {
            self.live_text("等待通道表", "Waiting for channel table")
                .to_owned()
        })?;
        let segments = snapshot
            .segments
            .into_iter()
            .map(|segment| SampleBlock {
                times: segment.times,
                channels: segment.channels,
            })
            .collect::<Vec<_>>();
        let specs = snapshot
            .channel_ids
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(column, channel_id)| {
                let descriptor = table.channel(channel_id)?;
                let presentation = self.live.channel_presentations.get(&channel_id);
                Some(ChannelMeasurementSpec {
                    channel_index: usize::from(channel_id),
                    column,
                    name: presentation
                        .map(|value| value.display_name.clone())
                        .unwrap_or_else(|| descriptor.name.clone()),
                    unit: descriptor.unit.clone(),
                    scale: f64::from(presentation.map_or(1.0, |value| value.scale)),
                })
            })
            .take(12)
            .collect::<Vec<_>>();
        let power = if self.power_enabled {
            let voltage_columns = self.power_voltage_channels.map(|channel| {
                snapshot
                    .channel_ids
                    .iter()
                    .position(|candidate| usize::from(*candidate) == channel)
            });
            let current_columns = self.power_current_channels.map(|channel| {
                snapshot
                    .channel_ids
                    .iter()
                    .position(|candidate| usize::from(*candidate) == channel)
            });
            if voltage_columns.iter().all(Option::is_some)
                && current_columns.iter().all(Option::is_some)
            {
                let mut power = ThreePhasePowerSpec::new(
                    voltage_columns.map(Option::unwrap),
                    current_columns.map(Option::unwrap),
                );
                power.voltage_scales = self.power_voltage_channels.map(|channel| {
                    self.live
                        .channel_presentations
                        .get(&(channel as u16))
                        .map_or(1.0, |value| f64::from(value.scale))
                });
                power.current_scales = self.power_current_channels.map(|channel| {
                    self.live
                        .channel_presentations
                        .get(&(channel as u16))
                        .map_or(1.0, |value| f64::from(value.scale))
                });
                power.nominal_frequency_hz = self.harmonic_base_hz.max(0.001);
                Some(power)
            } else {
                None
            }
        } else {
            None
        };
        Ok(LiveMeasurementInput {
            end_time,
            segments,
            specs,
            power,
            signature: self.live_measurement_signature(),
        })
    }

    fn live_measurement_signature(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.live
            .channel_table
            .as_ref()
            .map(|table| table.revision)
            .hash(&mut hasher);
        self.live.acquisition.channel_mask.hash(&mut hasher);
        self.live.acquisition.sample_rate_hz.hash(&mut hasher);
        for (channel_id, presentation) in &self.live.channel_presentations {
            channel_id.hash(&mut hasher);
            presentation.scale.to_bits().hash(&mut hasher);
            presentation.display_name.hash(&mut hasher);
        }
        self.power_enabled.hash(&mut hasher);
        self.power_voltage_channels.hash(&mut hasher);
        self.power_current_channels.hash(&mut hasher);
        self.harmonic_base_hz.to_bits().hash(&mut hasher);
        hasher.finish()
    }

    #[cfg(test)]
    fn refresh_live_measurements(&mut self) -> Result<EngineeringMeasurementResult, String> {
        let input = self.live_measurement_input()?;
        let result = analyze_segments(&input.segments, &input.specs, input.power.as_ref())
            .map_err(|error| error.to_string())?;
        self.live_measurement_cache = Some(LiveMeasurementCache {
            updated_at: Instant::now(),
            end_time: input.end_time,
            result: result.clone(),
            signature: input.signature,
        });
        Ok(result)
    }

    fn poll_live_measurement_worker(&mut self) {
        let Some(joined) = Self::take_finished_job(
            &mut self.live_measurement_worker,
            "Live measurement worker panicked.",
        ) else {
            return;
        };
        match joined {
            Ok(result) => match result.result {
                Ok(measurement)
                    if self.live.measurement_snapshot(1).is_some()
                        && result.signature == self.live_measurement_signature() =>
                {
                    self.live_measurement_cache = Some(LiveMeasurementCache {
                        updated_at: Instant::now(),
                        end_time: result.end_time,
                        result: measurement,
                        signature: result.signature,
                    });
                }
                Ok(_) => {}
                Err(error) => self.live.last_error = Some(error),
            },
            Err(error) => self.live.last_error = Some(error),
        }
    }

    fn dispatch_live_measurement_worker(&mut self) -> Result<(), String> {
        if self.live_measurement_worker.is_some()
            || self
                .live_measurement_last_dispatch
                .is_some_and(|instant| instant.elapsed() < Duration::from_millis(250))
            || self.live.display_paused
                && self.live_measurement_cache.as_ref().is_some_and(|cache| {
                    cache.end_time
                        == self.live.measurement_snapshot(1).and_then(|snapshot| {
                            snapshot
                                .segments
                                .last()
                                .and_then(|segment| segment.times.last())
                                .copied()
                        })
                })
        {
            return Ok(());
        }
        let input = self.live_measurement_input()?;
        self.live_measurement_last_dispatch = Some(Instant::now());
        Self::spawn_job(&mut self.live_measurement_worker, move || {
            LiveMeasurementWorkerResult {
                end_time: input.end_time,
                signature: input.signature,
                result: analyze_segments(&input.segments, &input.specs, input.power.as_ref())
                    .map_err(|error| error.to_string()),
            }
        });
        Ok(())
    }

    fn live_measurement_table(&mut self, ui: &mut egui::Ui) {
        self.poll_live_measurement_worker();
        if let Err(message) = self.dispatch_live_measurement_worker() {
            ui.label(message);
            return;
        }
        let signature = self.live_measurement_signature();
        let Some((result, refresh_age)) = self
            .live_measurement_cache
            .as_ref()
            .filter(|cache| cache.signature == signature)
            .map(|cache| (cache.result.clone(), cache.updated_at.elapsed()))
        else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(self.live_text("计算实时测量…", "Calculating Live measurements…"));
            });
            ui.ctx().request_repaint_after(Duration::from_millis(20));
            return;
        };
        ui.label(
            RichText::new(format!(
                "{} {:.0} ms",
                self.live_text("刷新于", "Updated"),
                refresh_age.as_secs_f64() * 1000.0
            ))
            .small()
            .color(Color32::from_gray(150)),
        );
        egui::ScrollArea::both().show(ui, |ui| {
            egui::Grid::new("live_engineering_measurements")
                .striped(true)
                .num_columns(10)
                .spacing([14.0, 5.0])
                .show(ui, |ui| {
                    for heading in [
                        self.live_text("通道", "Channel"),
                        "Avg",
                        "RMS",
                        "+Peak",
                        "-Peak",
                        "Abs",
                        "Pk-Pk",
                        "Freq",
                        self.live_text("样点", "Samples"),
                        self.live_text("质量", "Quality"),
                    ] {
                        ui.strong(heading);
                    }
                    ui.end_row();
                    for statistics in &result.channels {
                        ui.label(&statistics.name);
                        for value in [
                            statistics.mean,
                            statistics.rms,
                            statistics.positive_peak,
                            statistics.negative_peak,
                            statistics.absolute_peak,
                            statistics.peak_to_peak,
                        ] {
                            ui.label(Self::format_measurement_value(value));
                        }
                        ui.label(Self::format_measurement_value(
                            statistics.frequency.as_ref().map(|value| value.hz),
                        ));
                        ui.label(statistics.valid_samples.to_string());
                        ui.label(Self::measurement_quality_label(statistics));
                        ui.end_row();
                    }
                });
        });
        if let Some(power) = &result.power {
            ui.horizontal_wrapped(|ui| {
                ui.strong("3φ");
                ui.label(format!(
                    "P {:.3} {}",
                    power.active_power, power.active_power_unit
                ));
                ui.label(format!(
                    "Q₁ {:.3} {}",
                    power.fundamental_reactive_power, power.reactive_power_unit
                ));
                ui.label(format!(
                    "S {:.3} {}",
                    power.effective_apparent_power, power.apparent_power_unit
                ));
                ui.label(format!(
                    "PF {}",
                    Self::format_measurement_value(power.true_power_factor)
                ));
            });
        }
        if self.live.connection_state == ConnectionState::Streaming && !self.live.display_paused {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
    }

    fn live_event_table(&mut self, ui: &mut egui::Ui) {
        let keep_selection_label = self.live_text("保持当前选择", "Keep current selection");
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.live.keep_capture_selection, keep_selection_label);
            let has_pinned = self
                .live
                .capture_history
                .entries()
                .iter()
                .any(|entry| entry.pinned);
            let clear_label = if has_pinned && self.live_confirm_clear_capture_history {
                self.live_text("确认清除（含固定项）", "Confirm clear including pinned")
            } else {
                self.live_text("清除历史", "Clear history")
            };
            if ui.button(clear_label).clicked() {
                if has_pinned && !self.live_confirm_clear_capture_history {
                    self.live_confirm_clear_capture_history = true;
                } else {
                    self.live.capture_history.clear(true);
                    self.live.last_capture = None;
                    self.live_confirm_clear_capture_history = false;
                }
            }
        });
        let entries = self
            .live
            .capture_history
            .entries()
            .iter()
            .rev()
            .cloned()
            .collect::<Vec<_>>();
        if !entries.is_empty() {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                egui::Grid::new("live_capture_history")
                    .num_columns(6)
                    .striped(true)
                    .spacing([14.0, 5.0])
                    .show(ui, |ui| {
                        for heading in ["#", "Pin", "Label", "Sample", "Mode", "Quality"] {
                            ui.strong(heading);
                        }
                        ui.end_row();
                        for entry in entries {
                            let selected =
                                self.live.capture_history.selected_id() == Some(entry.id);
                            if ui
                                .selectable_label(selected, entry.id.0.to_string())
                                .clicked()
                            {
                                self.live.capture_history.select(entry.id);
                                self.live.last_capture = match &entry.payload {
                                    scope_analyzer::live::capture_history::CapturePayload::InMemory(
                                        capture,
                                    ) => Some((**capture).clone()),
                                    _ => None,
                                };
                            }
                            let mut pinned = entry.pinned;
                            if ui.checkbox(&mut pinned, "").changed() {
                                self.live.capture_history.set_pinned(entry.id, pinned);
                            }
                            ui.label(&entry.label);
                            ui.label(entry.trigger_sample_index.to_string());
                            ui.label(format!("{:?}", entry.trigger_config.mode));
                            ui.label(if entry.quality.auto_timeout {
                                "Auto"
                            } else if entry.quality.incomplete_pre || entry.quality.incomplete_post
                            {
                                "Partial"
                            } else {
                                "OK"
                            });
                            ui.end_row();
                        }
                    });
            });
            ui.horizontal(|ui| {
                let previous = ui.button(self.live_text("上一条", "Previous")).clicked();
                let next = ui.button(self.live_text("下一条", "Next")).clicked();
                if previous {
                    self.live.capture_history.select_previous();
                } else if next {
                    self.live.capture_history.select_next();
                }
                if previous || next {
                    self.live.last_capture =
                        self.live.capture_history.selected().and_then(|entry| {
                            match &entry.payload {
                                scope_analyzer::live::capture_history::CapturePayload::InMemory(
                                    capture,
                                ) => Some((**capture).clone()),
                                _ => None,
                            }
                        });
                }
                let remove = ui
                    .add_enabled(
                        self.live.capture_history.selected_id().is_some(),
                        egui::Button::new(self.live_text("删除当前", "Remove selected")),
                    )
                    .clicked();
                if remove {
                    if let Some(id) = self.live.capture_history.selected_id() {
                        self.live.capture_history.remove(id);
                        self.live.last_capture =
                            self.live.capture_history.selected().and_then(|entry| {
                                match &entry.payload {
                                scope_analyzer::live::capture_history::CapturePayload::InMemory(
                                    capture,
                                ) => Some((**capture).clone()),
                                _ => None,
                            }
                            });
                        self.project_dirty = true;
                    }
                }
            });
            if let Some(entry) = self.live.capture_history.selected().cloned() {
                let mut label = entry.label;
                let mut note = entry.note;
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label(self.live_text("标签", "Label"));
                    changed |= ui
                        .add(egui::TextEdit::singleline(&mut label).desired_width(240.0))
                        .changed();
                });
                ui.horizontal(|ui| {
                    ui.label(self.live_text("备注", "Note"));
                    changed |= ui
                        .add(
                            egui::TextEdit::multiline(&mut note)
                                .desired_width(420.0)
                                .desired_rows(2),
                        )
                        .changed();
                });
                if changed {
                    self.live
                        .capture_history
                        .set_metadata(entry.id, label, note, entry.pinned);
                    self.project_dirty = true;
                }
            }
            ui.separator();
        }
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

                    if let Some(notice) = &self.live_bandwidth_override_notice {
                        ui.label(self.live_text("带宽策略", "Bandwidth policy"));
                        ui.label(self.live_text("专家覆盖", "Expert override"));
                        ui.label(notice);
                        ui.end_row();
                    }

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
                    .channel_presentations
                    .get(channel_id)
                    .map(|presentation| presentation.visible)
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

        let snapshot_start = snapshot
            .segments
            .first()
            .and_then(|segment| segment.times.first())
            .copied();
        let snapshot_end = snapshot
            .segments
            .last()
            .and_then(|segment| segment.times.last())
            .copied();
        if !self.live.plot_viewport.initialized {
            if let (Some(start), Some(end)) = (snapshot_start, snapshot_end) {
                self.live.plot_viewport.reset_to_range(start, end);
            }
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
                    let name = self
                        .live
                        .channel_presentations
                        .get(&channel_id)
                        .map(|presentation| presentation.display_name.as_str())
                        .or_else(|| descriptor.as_ref().map(|channel| channel.name.as_str()))
                        .unwrap_or("-")
                        .to_owned();
                    let unit = descriptor
                        .as_ref()
                        .map(|channel| channel.unit.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let rgba = self
                        .live
                        .channel_presentations
                        .get(&channel_id)
                        .map(|presentation| presentation.color)
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
                                    ui.colored_label(color, RichText::new(&name).strong());
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
                                                .channel_presentations
                                                .get(&channel_id)
                                                .map(|presentation| presentation.scale)
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
                            let plot_response = Plot::new(("live_scope_lane", channel_id))
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
                                            plot_ui.line(line.name(&name));
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
                                    if self.live.plot_viewport.show_cursor_a {
                                        plot_ui.vline(
                                            VLine::new(self.live.plot_viewport.cursor_a)
                                                .color(Color32::from_rgb(238, 76, 82))
                                                .width(1.3),
                                        );
                                    }
                                    if self.live.plot_viewport.show_cursor_b {
                                        plot_ui.vline(
                                            VLine::new(self.live.plot_viewport.cursor_b)
                                                .color(Color32::from_rgb(89, 181, 255))
                                                .width(1.3),
                                        );
                                    }
                                });
                            let bounds = plot_response.transform.bounds();
                            if bounds.min()[0].is_finite()
                                && bounds.max()[0].is_finite()
                                && bounds.max()[0] > bounds.min()[0]
                            {
                                self.live.plot_viewport.view_start = bounds.min()[0];
                                self.live.plot_viewport.view_end = bounds.max()[0];
                                self.live.plot_viewport.initialized = true;
                            }
                            if plot_response.response.clicked() {
                                if let Some(position) =
                                    plot_response.response.interact_pointer_pos()
                                {
                                    let time =
                                        plot_response.transform.value_from_position(position).x;
                                    let cursor = if (time - self.live.plot_viewport.cursor_a).abs()
                                        <= (time - self.live.plot_viewport.cursor_b).abs()
                                    {
                                        CursorId::A
                                    } else {
                                        CursorId::B
                                    };
                                    match cursor {
                                        CursorId::A => {
                                            self.live.plot_viewport.cursor_a = time;
                                            self.live.plot_viewport.show_cursor_a = true;
                                        }
                                        CursorId::B => {
                                            self.live.plot_viewport.cursor_b = time;
                                            self.live.plot_viewport.show_cursor_b = true;
                                        }
                                    }
                                    self.live.plot_viewport.active_cursor = cursor;
                                }
                            }
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
        let capture = self.live.selected_trigger_capture()?;
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
                let trigger_events = source.trigger_records().to_vec();
                let tick_hz = source.tick_hz();
                let recent_path = path.clone();
                self.set_source(Arc::new(source), path, SourceKind::Scope);
                self.scope_trigger_events = trigger_events;
                self.scope_trigger_tick_hz = tick_hz;
                self.selected_scope_trigger = (!self.scope_trigger_events.is_empty()).then_some(0);
                self.remember_recent_file(&recent_path);
                self.live.workspace_mode = WorkspaceMode::Offline;
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn analyze_live_capture(&mut self) {
        let live_viewport = self.live.plot_viewport.clone();
        let Some(snapshot) = self.live.analysis_snapshot() else {
            self.live.last_error = Some(
                self.live_text("当前没有可分析的捕获", "No capture is available to analyze")
                    .to_owned(),
            );
            return;
        };
        let Some(channel_table) = self.live.channel_table.clone() else {
            self.live.last_error = Some(
                self.live_text(
                    "设备通道表尚不可用",
                    "The device channel table is unavailable",
                )
                .to_owned(),
            );
            return;
        };
        let source_name = if self.live.selected_trigger_capture().is_some() {
            self.live_text("实时触发捕获", "Live trigger capture")
        } else {
            self.live_text("实时冻结快照", "Live frozen snapshot")
        };
        let source =
            match scope_analyzer::live::snapshot_source::SnapshotDataSource::from_live_snapshot(
                source_name,
                snapshot,
                &channel_table,
                f64::from(self.live.acquisition.sample_rate_hz),
                &self.live.channel_presentations,
            ) {
                Ok(source) => source,
                Err(error) => {
                    self.live.last_error = Some(error.to_string());
                    return;
                }
            };

        self.set_source(
            Arc::new(source),
            PathBuf::from(format!("{source_name}.scope")),
            SourceKind::Scope,
        );
        if live_viewport.initialized {
            self.plot_viewport = live_viewport;
            self.plot_viewport.set_pane_count(self.scope_pane_count());
        }
        self.live.workspace_mode = WorkspaceMode::Offline;
        self.import_status = Some(self.live_text(
            "已冻结实时捕获，可使用光标测量、FFT/THD、序分量和导出。",
            "Live capture frozen. Cursor measurements, FFT/THD, sequence analysis, and export are available.",
        ).to_owned());
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
    use super::{ScopeApp, WorkspaceMode};
    use scope_analyzer::{
        live::{
            protocol::{ChannelDescriptor, ChannelKind, ChannelTable, HelloAck, WireFormat},
            trigger::TriggerCapture,
        },
        plot_viewport::PlotViewport,
        presentation::ChannelPresentation,
    };

    #[test]
    fn compact_rate_keeps_toolbar_summary_short() {
        assert_eq!(ScopeApp::compact_rate(500), "0.5");
        assert_eq!(ScopeApp::compact_rate(100_000), "100");
        assert_eq!(ScopeApp::compact_rate(1_000_000), "1 M");
    }

    #[test]
    fn critical_bandwidth_override_is_consumed_once() {
        let mut enabled = true;
        assert!(ScopeApp::consume_bandwidth_expert_override(
            true,
            &mut enabled
        ));
        assert!(!enabled);
        assert!(!ScopeApp::consume_bandwidth_expert_override(
            true,
            &mut enabled
        ));
    }

    #[test]
    fn trigger_capture_enters_mainline_analysis_and_export_with_shared_presentation() {
        let sample_rate_hz = 1_000_u32;
        let sample_count = 256_usize;
        let channel_ids = vec![0, 1, 2];
        let channels = (0..3)
            .map(|phase| {
                (0..sample_count)
                    .map(|index| {
                        let angle = std::f32::consts::TAU * 50.0 * index as f32
                            / sample_rate_hz as f32
                            - std::f32::consts::TAU * phase as f32 / 3.0;
                        angle.sin()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mut app = ScopeApp::new_for_test();
        app.scope_layout_cols = 2;
        app.live.acquisition.sample_rate_hz = sample_rate_hz;
        app.live.hello_ack = Some(HelloAck {
            device_capabilities: 0,
            max_payload: 1_048_576,
            tick_hz: u64::from(sample_rate_hz),
            channel_count: 3,
            max_batch_samples: 512,
            device_id: [7; 16],
            firmware_name: "capture-test".to_owned(),
        });
        app.live.channel_table = Some(ChannelTable {
            revision: 1,
            channels: channel_ids
                .iter()
                .map(|channel_id| ChannelDescriptor {
                    channel_id: *channel_id,
                    kind: ChannelKind::Analog,
                    wire_format: WireFormat::F32,
                    scale: 1.0,
                    offset: 0.0,
                    unit: "V".to_owned(),
                    name: format!("source-{channel_id}"),
                })
                .collect(),
        });
        for (index, channel_id) in channel_ids.iter().copied().enumerate() {
            app.live.channel_presentations.insert(
                channel_id,
                ChannelPresentation {
                    display_name: format!("Phase {}", ['A', 'B', 'C'][index]),
                    color: [20 + index as u8 * 50, 80, 180, 255],
                    visible: true,
                    scale: 2.0,
                    pane: usize::from(index > 0),
                },
            );
        }
        app.live.last_capture = Some(TriggerCapture {
            channel_ids,
            sample_indices: (0..sample_count as u64).collect(),
            timestamps: (0..sample_count as u64).collect(),
            channels,
            trigger_position: sample_count / 2,
            auto_timeout: false,
        });
        app.live.plot_viewport = PlotViewport::default();
        app.live
            .plot_viewport
            .reset_to_range(0.0, (sample_count - 1) as f64 / sample_rate_hz as f64);

        let live_rms = app.refresh_live_measurements().unwrap().channels[0]
            .rms
            .unwrap();
        app.live_measurement_cache = None;
        app.live_measurement_last_dispatch = None;
        app.dispatch_live_measurement_worker().unwrap();
        app.live.channel_presentations.get_mut(&0).unwrap().scale = 3.0;
        while app
            .live_measurement_worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        app.poll_live_measurement_worker();
        assert!(app.live_measurement_cache.is_none());
        app.live.channel_presentations.get_mut(&0).unwrap().scale = 2.0;
        app.live_measurement_last_dispatch = None;
        app.dispatch_live_measurement_worker().unwrap();
        while app
            .live_measurement_worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        app.poll_live_measurement_worker();
        assert!(app.live_measurement_cache.is_some());

        app.analyze_live_capture();

        assert_eq!(app.live.workspace_mode, WorkspaceMode::Offline);
        assert_eq!(app.display_names, vec!["Phase A", "Phase B", "Phase C"]);
        assert_eq!(app.channel_scales, vec![2.0, 2.0, 2.0]);
        assert_eq!(app.channel_panes, vec![0, 1, 1]);
        let source = app.source.clone().expect("snapshot source");
        let block = source
            .read_range(0.0, 0.255, &[0, 1, 2], sample_count)
            .unwrap();
        let measurement = ScopeApp::auto_measure(&block.times, &block.channels[0]).unwrap();
        assert!(measurement.statistics.max.is_some_and(|value| value > 0.9));
        assert!(measurement.statistics.min.is_some_and(|value| value < -0.9));
        let offline_rms = ScopeApp::auto_measure_segments(
            std::slice::from_ref(&block),
            &[(0, app.channel_scales[0])],
            None,
        )
        .unwrap()
        .rows[0]
            .1
            .statistics
            .rms
            .unwrap();
        assert!((live_rms - offline_rms).abs() < 1.0e-6);
        let fft = crate::fft::analyze(
            "Phase A".to_owned(),
            &block.channels[0],
            f64::from(sample_rate_hz),
            50.0,
            10,
        )
        .unwrap();
        assert!(fft.thd_percent < 1.0);
        let sequence = app.sequence_result().unwrap();
        assert!(sequence.positive.amplitude > sequence.negative.amplitude * 10.0);

        let plot_data = ScopeApp::load_plot_data(source, 0.0, 0.255, &[0, 1, 2], 3, 900.0).unwrap();
        app.apply_plot_job_data(plot_data, None);
        app.needs_plot_reload = false;
        app.needs_compare_plot_reload = false;
        app.export_waveform_png();
        assert!(app.show_export_preview);
        assert!(app.export_preview_dirty);
    }
}
