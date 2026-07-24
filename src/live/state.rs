use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::plot_viewport::PlotViewport;
use crate::presentation::ChannelPresentation;

use super::{
    buffer::{LiveBuffer, LiveSnapshot, SnapshotSegment},
    capture_history::{CaptureHistory, CapturePayload},
    hardware_capture::AssembledCapture,
    protocol::{validate_configure_for_device, ChannelTable, Configure, HelloAck, ResultCode},
    protocol_v2::{CaptureStatus, DecodedStreamSampleBatch, StreamTable},
    recording::{AsyncScopeRecorder, RecordingMetadata, RecordingStats},
    session::{AcquisitionConfig, ConnectionState, LiveSession, SessionEvent, SessionStats},
    snapshot::SnapshotDiagnostics,
    transport::TransportConfig,
    trigger::{TriggerCapture, TriggerConfig, TriggerEngine, TriggerMode},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkspaceMode {
    #[default]
    Offline,
    Live,
}

pub struct LiveScopeState {
    pub workspace_mode: WorkspaceMode,
    pub transport: TransportConfig,
    pub connection_state: ConnectionState,
    pub hello_ack: Option<HelloAck>,
    pub channel_table: Option<ChannelTable>,
    /// V2-only diagnostics. Kept separate so the default V1 workspace and
    /// `.scope` V1 recording path never reinterpret multi-domain rows.
    pub v2_stream_table: Option<StreamTable>,
    pub v2_last_snapshot: Option<DecodedStreamSampleBatch>,
    pub v2_snapshot_diagnostics: SnapshotDiagnostics,
    pub v2_capture_status: Option<CaptureStatus>,
    pub v2_last_capture: Option<AssembledCapture>,
    pub acquisition: Configure,
    pub configuration_applied: bool,
    pub history_seconds: u32,
    /// Latest immutable acquisition-worker snapshot. UI reads this only; it
    /// never mutates the live ring buffer or runs trigger processing.
    pub latest_display_snapshot: Option<Arc<LiveSnapshot>>,
    pub buffer: Option<LiveBuffer>,
    pub trigger: TriggerEngine,
    pub last_capture: Option<TriggerCapture>,
    pub capture_history: CaptureHistory,
    pub keep_capture_selection: bool,
    pub stats: SessionStats,
    pub last_error: Option<String>,
    pub recording_path: Option<PathBuf>,
    pub last_recording_stats: RecordingStats,
    pub channel_presentations: BTreeMap<u16, ChannelPresentation>,
    pub plot_viewport: PlotViewport,
    pub display_paused: bool,
    pub frozen_snapshot: Option<LiveSnapshot>,
    pub serial_ports: Vec<String>,
    session: Option<LiveSession>,
    recording: Option<AsyncScopeRecorder>,
    start_after_configure: bool,
}

impl Default for LiveScopeState {
    fn default() -> Self {
        Self {
            workspace_mode: WorkspaceMode::Offline,
            transport: TransportConfig::default(),
            connection_state: ConnectionState::Disconnected,
            hello_ack: None,
            channel_table: None,
            v2_stream_table: None,
            v2_last_snapshot: None,
            v2_snapshot_diagnostics: SnapshotDiagnostics::default(),
            v2_capture_status: None,
            v2_last_capture: None,
            acquisition: Configure {
                sample_rate_hz: 500,
                batch_samples: 10,
                channel_mask: u64::MAX,
            },
            configuration_applied: false,
            history_seconds: 1,
            latest_display_snapshot: None,
            buffer: None,
            trigger: TriggerEngine::new(TriggerConfig::default())
                .expect("default trigger configuration is valid"),
            last_capture: None,
            capture_history: CaptureHistory::default(),
            keep_capture_selection: false,
            stats: SessionStats::default(),
            last_error: None,
            recording_path: None,
            last_recording_stats: RecordingStats::default(),
            channel_presentations: BTreeMap::new(),
            plot_viewport: PlotViewport::default(),
            display_paused: false,
            frozen_snapshot: None,
            serial_ports: Vec::new(),
            session: None,
            recording: None,
            start_after_configure: false,
        }
    }
}

impl LiveScopeState {
    pub fn connect(&mut self) -> Result<(), String> {
        if self.session.is_some() {
            return Err("live session is already connected".to_owned());
        }
        self.last_error = None;
        self.hello_ack = None;
        self.channel_table = None;
        self.buffer = None;
        self.latest_display_snapshot = None;
        self.configuration_applied = false;
        self.stats = SessionStats::default();
        self.start_after_configure = false;
        self.workspace_mode = WorkspaceMode::Live;
        self.connection_state = ConnectionState::Connecting;
        self.session = Some(LiveSession::connect(self.transport.clone()).map_err(error_string)?);
        Ok(())
    }

    pub fn disconnect(&mut self) -> Result<(), String> {
        if self.recording.is_some() {
            self.stop_recording()?;
        }
        if let Some(session) = self.session.take() {
            session.disconnect().map_err(error_string)?;
        }
        self.connection_state = ConnectionState::Disconnected;
        self.start_after_configure = false;
        Ok(())
    }

    pub fn configure(&mut self, configure: Configure) -> Result<(), String> {
        let hello = self
            .hello_ack
            .as_ref()
            .ok_or_else(|| "device handshake is not complete".to_owned())?;
        let table = self
            .channel_table
            .as_ref()
            .ok_or_else(|| "device channel table is not available".to_owned())?;
        validate_configure_for_device(&configure, hello, table).map_err(error_string)?;
        if configure.channel_mask & (1_u64 << self.trigger.config().source_channel) == 0 {
            let source_channel = table
                .channels
                .iter()
                .find(|channel| configure.channel_mask & (1_u64 << channel.channel_id) != 0)
                .map(|channel| channel.channel_id)
                .ok_or_else(|| "channel mask selects no known channels".to_owned())?;
            let mut trigger = self.trigger.config().clone();
            trigger.source_channel = source_channel;
            self.trigger.set_config(trigger).map_err(error_string)?;
        }
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| "live session is not connected".to_owned())?;
        session
            .configure_with_acquisition(
                configure.clone(),
                AcquisitionConfig {
                    history_seconds: self.history_seconds,
                    trigger: self.trigger.config().clone(),
                },
            )
            .map_err(error_string)?;
        self.configuration_applied = false;
        Ok(())
    }

    pub fn start(&self) -> Result<(), String> {
        if !self.configuration_applied || self.connection_state != ConnectionState::Ready {
            return Err("live acquisition has not been configured successfully".to_owned());
        }
        self.session
            .as_ref()
            .ok_or_else(|| "live session is not connected".to_owned())?
            .start()
            .map_err(error_string)
    }

    pub fn start_with_configuration(&mut self, configure: Configure) -> Result<(), String> {
        if self.connection_state != ConnectionState::Ready {
            return Err("live acquisition is not ready to start".to_owned());
        }
        if self.configuration_applied {
            return self.start();
        }
        self.start_after_configure = true;
        if let Err(error) = self.configure(configure) {
            self.start_after_configure = false;
            return Err(error);
        }
        Ok(())
    }

    pub fn stop(&self) -> Result<(), String> {
        if self.connection_state != ConnectionState::Streaming {
            return Err("live acquisition is not streaming".to_owned());
        }
        self.session
            .as_ref()
            .ok_or_else(|| "live session is not connected".to_owned())?
            .stop()
            .map_err(error_string)
    }

    pub fn start_recording(&mut self, path: &Path) -> Result<(), String> {
        if self.recording.is_some() {
            return Err("recording is already active".to_owned());
        }
        if !self.configuration_applied {
            return Err("live acquisition has not been configured successfully".to_owned());
        }
        let hello = self
            .hello_ack
            .as_ref()
            .ok_or_else(|| "device handshake is not complete".to_owned())?;
        let channel_table = self
            .channel_table
            .clone()
            .ok_or_else(|| "device channel table is not available".to_owned())?;
        let metadata = RecordingMetadata {
            device_id: hex_device_id(&hello.device_id),
            firmware_name: hello.firmware_name.clone(),
            tick_hz: hello.tick_hz,
            channel_table,
            sample_rate_hz: self.acquisition.sample_rate_hz,
            batch_samples: self.acquisition.batch_samples,
            channel_mask: self.acquisition.channel_mask,
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            channel_presentations: self.channel_presentations.clone(),
        };
        let recorder = AsyncScopeRecorder::create(path, metadata).map_err(error_string)?;
        let ingress = recorder.ingress().map_err(error_string)?;
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| "live session is not connected".to_owned())?;
        if let Err(error) = session.set_recording(Some(ingress)) {
            let _ = recorder.abort();
            return Err(error_string(error));
        }
        self.recording = Some(recorder);
        self.last_recording_stats = RecordingStats::default();
        self.recording_path = Some(path.to_path_buf());
        Ok(())
    }

    pub fn stop_recording(&mut self) -> Result<(), String> {
        if let Some(session) = &self.session {
            session.set_recording(None).map_err(error_string)?;
        }
        let writer = self
            .recording
            .take()
            .ok_or_else(|| "recording is not active".to_owned())?;
        self.last_recording_stats = writer.finish().map_err(error_string)?;
        Ok(())
    }

    pub fn is_recording(&self) -> bool {
        self.recording
            .as_ref()
            .is_some_and(AsyncScopeRecorder::is_accepting)
    }

    pub fn recording_stats(&self) -> RecordingStats {
        self.recording
            .as_ref()
            .map(AsyncScopeRecorder::stats)
            .unwrap_or(self.last_recording_stats)
    }

    pub fn recording_pending_records(&self) -> usize {
        self.recording
            .as_ref()
            .map(AsyncScopeRecorder::pending_records)
            .unwrap_or(0)
    }

    pub fn snapshot(&self, max_points: usize) -> Option<LiveSnapshot> {
        self.latest_display_snapshot
            .as_ref()
            .map(|snapshot| (**snapshot).clone())
            .or_else(|| {
                self.buffer
                    .as_ref()
                    .map(|buffer| buffer.snapshot(max_points))
            })
    }

    pub fn measurement_snapshot(&self, max_samples: usize) -> Option<LiveSnapshot> {
        if let Some(capture) = self.selected_trigger_capture() {
            return self.trigger_capture_snapshot(capture, max_samples);
        }
        if self.display_paused {
            return self.frozen_snapshot.clone();
        }
        self.latest_display_snapshot
            .as_ref()
            .map(|snapshot| (**snapshot).clone())
            .or_else(|| {
                self.buffer
                    .as_ref()
                    .map(|buffer| buffer.snapshot_recent(max_samples))
            })
    }

    pub fn set_display_paused(&mut self, paused: bool) {
        if paused && !self.display_paused {
            self.frozen_snapshot = self.current_unpaused_snapshot(8_000);
        } else if !paused {
            self.frozen_snapshot = None;
        }
        self.display_paused = paused;
    }

    pub fn display_snapshot(&self, max_points: usize) -> Option<LiveSnapshot> {
        if self.display_paused {
            self.frozen_snapshot.clone()
        } else {
            self.current_unpaused_snapshot(max_points)
        }
    }

    /// Returns the latest immutable analysis input. A completed trigger
    /// capture takes precedence; otherwise this is the currently frozen view
    /// or the worker-published display snapshot.
    pub fn analysis_snapshot(&self) -> Option<LiveSnapshot> {
        if let Some(capture) = self.selected_trigger_capture() {
            return self.trigger_capture_snapshot(capture, usize::MAX);
        }
        if self.display_paused {
            return self.frozen_snapshot.clone();
        }
        self.snapshot(usize::MAX)
    }

    pub fn has_analysis_snapshot(&self) -> bool {
        self.selected_trigger_capture().is_some()
            || (self.display_paused && self.frozen_snapshot.is_some())
            || self
                .latest_display_snapshot
                .as_ref()
                .is_some_and(|snapshot| !snapshot.segments.is_empty())
            || self
                .buffer
                .as_ref()
                .is_some_and(|buffer| !buffer.is_empty())
    }

    pub fn scaled_display_value(&self, channel_id: u16, value: f32) -> f32 {
        value
            * self
                .channel_presentations
                .get(&channel_id)
                .map(|presentation| presentation.scale)
                .unwrap_or(1.0)
    }

    pub fn arm_trigger(&mut self) {
        self.last_capture = None;
        self.trigger.arm();
        if let Some(session) = &self.session {
            if let Err(error) = session.arm_trigger() {
                self.last_error = Some(error_string(error));
            }
        }
    }

    pub fn set_trigger_config(&mut self, config: TriggerConfig) -> Result<(), String> {
        self.trigger
            .set_config(config.clone())
            .map_err(error_string)?;
        if let Some(session) = &self.session {
            session.set_trigger_config(config).map_err(error_string)?;
        }
        self.last_capture = None;
        Ok(())
    }

    fn current_unpaused_snapshot(&self, max_points: usize) -> Option<LiveSnapshot> {
        if self.trigger.config().mode != TriggerMode::Auto {
            if let Some(capture) = self.selected_trigger_capture() {
                return self.trigger_capture_snapshot(capture, max_points);
            }
        }
        self.snapshot(max_points)
    }

    pub fn selected_trigger_capture(&self) -> Option<&TriggerCapture> {
        self.capture_history
            .selected()
            .and_then(|entry| match &entry.payload {
                CapturePayload::InMemory(capture) => Some(capture.as_ref()),
                CapturePayload::RecordingRange { .. } => None,
            })
            .or(self.last_capture.as_ref())
    }

    fn trigger_capture_snapshot(
        &self,
        capture: &TriggerCapture,
        max_points: usize,
    ) -> Option<LiveSnapshot> {
        let tick_hz = self.hello_ack.as_ref()?.tick_hz;
        if max_points == 0 || capture.timestamps.is_empty() {
            return Some(LiveSnapshot {
                channel_ids: capture.channel_ids.clone(),
                segments: Vec::new(),
            });
        }
        let mut selected = std::collections::BTreeSet::new();
        if capture.timestamps.len() <= max_points {
            selected.extend(0..capture.timestamps.len());
        } else if max_points == 1 {
            selected.insert(capture.trigger_position.min(capture.timestamps.len() - 1));
        } else {
            selected.insert(capture.trigger_position.min(capture.timestamps.len() - 1));
            if selected.len() < max_points {
                selected.insert(0);
            }
            if selected.len() < max_points {
                selected.insert(capture.timestamps.len() - 1);
            }
            let remaining = max_points.saturating_sub(selected.len());
            for offset in 0..remaining {
                selected.insert((offset + 1) * (capture.timestamps.len() - 1) / (remaining + 1));
            }
        }
        Some(LiveSnapshot {
            channel_ids: capture.channel_ids.clone(),
            segments: vec![SnapshotSegment {
                times: selected
                    .iter()
                    .map(|index| capture.timestamps[*index] as f64 / tick_hz as f64)
                    .collect(),
                channels: capture
                    .channels
                    .iter()
                    .map(|channel| selected.iter().map(|index| channel[*index]).collect())
                    .collect(),
            }],
        })
    }

    /// Polls pending session events and reports whether a new capture was
    /// successfully retained in history.
    pub fn poll(&mut self) -> bool {
        let recording_error = self
            .recording
            .as_mut()
            .and_then(AsyncScopeRecorder::poll_error);
        if let Some(error) = recording_error {
            if let Some(recording) = &self.recording {
                self.last_recording_stats = recording.stats();
            }
            self.recording.take();
            self.last_error = Some(error.to_string());
        }
        let mut events = Vec::new();
        if let Some(session) = &self.session {
            while let Ok(event) = session.try_recv() {
                events.push(event);
            }
        }
        let mut capture_history_changed = false;
        for event in events {
            match self.handle_event(event) {
                Ok(changed) => capture_history_changed |= changed,
                Err(error) => self.last_error = Some(error),
            }
        }
        capture_history_changed
    }

    fn handle_event(&mut self, event: SessionEvent) -> Result<bool, String> {
        let mut capture_history_changed = false;
        match event {
            SessionEvent::State(state) => {
                self.connection_state = state;
                if state == ConnectionState::Disconnected {
                    self.start_after_configure = false;
                    self.session.take();
                }
                if state == ConnectionState::Disconnected && self.recording.is_some() {
                    self.last_recording_stats = self.recording_stats();
                    self.recording.take();
                    self.last_error = Some(
                        "live connection ended during recording; the recoverable recording prefix was preserved"
                            .to_owned(),
                    );
                }
            }
            SessionEvent::HelloAck(hello) => self.hello_ack = Some(hello),
            SessionEvent::ChannelTable(table) => {
                let known_mask = table
                    .channels
                    .iter()
                    .fold(0_u64, |mask, channel| mask | (1_u64 << channel.channel_id));
                let retained_mask = self.acquisition.channel_mask & known_mask;
                self.acquisition.channel_mask =
                    if retained_mask == 0 || self.acquisition.channel_mask == u64::MAX {
                        known_mask
                    } else {
                        retained_mask
                    };
                for channel in &table.channels {
                    self.channel_presentations
                        .entry(channel.channel_id)
                        .or_insert_with(|| {
                            ChannelPresentation::new(
                                channel.name.clone(),
                                default_channel_color(channel.channel_id),
                            )
                        });
                }
                self.channel_table = Some(table);
                self.configuration_applied = false;
            }
            SessionEvent::StreamTable(table) => self.v2_stream_table = Some(table),
            SessionEvent::Configured(configure) => {
                self.acquisition = configure;
                self.configuration_applied = true;
                self.rebuild_buffer()?;
                if std::mem::take(&mut self.start_after_configure) {
                    self.session
                        .as_ref()
                        .ok_or_else(|| "live session is not connected".to_owned())?
                        .start()
                        .map_err(error_string)?;
                }
            }
            SessionEvent::ConfiguredV2(_) => self.configuration_applied = true,
            SessionEvent::CommandResult(result) => {
                if result.result_code != ResultCode::Ok {
                    self.start_after_configure = false;
                    return Err(format!(
                        "device command {} failed: {}",
                        result.request_sequence, result.detail
                    ));
                }
            }
            SessionEvent::Batch(_) => {
                return Err("received an unprocessed live batch on the UI path".to_owned());
            }
            SessionEvent::SnapshotV2(snapshot, diagnostics) => {
                self.v2_last_snapshot = Some(snapshot);
                self.v2_snapshot_diagnostics = diagnostics;
            }
            SessionEvent::CaptureStatus(status) => self.v2_capture_status = Some(status),
            SessionEvent::CaptureComplete(capture) => self.v2_last_capture = Some(capture),
            SessionEvent::CaptureFailure(error) => self.last_error = Some(error),
            SessionEvent::Gap(_) => {
                // Gap detection and live-ring mutation occur in the acquisition worker.
            }
            SessionEvent::DisplaySnapshot(snapshot) => {
                self.latest_display_snapshot = Some(snapshot)
            }
            SessionEvent::TriggerArmed(armed) => {
                if armed {
                    self.trigger.arm();
                } else {
                    self.trigger.disarm();
                }
            }
            SessionEvent::TriggerCapture(capture, trigger_config) => {
                if let Some(writer) = &mut self.recording {
                    if let Err(error) =
                        writer.try_write_trigger(capture.clone(), trigger_config.clone())
                    {
                        self.recording.take();
                        return Err(error_string(error));
                    }
                }
                let created_unix_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64;
                match self.capture_history.insert_live(
                    capture.clone(),
                    trigger_config,
                    created_unix_ms,
                    !self.keep_capture_selection,
                ) {
                    Ok(_) => capture_history_changed = true,
                    Err(error) => self.last_error = Some(error.to_string()),
                }
                self.last_capture = Some(capture);
            }
            SessionEvent::Stats(stats) => self.stats = stats,
            SessionEvent::RecordingError(error) => {
                self.last_recording_stats = self.recording_stats();
                self.recording.take();
                self.last_error = Some(error);
            }
            SessionEvent::Error(error) => self.last_error = Some(error),
        }
        Ok(capture_history_changed)
    }

    fn rebuild_buffer(&mut self) -> Result<(), String> {
        if !self.configuration_applied {
            return Ok(());
        }
        let Some(table) = &self.channel_table else {
            return Ok(());
        };
        let channel_ids = table
            .channels
            .iter()
            .filter(|channel| self.acquisition.channel_mask & (1_u64 << channel.channel_id) != 0)
            .map(|channel| channel.channel_id)
            .collect::<Vec<_>>();
        self.buffer = None;
        self.latest_display_snapshot = None;
        if !channel_ids.contains(&self.trigger.config().source_channel) {
            let mut config = self.trigger.config().clone();
            config.source_channel = *channel_ids
                .first()
                .ok_or_else(|| "channel mask selects no known channels".to_owned())?;
            self.trigger.set_config(config).map_err(error_string)?;
            if let Some(session) = &self.session {
                session
                    .set_trigger_config(self.trigger.config().clone())
                    .map_err(error_string)?;
            }
        }
        Ok(())
    }

    pub fn refresh_serial_ports(&mut self) -> Result<(), String> {
        self.serial_ports = super::transport::available_serial_ports()
            .map_err(error_string)?
            .into_iter()
            .map(|port| port.port_name)
            .collect();
        Ok(())
    }
}

impl Drop for LiveScopeState {
    fn drop(&mut self) {
        self.recording.take();
        if let Some(session) = self.session.take() {
            let _ = session.disconnect();
        }
    }
}

fn hex_device_id(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn error_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn default_channel_color(channel_id: u16) -> [u8; 4] {
    const COLORS: [[u8; 4]; 8] = [
        [32, 120, 220, 255],
        [220, 70, 70, 255],
        [30, 160, 95, 255],
        [180, 100, 210, 255],
        [230, 145, 30, 255],
        [35, 165, 175, 255],
        [210, 80, 155, 255],
        [110, 125, 145, 255],
    ];
    COLORS[usize::from(channel_id) % COLORS.len()]
}

#[cfg(test)]
mod tests {
    use std::{
        thread,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{
        data::{DataSource, SampleBlock},
        live::{
            scope_source::ScopeRecordingDataSource,
            simulator::{SimulatorConfig, SimulatorHandle},
        },
        measurements::{analyze_segments, ChannelMeasurementSpec},
    };

    fn wait_until(state: &mut LiveScopeState, predicate: impl Fn(&LiveScopeState) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            state.poll();
            if predicate(state) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for live scope state");
    }

    #[test]
    fn trigger_capture_reports_a_history_change() {
        let mut state = LiveScopeState::default();
        state
            .set_trigger_config(TriggerConfig {
                mode: TriggerMode::Auto,
                source_channel: 0,
                pre_samples: 0,
                post_samples: 0,
                auto_timeout_samples: 1,
                ..TriggerConfig::default()
            })
            .unwrap();

        let changed = state
            .handle_event(SessionEvent::TriggerCapture(
                TriggerCapture {
                    channel_ids: vec![0],
                    sample_indices: vec![0],
                    timestamps: vec![0],
                    channels: vec![vec![0.0]],
                    trigger_position: 0,
                    auto_timeout: true,
                },
                state.trigger.config().clone(),
            ))
            .unwrap();

        assert!(changed);
        assert_eq!(state.capture_history.entries().len(), 1);
    }

    #[test]
    fn simulator_acquisition_records_and_replays() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            // A paced producer exercises the same V1 recording path without
            // saturating the TCP peer before the UI has drained its events.
            accelerated: false,
            ..SimulatorConfig::default()
        })
        .unwrap();
        let path = std::env::temp_dir().join(format!(
            "scope_live_e2e_{}_{}.scope",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut state = LiveScopeState::default();
        state.transport = TransportConfig::Tcp {
            address: simulator.address().to_string(),
        };
        state.connect().unwrap();
        wait_until(&mut state, |state| {
            state.connection_state == ConnectionState::Ready
        });
        state
            .configure(Configure {
                sample_rate_hz: 10_000,
                batch_samples: 10,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_until(&mut state, |state| {
            state.configuration_applied && state.acquisition.batch_samples == 10
        });
        state.start_recording(&path).unwrap();
        state.start().unwrap();
        wait_until(&mut state, |state| {
            state
                .latest_display_snapshot
                .as_ref()
                .is_some_and(|snapshot| {
                    snapshot
                        .segments
                        .iter()
                        .map(|segment| segment.times.len())
                        .sum::<usize>()
                        >= 30
                })
        });
        wait_until(&mut state, |state| {
            state.recording_stats().sample_frames >= 3
        });
        // `AsyncScopeRecorder` uses the production 1024-record bounded
        // ingress queue; this checks that UI polling never observes more than
        // the documented queue capacity.
        assert!(state.recording_pending_records() <= 1_024);
        state.stop().unwrap();
        wait_until(&mut state, |state| {
            state.connection_state == ConnectionState::Ready
        });
        state.stop_recording().unwrap();
        assert!(state.recording_stats().sample_frames >= 3);
        state.disconnect().unwrap();

        let source = ScopeRecordingDataSource::open(&path).unwrap();
        assert!(source.metadata().sample_count >= 30);
        let block = source
            .read_range(
                source.metadata().start_time,
                source.metadata().end_time,
                &[0, 3],
                100,
            )
            .unwrap();
        assert!(!block.times.is_empty());
        assert!(block.channels[1]
            .iter()
            .all(|value| *value == 0.0 || *value == 1.0));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn simulator_stream_measure_trigger_history_and_recording_replay_end_to_end() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: false,
            drop_every: Some(10),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let path = std::env::temp_dir().join(format!(
            "scope_live_measure_trigger_e2e_{}_{}.scope",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut state = LiveScopeState::default();
        state.transport = TransportConfig::Tcp {
            address: simulator.address().to_string(),
        };
        state
            .set_trigger_config(TriggerConfig {
                mode: TriggerMode::Auto,
                source_channel: 0,
                pre_samples: 10,
                post_samples: 10,
                auto_timeout_samples: 40,
                ..TriggerConfig::default()
            })
            .unwrap();
        state.connect().unwrap();
        wait_until(&mut state, |state| {
            state.connection_state == ConnectionState::Ready
        });
        state
            .configure(Configure {
                sample_rate_hz: 10_000,
                batch_samples: 100,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_until(&mut state, |state| state.configuration_applied);
        state.start_recording(&path).unwrap();
        state.start().unwrap();
        wait_until(&mut state, |state| {
            state.capture_history.entries().len() >= 3
                && state
                    .latest_display_snapshot
                    .as_ref()
                    .is_some_and(|snapshot| {
                        snapshot
                            .segments
                            .iter()
                            .map(|segment| segment.times.len())
                            .sum::<usize>()
                            >= 1_000
                    })
        });

        let snapshot = state.snapshot(20_000).unwrap();
        let channel_zero = snapshot
            .channel_ids
            .iter()
            .position(|channel_id| *channel_id == 0)
            .unwrap();
        let segments = snapshot
            .segments
            .into_iter()
            .map(|segment| SampleBlock {
                times: segment.times,
                channels: segment.channels,
            })
            .collect::<Vec<_>>();
        let measurement = analyze_segments(
            &segments,
            &[ChannelMeasurementSpec {
                channel_index: 0,
                column: channel_zero,
                name: "Va".to_owned(),
                unit: "V".to_owned(),
                scale: 1.0,
            }],
            None,
        )
        .unwrap();
        let frequency = measurement.channels[0].frequency.as_ref().unwrap().hz;
        assert!((frequency - 50.0).abs() < 0.5, "measured {frequency} Hz");
        assert!(measurement.channels[0].rms.is_some_and(|value| value > 0.5));
        assert!(state.capture_history.entries().len() >= 3);

        state.stop().unwrap();
        wait_until(&mut state, |state| {
            state.connection_state == ConnectionState::Ready
        });
        state.stop_recording().unwrap();
        state.disconnect().unwrap();

        let recording = crate::live::recording::ScopeRecording::open(&path).unwrap();
        assert!(recording.clean_end());
        assert!(recording.triggers().len() >= 3);
        assert!(!recording.gaps().is_empty());
        assert!(!recording.sample_records().is_empty());
        let source = ScopeRecordingDataSource::open(&path).unwrap();
        let replay = source
            .read_range_segments(
                source.metadata().start_time,
                source.metadata().end_time,
                &[0],
                20_000,
            )
            .unwrap();
        assert!(replay.len() > 1);
        let replay_measurement =
            analyze_segments(&replay, &[ChannelMeasurementSpec::new(0, 0, "Va")], None).unwrap();
        let replay_frequency = replay_measurement.channels[0]
            .frequency
            .as_ref()
            .unwrap()
            .hz;
        assert!((replay_frequency - 50.0).abs() < 0.5);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn configuration_rejects_channels_not_advertised_by_the_device() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let mut state = LiveScopeState::default();
        state.transport = TransportConfig::Tcp {
            address: simulator.address().to_string(),
        };
        state.connect().unwrap();
        wait_until(&mut state, |state| {
            state.connection_state == ConnectionState::Ready
        });
        assert_eq!(state.acquisition.channel_mask, 0b1111);
        assert_eq!(
            state
                .channel_presentations
                .get(&0)
                .map(|presentation| presentation.scale),
            Some(1.0)
        );

        let result = state.configure(Configure {
            sample_rate_hz: 10_000,
            batch_samples: 10,
            channel_mask: 1 << 60,
        });

        assert!(result.is_err());
        assert!(!state.configuration_applied);
        state.disconnect().unwrap();
    }

    #[test]
    fn start_with_configuration_applies_settings_and_streams_in_one_action() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let mut state = LiveScopeState::default();
        state.transport = TransportConfig::Tcp {
            address: simulator.address().to_string(),
        };
        state.connect().unwrap();
        wait_until(&mut state, |state| {
            state.connection_state == ConnectionState::Ready
        });

        state
            .start_with_configuration(Configure {
                sample_rate_hz: 10_000,
                batch_samples: 10,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_until(&mut state, |state| {
            state.connection_state == ConnectionState::Streaming
        });

        assert!(state.configuration_applied);
        assert_eq!(state.acquisition.batch_samples, 10);
        state.disconnect().unwrap();
    }

    #[test]
    fn recording_bypasses_a_backpressured_display_queue() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let path = std::env::temp_dir().join(format!(
            "scope_live_backpressure_{}_{}.scope",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut state = LiveScopeState::default();
        state.transport = TransportConfig::Tcp {
            address: simulator.address().to_string(),
        };
        state.connect().unwrap();
        wait_until(&mut state, |state| {
            state.connection_state == ConnectionState::Ready
        });
        state
            .configure(Configure {
                sample_rate_hz: 10_000,
                batch_samples: 10,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_until(&mut state, |state| state.configuration_applied);
        state.start_recording(&path).unwrap();
        state.start().unwrap();
        wait_until(&mut state, |state| {
            state.connection_state == ConnectionState::Streaming
        });

        std::thread::sleep(Duration::from_millis(700));
        state.stop().unwrap();
        wait_until(&mut state, |state| {
            state.connection_state == ConnectionState::Ready
        });
        state.stop_recording().unwrap();
        let displayed_samples = state
            .latest_display_snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .segments
                    .iter()
                    .map(|segment| segment.times.len())
                    .sum::<usize>()
            })
            .unwrap_or(0);
        state.disconnect().unwrap();

        let source = ScopeRecordingDataSource::open(&path).unwrap();
        assert!(source.metadata().sample_count > displayed_samples as u64);
        assert!(source.metadata().sample_count >= 2_000);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unexpected_disconnect_stops_recording_and_preserves_a_recoverable_prefix() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
            disconnect_after: Some(10),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let path = std::env::temp_dir().join(format!(
            "scope_live_disconnect_{}_{}.scope",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut state = LiveScopeState::default();
        state.transport = TransportConfig::Tcp {
            address: simulator.address().to_string(),
        };
        state.connect().unwrap();
        wait_until(&mut state, |state| {
            state.connection_state == ConnectionState::Ready
        });
        state
            .configure(Configure {
                sample_rate_hz: 10_000,
                batch_samples: 10,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_until(&mut state, |state| state.configuration_applied);
        state.start_recording(&path).unwrap();
        state.start().unwrap();

        wait_until(&mut state, |state| {
            state.connection_state == ConnectionState::Disconnected
        });
        assert!(state.session.is_none());
        assert!(!state.is_recording());
        assert!(state
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("recoverable")));
        std::thread::sleep(Duration::from_millis(100));

        let recording = crate::live::recording::ScopeRecording::open(&path).unwrap();
        assert!(!recording.clean_end());
        assert!(recording.recovered_tail());
        assert!(!recording.sample_records().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn normal_trigger_capture_drives_display_until_rearmed() {
        let mut state = LiveScopeState::default();
        state.hello_ack = Some(crate::live::protocol::HelloAck {
            device_capabilities: 0,
            max_payload: 1024,
            tick_hz: 1_000,
            channel_count: 1,
            max_batch_samples: 10,
            device_id: [0; 16],
            firmware_name: "test".to_owned(),
        });
        state
            .trigger
            .set_config(crate::live::trigger::TriggerConfig {
                mode: crate::live::trigger::TriggerMode::Normal,
                source_channel: 0,
                ..crate::live::trigger::TriggerConfig::default()
            })
            .unwrap();
        state.last_capture = Some(crate::live::trigger::TriggerCapture {
            channel_ids: vec![0],
            sample_indices: vec![10, 11, 12],
            timestamps: vec![100, 101, 102],
            channels: vec![vec![-1.0, 0.0, 1.0]],
            trigger_position: 1,
            auto_timeout: false,
        });
        state.channel_presentations.insert(
            0,
            ChannelPresentation {
                display_name: "CH0".to_owned(),
                color: default_channel_color(0),
                visible: true,
                scale: 2.0,
                pane: 0,
            },
        );

        let snapshot = state.display_snapshot(100).unwrap();

        assert_eq!(snapshot.segments.len(), 1);
        assert_eq!(snapshot.segments[0].times, vec![0.1, 0.101, 0.102]);
        assert_eq!(state.scaled_display_value(0, 1.5), 3.0);
        state.arm_trigger();
        assert!(state.last_capture.is_none());
        assert!(state.trigger.is_armed());
    }
}
