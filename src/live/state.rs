use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::plot_viewport::PlotViewport;
use crate::presentation::ChannelPresentation;

use super::{
    buffer::{LiveBuffer, LiveSnapshot, SnapshotSegment},
    capture_history::{CaptureHistory, CapturePayload},
    protocol::{validate_configure_for_device, ChannelTable, Configure, HelloAck, ResultCode},
    recording::{AsyncScopeRecorder, RecordingMetadata, RecordingStats},
    session::{ConnectionState, LiveSession, SessionEvent, SessionStats},
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
    pub acquisition: Configure,
    pub configuration_applied: bool,
    pub history_seconds: u32,
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
            acquisition: Configure {
                sample_rate_hz: 500,
                batch_samples: 10,
                channel_mask: u64::MAX,
            },
            configuration_applied: false,
            history_seconds: 1,
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
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| "live session is not connected".to_owned())?;
        session.configure(configure.clone()).map_err(error_string)?;
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
        self.buffer
            .as_ref()
            .map(|buffer| buffer.snapshot(max_points))
    }

    pub fn measurement_snapshot(&self, max_samples: usize) -> Option<LiveSnapshot> {
        if let Some(capture) = self.selected_trigger_capture() {
            return self.trigger_capture_snapshot(capture, max_samples);
        }
        if self.display_paused {
            return self.frozen_snapshot.clone();
        }
        self.buffer
            .as_ref()
            .map(|buffer| buffer.snapshot_recent(max_samples))
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

    /// Returns an immutable, full-resolution analysis input. A completed
    /// trigger capture takes precedence; otherwise the currently frozen view
    /// or the full retained ring buffer is used.
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
    }

    pub fn set_trigger_config(&mut self, config: TriggerConfig) -> Result<(), String> {
        self.trigger.set_config(config).map_err(error_string)?;
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

    pub fn poll(&mut self) {
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
        for event in events {
            if let Err(error) = self.handle_event(event) {
                self.last_error = Some(error);
            }
        }
    }

    fn handle_event(&mut self, event: SessionEvent) -> Result<(), String> {
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
            SessionEvent::CommandResult(result) => {
                if result.result_code != ResultCode::Ok {
                    self.start_after_configure = false;
                    return Err(format!(
                        "device command {} failed: {}",
                        result.request_sequence, result.detail
                    ));
                }
            }
            SessionEvent::Batch(batch) => {
                for capture in self.trigger.feed_all(&batch).map_err(error_string)? {
                    if let Some(writer) = &mut self.recording {
                        if let Err(error) =
                            writer.try_write_trigger(capture.clone(), self.trigger.config().clone())
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
                    if let Err(error) = self.capture_history.insert_live(
                        capture.clone(),
                        self.trigger.config().clone(),
                        created_unix_ms,
                        !self.keep_capture_selection,
                    ) {
                        self.last_error = Some(error.to_string());
                    }
                    self.last_capture = Some(capture);
                }
                let buffer = self
                    .buffer
                    .as_mut()
                    .ok_or_else(|| "live buffer is not initialized".to_owned())?;
                buffer.push_batch(batch).map_err(error_string)?;
            }
            SessionEvent::Gap(gap) => {
                self.trigger.on_gap();
                if let Some(buffer) = &mut self.buffer {
                    buffer.push_gap(gap.start_sample_index, gap.missing_samples, gap.reason);
                }
            }
            SessionEvent::Stats(stats) => self.stats = stats,
            SessionEvent::RecordingError(error) => {
                self.last_recording_stats = self.recording_stats();
                self.recording.take();
                self.last_error = Some(error);
            }
            SessionEvent::Error(error) => self.last_error = Some(error),
        }
        Ok(())
    }

    fn rebuild_buffer(&mut self) -> Result<(), String> {
        if !self.configuration_applied {
            return Ok(());
        }
        let Some(table) = &self.channel_table else {
            return Ok(());
        };
        let Some(hello) = &self.hello_ack else {
            return Ok(());
        };
        let channel_ids = table
            .channels
            .iter()
            .filter(|channel| self.acquisition.channel_mask & (1_u64 << channel.channel_id) != 0)
            .map(|channel| channel.channel_id)
            .collect::<Vec<_>>();
        let capacity = u64::from(self.acquisition.sample_rate_hz)
            .checked_mul(u64::from(self.history_seconds))
            .ok_or_else(|| "live history capacity overflow".to_owned())?
            .clamp(1, 5_000_000) as usize;
        self.buffer = Some(
            LiveBuffer::new(channel_ids.clone(), capacity, hello.tick_hz).map_err(error_string)?,
        );
        if !channel_ids.contains(&self.trigger.config().source_channel) {
            let mut config = self.trigger.config().clone();
            config.source_channel = *channel_ids
                .first()
                .ok_or_else(|| "channel mask selects no known channels".to_owned())?;
            self.trigger.set_config(config).map_err(error_string)?;
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
    fn simulator_acquisition_records_and_replays() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
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
                .buffer
                .as_ref()
                .is_some_and(|buffer| buffer.len() >= 30)
        });
        wait_until(&mut state, |state| {
            state.recording_stats().sample_frames >= 3
        });
        assert!(state.recording_pending_records() <= 128);
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
                    .buffer
                    .as_ref()
                    .is_some_and(|buffer| buffer.len() >= 1_000)
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
            .buffer
            .as_ref()
            .map(|buffer| buffer.len())
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
