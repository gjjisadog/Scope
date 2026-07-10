use std::path::{Path, PathBuf};

use super::{
    buffer::{LiveBuffer, LiveSnapshot},
    protocol::{ChannelTable, Configure, Frame, HelloAck, ResultCode},
    recording::{RecordingMetadata, ScopeWriter},
    session::{ConnectionState, LiveSession, SessionEvent, SessionStats},
    transport::TransportConfig,
    trigger::{TriggerCapture, TriggerConfig, TriggerEngine},
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
    pub history_seconds: u32,
    pub buffer: Option<LiveBuffer>,
    pub trigger: TriggerEngine,
    pub last_capture: Option<TriggerCapture>,
    pub stats: SessionStats,
    pub last_error: Option<String>,
    pub recording_path: Option<PathBuf>,
    session: Option<LiveSession>,
    recording: Option<ScopeWriter>,
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
                sample_rate_hz: 10_000,
                batch_samples: 100,
                channel_mask: u64::MAX,
            },
            history_seconds: 10,
            buffer: None,
            trigger: TriggerEngine::new(TriggerConfig::default())
                .expect("default trigger configuration is valid"),
            last_capture: None,
            stats: SessionStats::default(),
            last_error: None,
            recording_path: None,
            session: None,
            recording: None,
        }
    }
}

impl LiveScopeState {
    pub fn connect(&mut self) -> Result<(), String> {
        if self.session.is_some() {
            return Err("live session is already connected".to_owned());
        }
        self.last_error = None;
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
        Ok(())
    }

    pub fn configure(&mut self, configure: Configure) -> Result<(), String> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| "live session is not connected".to_owned())?;
        session.configure(configure.clone()).map_err(error_string)?;
        self.acquisition = configure;
        self.rebuild_buffer()?;
        Ok(())
    }

    pub fn start(&self) -> Result<(), String> {
        self.session
            .as_ref()
            .ok_or_else(|| "live session is not connected".to_owned())?
            .start()
            .map_err(error_string)
    }

    pub fn stop(&self) -> Result<(), String> {
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
        };
        self.recording = Some(ScopeWriter::create(path, metadata).map_err(error_string)?);
        self.recording_path = Some(path.to_path_buf());
        Ok(())
    }

    pub fn stop_recording(&mut self) -> Result<(), String> {
        let writer = self
            .recording
            .take()
            .ok_or_else(|| "recording is not active".to_owned())?;
        writer.finish().map_err(error_string)
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    pub fn snapshot(&self, max_points: usize) -> Option<LiveSnapshot> {
        self.buffer
            .as_ref()
            .map(|buffer| buffer.snapshot(max_points))
    }

    pub fn poll(&mut self) {
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
            SessionEvent::State(state) => self.connection_state = state,
            SessionEvent::HelloAck(hello) => self.hello_ack = Some(hello),
            SessionEvent::ChannelTable(table) => {
                self.channel_table = Some(table);
                self.rebuild_buffer()?;
            }
            SessionEvent::CommandResult(result) => {
                if result.result_code != ResultCode::Ok {
                    return Err(format!(
                        "device command {} failed: {}",
                        result.request_sequence, result.detail
                    ));
                }
            }
            SessionEvent::Batch(batch) => {
                if let Some(writer) = &mut self.recording {
                    let frame = Frame::decode(&batch.raw_frame).map_err(error_string)?;
                    writer.write_sample_frame(&frame).map_err(error_string)?;
                }
                if let Some(capture) = self.trigger.feed(&batch).map_err(error_string)? {
                    if let Some(writer) = &mut self.recording {
                        writer.write_trigger(&capture).map_err(error_string)?;
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
                if let Some(writer) = &mut self.recording {
                    writer.write_gap(gap, 0).map_err(error_string)?;
                }
            }
            SessionEvent::Stats(stats) => self.stats = stats,
            SessionEvent::Error(error) => self.last_error = Some(error),
        }
        Ok(())
    }

    fn rebuild_buffer(&mut self) -> Result<(), String> {
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
}

impl Drop for LiveScopeState {
    fn drop(&mut self) {
        if let Some(writer) = self.recording.take() {
            let _ = writer.finish();
        }
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

#[cfg(test)]
mod tests {
    use std::{
        thread,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{
        data::DataSource,
        live::{
            scope_source::ScopeRecordingDataSource,
            simulator::{SimulatorConfig, SimulatorHandle},
        },
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
        state.start_recording(&path).unwrap();
        state.start().unwrap();
        wait_until(&mut state, |state| {
            state
                .buffer
                .as_ref()
                .is_some_and(|buffer| buffer.len() >= 30)
        });
        state.stop().unwrap();
        wait_until(&mut state, |state| {
            state.connection_state == ConnectionState::Ready
        });
        state.stop_recording().unwrap();
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
}
