//! A bounded, evidence-producing live acquisition smoke test.
//!
//! This module deliberately uses the same `LiveSession` and recording path as
//! the desktop application. It can be pointed at a real serial device (or a
//! TCP bridge) by an acceptance runner, while the unit test uses only the
//! protocol simulator and is never reported as hardware evidence.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use crossbeam_channel::RecvTimeoutError;
use serde::Serialize;
use thiserror::Error;

use super::{
    protocol::{validate_configure_for_device, ChannelTable, Configure, HelloAck, ResultCode},
    recording::{AsyncScopeRecorder, RecordingError, RecordingMetadata, ScopeRecording},
    session::{ConnectionState, LiveSession, SessionError, SessionEvent},
    transport::TransportConfig,
};

const DEFAULT_SAMPLE_RATE_HZ: u32 = 500;
const DEFAULT_BATCH_SAMPLES: u16 = 16;
const MAX_SMOKE_DURATION: Duration = Duration::from_secs(60);
const EVENT_POLL_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone, Debug)]
pub struct HardwareSmokeConfig {
    pub transport: TransportConfig,
    pub output: PathBuf,
    pub duration: Duration,
    pub sample_rate_hz: Option<u32>,
    pub batch_samples: Option<u16>,
    pub channel_count: usize,
}

impl HardwareSmokeConfig {
    pub fn validate(&self) -> Result<(), HardwareSmokeError> {
        self.transport
            .validate()
            .map_err(|error| HardwareSmokeError::InvalidConfig(error.to_string()))?;
        if self.output.as_os_str().is_empty() {
            return Err(HardwareSmokeError::InvalidConfig(
                "output path must not be empty".to_owned(),
            ));
        }
        if self.duration.is_zero() || self.duration > MAX_SMOKE_DURATION {
            return Err(HardwareSmokeError::InvalidConfig(format!(
                "duration must be within 1ms..={}s",
                MAX_SMOKE_DURATION.as_secs()
            )));
        }
        if self.sample_rate_hz == Some(0) {
            return Err(HardwareSmokeError::InvalidConfig(
                "sample rate must be greater than zero".to_owned(),
            ));
        }
        if self.batch_samples == Some(0) {
            return Err(HardwareSmokeError::InvalidConfig(
                "batch samples must be greater than zero".to_owned(),
            ));
        }
        if self.channel_count == 0 || self.channel_count > 64 {
            return Err(HardwareSmokeError::InvalidConfig(
                "channel count must be within 1..=64".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HardwareSmokeResult {
    pub transport: String,
    pub output: String,
    pub device_id: String,
    pub firmware_name: String,
    pub tick_hz: u64,
    pub sample_rate_hz: u32,
    pub batch_samples: u16,
    pub channel_mask: u64,
    pub duration_ms: u64,
    pub batch_events: u64,
    pub gap_events: u64,
    pub sample_count: u64,
    pub gap_records: usize,
    pub clean_end: bool,
}

#[derive(Debug, Error)]
pub enum HardwareSmokeError {
    #[error("invalid hardware smoke configuration: {0}")]
    InvalidConfig(String),
    #[error("hardware smoke session error: {0}")]
    Session(#[from] SessionError),
    #[error("hardware smoke recording error: {0}")]
    Recording(#[from] RecordingError),
    #[error("hardware smoke timed out while waiting for {0}")]
    Timeout(&'static str),
    #[error("hardware device reported an error: {0}")]
    Device(String),
}

pub fn run(config: &HardwareSmokeConfig) -> Result<HardwareSmokeResult, HardwareSmokeError> {
    config.validate()?;
    let session = LiveSession::connect(config.transport.clone())?;
    let result = run_connected(&session, config);
    let disconnect_result = session.disconnect();
    match result {
        Err(error) => Err(error),
        Ok(result) => {
            disconnect_result?;
            Ok(result)
        }
    }
}

fn run_connected(
    session: &LiveSession,
    config: &HardwareSmokeConfig,
) -> Result<HardwareSmokeResult, HardwareSmokeError> {
    let (hello, table) = wait_for_handshake(session)?;
    let configure = choose_configuration(config, &hello, &table)?;
    session.configure(configure.clone())?;
    wait_for_configured(session)?;

    let metadata = RecordingMetadata {
        device_id: hex_device_id(&hello.device_id),
        firmware_name: hello.firmware_name.clone(),
        tick_hz: hello.tick_hz,
        channel_table: table,
        sample_rate_hz: configure.sample_rate_hz,
        batch_samples: configure.batch_samples,
        channel_mask: configure.channel_mask,
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        channel_presentations: Default::default(),
    };
    let recorder = AsyncScopeRecorder::create(&config.output, metadata)?;
    let ingress = recorder.ingress()?;
    if let Err(error) = session.set_recording(Some(ingress)) {
        let _ = recorder.abort();
        return Err(error.into());
    }
    if let Err(error) = session.start() {
        let _ = session.set_recording(None);
        let _ = recorder.abort();
        return Err(error.into());
    }

    let (batch_events, gap_events) = match capture_for_duration(session, config.duration) {
        Ok(result) => result,
        Err(error) => {
            let _ = session.stop();
            let _ = session.set_recording(None);
            let _ = recorder.abort();
            return Err(error);
        }
    };
    session.stop()?;
    wait_for_ready(session)?;
    session.set_recording(None)?;
    let _stats = recorder.finish()?;

    let recording = ScopeRecording::open(&config.output)?;
    let sample_count = recording
        .sample_records()
        .iter()
        .map(|record| u64::from(record.sample_count))
        .sum::<u64>();
    if !recording.clean_end() {
        return Err(HardwareSmokeError::Device(
            "recording did not contain a clean SessionEnd".to_owned(),
        ));
    }
    if sample_count == 0 {
        return Err(HardwareSmokeError::Device(
            "device produced no sample frames during smoke duration".to_owned(),
        ));
    }
    Ok(HardwareSmokeResult {
        transport: transport_label(&config.transport),
        output: config.output.display().to_string(),
        device_id: hex_device_id(&hello.device_id),
        firmware_name: hello.firmware_name,
        tick_hz: hello.tick_hz,
        sample_rate_hz: configure.sample_rate_hz,
        batch_samples: configure.batch_samples,
        channel_mask: configure.channel_mask,
        duration_ms: config.duration.as_millis().try_into().unwrap_or(u64::MAX),
        batch_events,
        gap_events,
        sample_count,
        gap_records: recording.gaps().len(),
        clean_end: recording.clean_end(),
    })
}

fn transport_label(transport: &TransportConfig) -> String {
    match transport {
        TransportConfig::Serial { port, baud } => format!("serial:{port}@{baud}"),
        TransportConfig::Tcp { address } => format!("tcp:{address}"),
    }
}

fn choose_configuration(
    config: &HardwareSmokeConfig,
    hello: &HelloAck,
    table: &ChannelTable,
) -> Result<Configure, HardwareSmokeError> {
    let channel_count = config.channel_count.min(table.channels.len());
    let channel_mask = table
        .channels
        .iter()
        .take(channel_count)
        .fold(0_u64, |mask, channel| mask | (1_u64 << channel.channel_id));
    let max_rate = hello.tick_hz.min(u64::from(u32::MAX)) as u32;
    let sample_rate_hz = config
        .sample_rate_hz
        .unwrap_or(DEFAULT_SAMPLE_RATE_HZ)
        .min(max_rate)
        .max(1);
    let batch_samples = config
        .batch_samples
        .unwrap_or(DEFAULT_BATCH_SAMPLES)
        .min(hello.max_batch_samples)
        .max(1);
    let configure = Configure {
        sample_rate_hz,
        batch_samples,
        channel_mask,
    };
    validate_configure_for_device(&configure, hello, table)
        .map_err(|error| HardwareSmokeError::Device(error.to_string()))?;
    Ok(configure)
}

fn wait_for_handshake(
    session: &LiveSession,
) -> Result<(HelloAck, ChannelTable), HardwareSmokeError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut hello = None;
    let mut table = None;
    loop {
        match recv_until(session, deadline, "device handshake")? {
            SessionEvent::HelloAck(value) => hello = Some(value),
            SessionEvent::ChannelTable(value) => table = Some(value),
            SessionEvent::Error(error) | SessionEvent::RecordingError(error) => {
                return Err(HardwareSmokeError::Device(error));
            }
            SessionEvent::State(ConnectionState::Disconnected) => {
                return Err(HardwareSmokeError::Device(
                    "device disconnected during handshake".to_owned(),
                ));
            }
            SessionEvent::State(ConnectionState::Ready) if hello.is_some() && table.is_some() => {
                return Ok((hello.expect("checked above"), table.expect("checked above")));
            }
            _ => {}
        }
    }
}

fn wait_for_configured(session: &LiveSession) -> Result<(), HardwareSmokeError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match recv_until(session, deadline, "device configuration")? {
            SessionEvent::Configured(_) => return Ok(()),
            SessionEvent::CommandResult(result) if result.result_code != ResultCode::Ok => {
                return Err(HardwareSmokeError::Device(result.detail));
            }
            SessionEvent::Error(error) | SessionEvent::RecordingError(error) => {
                return Err(HardwareSmokeError::Device(error));
            }
            SessionEvent::State(ConnectionState::Disconnected) => {
                return Err(HardwareSmokeError::Device(
                    "device disconnected during configuration".to_owned(),
                ));
            }
            _ => {}
        }
    }
}

fn wait_for_ready(session: &LiveSession) -> Result<(), HardwareSmokeError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match recv_until(session, deadline, "device stop")? {
            SessionEvent::State(ConnectionState::Ready) => return Ok(()),
            SessionEvent::CommandResult(result) if result.result_code != ResultCode::Ok => {
                return Err(HardwareSmokeError::Device(result.detail));
            }
            SessionEvent::Error(error) | SessionEvent::RecordingError(error) => {
                return Err(HardwareSmokeError::Device(error));
            }
            SessionEvent::State(ConnectionState::Disconnected) => {
                return Err(HardwareSmokeError::Device(
                    "device disconnected while stopping".to_owned(),
                ));
            }
            _ => {}
        }
    }
}

fn capture_for_duration(
    session: &LiveSession,
    duration: Duration,
) -> Result<(u64, u64), HardwareSmokeError> {
    let deadline = Instant::now() + duration;
    let mut batch_events = 0_u64;
    let mut gap_events = 0_u64;
    while Instant::now() < deadline {
        match session.recv_timeout(EVENT_POLL_TIMEOUT) {
            // The acquisition worker keeps raw batches and gaps off the UI
            // queue. Its periodic statistics retain the same evidence for
            // the smoke result without reintroducing batch work here.
            Ok(SessionEvent::Stats(stats)) => {
                batch_events = batch_events.max(stats.received_batches);
                gap_events = gap_events.max(stats.sequence_gaps);
            }
            Ok(SessionEvent::Error(error) | SessionEvent::RecordingError(error)) => {
                return Err(HardwareSmokeError::Device(error));
            }
            Ok(SessionEvent::State(ConnectionState::Disconnected)) => {
                return Err(HardwareSmokeError::Device(
                    "device disconnected during acquisition".to_owned(),
                ));
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(HardwareSmokeError::Device(
                    "session event channel disconnected during acquisition".to_owned(),
                ));
            }
        }
    }
    Ok((batch_events, gap_events))
}

fn recv_until(
    session: &LiveSession,
    deadline: Instant,
    phase: &'static str,
) -> Result<SessionEvent, HardwareSmokeError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(HardwareSmokeError::Timeout(phase));
    }
    session
        .recv_timeout(remaining)
        .map_err(|error| match error {
            RecvTimeoutError::Timeout => HardwareSmokeError::Timeout(phase),
            RecvTimeoutError::Disconnected => HardwareSmokeError::Device(format!(
                "session event channel disconnected while waiting for {phase}"
            )),
        })
}

fn hex_device_id(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::live::simulator::{SimulatorConfig, SimulatorHandle};

    #[test]
    fn simulator_smoke_writes_clean_recording() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            sample_rate_hz: 1_000,
            batch_samples: 10,
            ..SimulatorConfig::default()
        })
        .unwrap();
        let output = std::env::temp_dir().join(format!(
            "scope-hardware-smoke-{}-{}.scope",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let result = run(&HardwareSmokeConfig {
            transport: TransportConfig::Tcp {
                address: simulator.address().to_string(),
            },
            output: output.clone(),
            duration: Duration::from_millis(80),
            sample_rate_hz: Some(1_000),
            batch_samples: Some(10),
            channel_count: 2,
        })
        .unwrap();
        assert!(result.clean_end);
        assert!(result.sample_count > 0);
        let recording = ScopeRecording::open(&output).unwrap();
        assert!(recording.clean_end());
        fs::remove_file(output).unwrap();
    }
}
