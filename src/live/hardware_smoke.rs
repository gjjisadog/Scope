//! A bounded, evidence-producing live acquisition smoke test.
//!
//! This module deliberately uses the same `LiveSession` and recording path as
//! the desktop application. It can be pointed at a real serial device (or a
//! TCP bridge) by an acceptance runner, while the unit test uses only the
//! protocol simulator and is never reported as hardware evidence.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::{Duration, Instant},
};

use crossbeam_channel::RecvTimeoutError;
use serde::Serialize;
use thiserror::Error;

use super::{
    machine_profile::{MachineProfile, ProfileError},
    protocol::{
        validate_configure_for_device, ChannelTable, Configure, HelloAck, ResultCode, WireFormat,
        FRAME_CRC_LEN, FRAME_HEADER_LEN,
    },
    protocol_v2::SampleDomain,
    protocol_v2_r2::{
        stream_sample_batch_r2_payload_len, ConfigureStreamsR2, MetadataEncodingR2,
        StreamSampleBatchR2, StreamSubscriptionR2, StreamTableR2,
        CAPABILITY_V2_COMPRESSED_METADATA, CAPABILITY_V2_MULTI_STREAM, CAPABILITY_V2_STREAMS_R2,
    },
    recording::{AsyncScopeRecorder, RecordingError, RecordingMetadata, ScopeRecording},
    session::{
        ConnectionState, LiveSession, SessionError, SessionEvent, SessionStats, StreamSessionStats,
    },
    snapshot::SnapshotMeta,
    transport::TransportConfig,
};

const DEFAULT_SAMPLE_RATE_HZ: u32 = 500;
const DEFAULT_BATCH_SAMPLES: u16 = 16;
const MAX_SMOKE_DURATION: Duration = Duration::from_secs(60);
const EVENT_POLL_TIMEOUT: Duration = Duration::from_millis(100);
const SAFE_SERIAL_UTILIZATION_LIMIT: f64 = 0.70;
const MINIMUM_THROUGHPUT_RATIO: f64 = 0.95;
const MINIMUM_LINK_STRESS_DURATION: Duration = Duration::from_secs(5);

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
    #[error("hardware smoke profile error: {0}")]
    Profile(#[from] ProfileError),
    #[error("hardware smoke timed out while waiting for {0}")]
    Timeout(&'static str),
    #[error("hardware device reported an error: {0}")]
    Device(String),
    #[error("hardware smoke contract {code}: {message}")]
    Contract { code: &'static str, message: String },
}

impl HardwareSmokeError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfig(_) => "invalid_config",
            Self::Session(_) => "session_error",
            Self::Recording(_) => "recording_error",
            Self::Profile(error) => error.code(),
            Self::Timeout(_) => "timeout",
            Self::Device(_) => "device_error",
            Self::Contract { code, .. } => code,
        }
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardwareSmokeMode {
    Handshake,
    Ctrl8k,
    Multistream,
    LinkStress,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChannelSet {
    #[default]
    Required,
    All,
}

impl ChannelSet {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "required" => Some(Self::Required),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::All => "all",
        }
    }
}

impl HardwareSmokeMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "handshake" => Some(Self::Handshake),
            "ctrl8k" => Some(Self::Ctrl8k),
            "multistream" => Some(Self::Multistream),
            "link-stress" => Some(Self::LinkStress),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Handshake => "handshake",
            Self::Ctrl8k => "ctrl8k",
            Self::Multistream => "multistream",
            Self::LinkStress => "link-stress",
        }
    }
}

#[derive(Clone, Debug)]
pub struct HardwareSmokeV2R2Config {
    pub transport: TransportConfig,
    pub profile: String,
    pub mode: HardwareSmokeMode,
    pub channel_set: ChannelSet,
    pub duration: Duration,
    pub batch_samples: u16,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct HardwareSmokeStreamResult {
    pub stream_id: u16,
    pub domain: String,
    pub selected_channel_count: usize,
    pub selected_channel_mask: u64,
    #[serde(skip)]
    pub selected_batch_samples: u16,
    pub received_batches: u64,
    pub expected_rows: u64,
    pub received_rows: u64,
    pub throughput_ratio: f64,
    pub minimum_throughput_ratio: f64,
    pub row_gaps: u64,
    pub row_reorders: u64,
    pub invalid_rows: u64,
    pub host_dropped_rows: u64,
    pub device_dropped_rows: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct R2SerialLinkBudget {
    pub payload_bytes_per_second: f64,
    pub wire_bytes_per_second: f64,
    pub required_baud: u64,
    pub configured_baud: Option<u32>,
    pub estimated_utilization: f64,
    pub safe_utilization_limit: f64,
    pub headroom: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct HardwareSmokeV2R2Result {
    pub protocol: &'static str,
    pub protocol_revision: &'static str,
    pub mode: &'static str,
    pub channel_set: &'static str,
    pub profile: String,
    pub transport: String,
    pub baud: Option<u32>,
    pub link_budget: R2SerialLinkBudget,
    pub session_id: u32,
    pub device_id: String,
    pub firmware_name: String,
    pub capabilities: u32,
    pub channel_table_revision: u32,
    pub stream_table_revision: u32,
    pub stream_count: usize,
    pub heartbeat_round_trip_count: u64,
    pub heartbeat_last_rtt_ms: u64,
    pub crc_errors: u64,
    pub protocol_errors: u64,
    pub duration_ms: u64,
    pub received_frames: u64,
    pub received_batches: u64,
    pub received_rows: u64,
    pub host_dropped_batches: u64,
    pub host_dropped_rows: u64,
    pub device_dropped_samples: u64,
    pub device_tx_overruns: u64,
    pub heartbeat_timeout_count: u64,
    pub effective_payload_bytes: u64,
    pub estimated_link_utilization: f64,
    #[serde(rename = "average_data_rate_Bps")]
    pub average_data_rate_bps: f64,
    pub streams: Vec<HardwareSmokeStreamResult>,
    pub ready_after_stop: bool,
    pub ok: bool,
}

pub fn estimate_r2_serial_budget(
    configured_baud: Option<u32>,
    subscriptions: &[StreamSubscriptionR2],
    streams: &StreamTableR2,
    channels: &ChannelTable,
) -> Result<R2SerialLinkBudget, HardwareSmokeError> {
    let mut payload_bytes_per_second = 0.0;
    let mut wire_bytes_per_second = 0.0;
    for subscription in subscriptions {
        let descriptor = streams.stream(subscription.stream_id).ok_or_else(|| {
            HardwareSmokeError::InvalidConfig(format!(
                "link budget references unknown stream {}",
                subscription.stream_id
            ))
        })?;
        let channel_ids = descriptor
            .channel_ids
            .iter()
            .copied()
            .filter(|channel_id| subscription.channel_mask & (1_u64 << channel_id) != 0)
            .collect::<Vec<_>>();
        let bytes_per_row = channel_ids.iter().try_fold(0_usize, |total, channel_id| {
            let channel = channels.channel(*channel_id).ok_or_else(|| {
                HardwareSmokeError::InvalidConfig(format!(
                    "link budget references unknown channel {channel_id}"
                ))
            })?;
            total
                .checked_add(wire_format_bytes(channel.wire_format))
                .ok_or_else(|| HardwareSmokeError::InvalidConfig("link budget overflow".to_owned()))
        })?;
        let row_count = usize::from(subscription.batch_samples);
        let sample_data_len = bytes_per_row
            .checked_mul(row_count)
            .ok_or_else(|| HardwareSmokeError::InvalidConfig("link budget overflow".to_owned()))?;
        let row_metadata = (0..subscription.batch_samples)
            .map(|offset| {
                let row_sequence = u64::from(offset) + 1;
                let logical_cycle_sequence =
                    row_sequence.saturating_mul(u64::from(descriptor.logical_cycle_step));
                SnapshotMeta {
                    row_sequence,
                    logical_cycle_sequence,
                    source_sequence: logical_cycle_sequence,
                    applied_sequence: logical_cycle_sequence,
                    valid_flags: 0,
                }
            })
            .collect::<Vec<_>>();
        let batch = StreamSampleBatchR2 {
            stream_id: descriptor.stream_id,
            stream_revision: streams.revision,
            domain: descriptor.domain,
            capture_phase: descriptor.capture_phase,
            consistency_group: descriptor.consistency_group,
            first_row_sequence: 1,
            row_sequence_step: 1,
            logical_cycle_step: descriptor.logical_cycle_step,
            sample_period_ticks: 1,
            row_count: subscription.batch_samples,
            channel_ids,
            sample_data: vec![0; sample_data_len],
            metadata_encoding: MetadataEncodingR2::AffineWithOverrides,
            row_metadata,
        };
        let payload_len = stream_sample_batch_r2_payload_len(&batch)
            .map_err(|error| HardwareSmokeError::InvalidConfig(error.to_string()))?;
        let frames_per_second =
            f64::from(descriptor.sample_rate_hz) / f64::from(subscription.batch_samples);
        payload_bytes_per_second += payload_len as f64 * frames_per_second;
        wire_bytes_per_second +=
            (FRAME_HEADER_LEN + payload_len + FRAME_CRC_LEN) as f64 * frames_per_second;
    }
    let required_baud = (wire_bytes_per_second * 10.0).ceil() as u64;
    let estimated_utilization = configured_baud
        .filter(|baud| *baud > 0)
        .map(|baud| required_baud as f64 / f64::from(baud))
        .unwrap_or(0.0);
    Ok(R2SerialLinkBudget {
        payload_bytes_per_second,
        wire_bytes_per_second,
        required_baud,
        configured_baud,
        estimated_utilization,
        safe_utilization_limit: SAFE_SERIAL_UTILIZATION_LIMIT,
        headroom: configured_baud
            .map(|_| SAFE_SERIAL_UTILIZATION_LIMIT - estimated_utilization)
            .unwrap_or(0.0),
    })
}

fn enforce_serial_link_budget(budget: &R2SerialLinkBudget) -> Result<(), HardwareSmokeError> {
    if let Some(configured_baud) = budget.configured_baud {
        if budget.estimated_utilization > budget.safe_utilization_limit {
            return contract(
                "link_budget_exceeded",
                format!(
                    "required_baud={} configured_baud={} estimated_utilization={:.6}",
                    budget.required_baud, configured_baud, budget.estimated_utilization
                ),
            );
        }
    }
    Ok(())
}

fn select_r2_subscriptions(
    mode: HardwareSmokeMode,
    channel_set: ChannelSet,
    requested_batch_samples: u16,
    max_batch_samples: u16,
    profile: &MachineProfile,
    channels: &ChannelTable,
    streams: &StreamTableR2,
) -> Result<Vec<StreamSubscriptionR2>, HardwareSmokeError> {
    let selected = match mode {
        HardwareSmokeMode::Handshake => Vec::new(),
        HardwareSmokeMode::Ctrl8k => vec![SampleDomain::Control8k],
        HardwareSmokeMode::Multistream | HardwareSmokeMode::LinkStress => {
            vec![SampleDomain::Control8k, SampleDomain::Slow1k]
        }
    };
    selected
        .iter()
        .map(|domain| {
            let stream = streams
                .streams
                .iter()
                .find(|value| value.domain == *domain)
                .ok_or_else(|| HardwareSmokeError::Contract {
                    code: "profile_missing_stream",
                    message: format!("missing {} stream", domain_name(*domain)),
                })?;
            let profile_stream = profile.stream(*domain).ok_or_else(|| {
                HardwareSmokeError::InvalidConfig(format!(
                    "profile has no {} contract",
                    domain_name(*domain)
                ))
            })?;
            let mut requested_names = match channel_set {
                ChannelSet::Required => profile_stream.required_channels.clone(),
                ChannelSet::All => profile_stream
                    .required_channels
                    .iter()
                    .chain(&profile_stream.optional_channels)
                    .cloned()
                    .collect(),
            };
            if requested_names.is_empty()
                && *domain == SampleDomain::Slow1k
                && matches!(
                    mode,
                    HardwareSmokeMode::Multistream | HardwareSmokeMode::LinkStress
                )
            {
                if let Some(name) = profile_stream.optional_channels.iter().find(|name| {
                    stream.channel_ids.iter().any(|channel_id| {
                        channels
                            .channel(*channel_id)
                            .is_some_and(|channel| channel.name == name.as_str())
                    })
                }) {
                    requested_names.push(name.clone());
                }
            }
            let requested_names = requested_names.iter().collect::<BTreeSet<_>>();
            let channel_mask = stream
                .channel_ids
                .iter()
                .filter_map(|channel_id| {
                    channels
                        .channel(*channel_id)
                        .filter(|channel| requested_names.contains(&channel.name))
                })
                .fold(0_u64, |mask, channel| mask | (1_u64 << channel.channel_id));
            if channel_mask == 0 {
                return Err(HardwareSmokeError::Contract {
                    code: "profile_missing_channel",
                    message: format!("{} has no selectable profile channels", stream.stream_id),
                });
            }
            let batch_samples = if selected.len() > 1 {
                let ctrl_rows = requested_batch_samples.max(8).next_multiple_of(8);
                match domain {
                    SampleDomain::Control8k => ctrl_rows,
                    SampleDomain::Slow1k => ctrl_rows / 8,
                    SampleDomain::Fast32k => ctrl_rows.saturating_mul(4),
                }
            } else {
                requested_batch_samples
            };
            Ok(StreamSubscriptionR2 {
                stream_id: stream.stream_id,
                batch_samples: batch_samples.min(max_batch_samples),
                channel_mask,
            })
        })
        .collect()
}

const fn wire_format_bytes(format: WireFormat) -> usize {
    match format {
        WireFormat::I16 => 2,
        WireFormat::I32 | WireFormat::F32 => 4,
        WireFormat::U8 => 1,
    }
}

pub fn run_v2_r2(
    config: &HardwareSmokeV2R2Config,
) -> Result<HardwareSmokeV2R2Result, HardwareSmokeError> {
    config
        .transport
        .validate()
        .map_err(|error| HardwareSmokeError::InvalidConfig(error.to_string()))?;
    if config.profile.trim().is_empty() {
        return Err(HardwareSmokeError::InvalidConfig(
            "--profile is required for v2-r2".to_owned(),
        ));
    }
    if config.duration.is_zero() || config.duration > MAX_SMOKE_DURATION {
        return Err(HardwareSmokeError::InvalidConfig(
            "duration must be within 1ms..=60s".to_owned(),
        ));
    }
    if config.mode == HardwareSmokeMode::LinkStress
        && config.duration < MINIMUM_LINK_STRESS_DURATION
    {
        return Err(HardwareSmokeError::InvalidConfig(
            "link-stress duration must be at least 5s".to_owned(),
        ));
    }
    if config.batch_samples == 0 {
        return Err(HardwareSmokeError::InvalidConfig(
            "batch samples must be greater than zero".to_owned(),
        ));
    }

    let profile = MachineProfile::load_named(&config.profile)?;
    let session = LiveSession::connect_v2_r2(config.transport.clone())?;
    let result = run_v2_r2_connected(&session, config, &profile);
    let disconnect_result = session.disconnect();
    match result {
        Err(error) => Err(error),
        Ok(result) => {
            disconnect_result?;
            Ok(result)
        }
    }
}

fn run_v2_r2_connected(
    session: &LiveSession,
    config: &HardwareSmokeV2R2Config,
    profile: &MachineProfile,
) -> Result<HardwareSmokeV2R2Result, HardwareSmokeError> {
    let (hello, channels, streams, mut stats) = wait_for_v2_r2_handshake(session)?;
    let required =
        CAPABILITY_V2_STREAMS_R2 | CAPABILITY_V2_MULTI_STREAM | CAPABILITY_V2_COMPRESSED_METADATA;
    if hello.device_capabilities & required != required {
        return contract(
            "profile_capability_mismatch",
            format!(
                "device capabilities {:#010x} do not include {required:#010x}",
                hello.device_capabilities
            ),
        );
    }
    if stats.session_id == 0 {
        return contract("session_id_zero", "device returned a zero session_id");
    }
    channels
        .validate()
        .map_err(|error| HardwareSmokeError::Device(error.to_string()))?;
    streams
        .validate_against_channels(&channels)
        .map_err(|error| HardwareSmokeError::Device(error.to_string()))?;
    profile.validate_compatibility(hello.device_capabilities, &channels, &streams)?;

    let mut selected = Vec::new();
    match config.mode {
        HardwareSmokeMode::Handshake => {}
        HardwareSmokeMode::Ctrl8k => selected.push(SampleDomain::Control8k),
        HardwareSmokeMode::Multistream | HardwareSmokeMode::LinkStress => {
            selected.extend([SampleDomain::Control8k, SampleDomain::Slow1k]);
        }
    }

    let mut effective_payload_bytes = 0_u64;
    let mut measured_streaming_duration = Duration::ZERO;
    let subscriptions = select_r2_subscriptions(
        config.mode,
        config.channel_set,
        config.batch_samples,
        hello.max_batch_samples,
        profile,
        &channels,
        &streams,
    )?;
    let configured_baud = match &config.transport {
        TransportConfig::Serial { baud, .. } => Some(*baud),
        TransportConfig::Tcp { .. } => None,
    };
    let link_budget =
        estimate_r2_serial_budget(configured_baud, &subscriptions, &streams, &channels)?;
    let mut ready_after_stop = config.mode == HardwareSmokeMode::Handshake;
    if !subscriptions.is_empty() {
        session.configure_streams_r2(ConfigureStreamsR2 {
            transaction_id: 1,
            subscriptions: subscriptions.clone(),
        })?;
        wait_for_v2_r2_configured(session)?;
        enforce_serial_link_budget(&link_budget)?;
        session.start()?;
        let capture = collect_v2_r2(session, config.duration, &streams, hello.tick_hz, stats)?;
        stats = capture.stats;
        effective_payload_bytes = capture.effective_payload_bytes;
        measured_streaming_duration = capture.measured_streaming_duration;
        session.stop()?;
        wait_for_ready(session)?;
        ready_after_stop = true;
    }

    let duration_ms = config.duration.as_millis().try_into().unwrap_or(u64::MAX);
    let elapsed_seconds = config.duration.as_secs_f64();
    let average_data_rate_bps = if elapsed_seconds > 0.0 {
        effective_payload_bytes as f64 / elapsed_seconds
    } else {
        0.0
    };
    let baud = configured_baud;
    let estimated_link_utilization = baud
        .map(|value| average_data_rate_bps * 10.0 / f64::from(value))
        .unwrap_or(0.0);
    let stream_results = selected
        .into_iter()
        .filter_map(|domain| {
            let descriptor = streams
                .streams
                .iter()
                .find(|value| value.domain == domain)?;
            let value = stats
                .stream_stats
                .iter()
                .find(|value| value.stream_id == descriptor.stream_id)
                .copied()
                .unwrap_or_default();
            let subscription = subscriptions
                .iter()
                .find(|value| value.stream_id == descriptor.stream_id)?;
            Some(stream_result(
                domain,
                value,
                subscription,
                descriptor.sample_rate_hz,
                measured_streaming_duration,
                stats.invalid_snapshot_rows,
                stats.device_dropped_samples,
            ))
        })
        .collect::<Vec<_>>();
    ensure_clean_v2_r2(config.mode, &stats, &stream_results)?;
    let received_rows = stream_results.iter().map(|value| value.received_rows).sum();

    Ok(HardwareSmokeV2R2Result {
        protocol: "scp1",
        protocol_revision: "v2-r2",
        mode: config.mode.name(),
        channel_set: config.channel_set.name(),
        profile: profile.profile_name.clone(),
        transport: transport_label(&config.transport),
        baud,
        link_budget,
        session_id: stats.session_id,
        device_id: hex_device_id(&hello.device_id),
        firmware_name: hello.firmware_name,
        capabilities: hello.device_capabilities,
        channel_table_revision: channels.revision,
        stream_table_revision: streams.revision,
        stream_count: streams.streams.len(),
        heartbeat_round_trip_count: stats.heartbeat_round_trip_count,
        heartbeat_last_rtt_ms: stats.heartbeat_last_rtt_ms,
        crc_errors: stats.crc_errors,
        protocol_errors: stats.protocol_errors,
        duration_ms,
        received_frames: stats.received_frames,
        received_batches: stats.received_batches,
        received_rows,
        host_dropped_batches: stats.host_dropped_v2_batches,
        host_dropped_rows: stats.host_dropped_v2_rows,
        device_dropped_samples: stats.device_dropped_samples,
        device_tx_overruns: stats.device_tx_overruns,
        heartbeat_timeout_count: stats.heartbeat_timeout_count,
        effective_payload_bytes,
        estimated_link_utilization,
        average_data_rate_bps,
        streams: stream_results,
        ready_after_stop,
        ok: true,
    })
}

struct V2CaptureStats {
    stats: SessionStats,
    effective_payload_bytes: u64,
    measured_streaming_duration: Duration,
}

fn wait_for_v2_r2_handshake(
    session: &LiveSession,
) -> Result<(HelloAck, ChannelTable, StreamTableR2, SessionStats), HardwareSmokeError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut hello = None;
    let mut channels = None;
    let mut streams = None;
    let mut stats = None;
    let mut ready = false;
    let mut ping_sent = false;
    loop {
        match recv_until(session, deadline, "R2 handshake")? {
            SessionEvent::HelloAck(value) => hello = Some(value),
            SessionEvent::ChannelTable(value) => channels = Some(value),
            SessionEvent::StreamTableR2(value) => streams = Some(value),
            SessionEvent::Stats(value) => stats = Some(value),
            SessionEvent::State(ConnectionState::Ready) => ready = true,
            SessionEvent::Error(error) | SessionEvent::RecordingError(error) => {
                return Err(HardwareSmokeError::Device(error));
            }
            SessionEvent::State(ConnectionState::Disconnected) => {
                return Err(HardwareSmokeError::Device(
                    "device disconnected during R2 handshake".to_owned(),
                ));
            }
            _ => {}
        }
        if ready && hello.is_some() && channels.is_some() && streams.is_some() && !ping_sent {
            session.ping(0x4833_304b_4954_5232)?;
            ping_sent = true;
        }
        if ping_sent
            && stats
                .as_ref()
                .is_some_and(|value| value.heartbeat_round_trip_count > 0)
        {
            return Ok((
                hello.expect("checked above"),
                channels.expect("checked above"),
                streams.expect("checked above"),
                stats.expect("checked above"),
            ));
        }
    }
}

fn wait_for_v2_r2_configured(session: &LiveSession) -> Result<(), HardwareSmokeError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match recv_until(session, deadline, "R2 atomic configuration")? {
            SessionEvent::ConfiguredV2R2(_) => return Ok(()),
            SessionEvent::CommandResult(result) if result.result_code != ResultCode::Ok => {
                return Err(HardwareSmokeError::Device(result.detail));
            }
            SessionEvent::Error(error) | SessionEvent::RecordingError(error) => {
                return Err(HardwareSmokeError::Device(error));
            }
            SessionEvent::State(ConnectionState::Disconnected) => {
                return Err(HardwareSmokeError::Device(
                    "device disconnected during R2 configuration".to_owned(),
                ));
            }
            _ => {}
        }
    }
}

fn collect_v2_r2(
    session: &LiveSession,
    duration: Duration,
    streams: &StreamTableR2,
    tick_hz: u64,
    initial_stats: SessionStats,
) -> Result<V2CaptureStats, HardwareSmokeError> {
    let started = Instant::now();
    let deadline = started + duration;
    let mut stats = initial_stats;
    let mut effective_payload_bytes = 0_u64;
    let mut seen = BTreeMap::<u16, u64>::new();
    let mut local_counts = BTreeMap::<u16, (u64, u64)>::new();
    while Instant::now() < deadline {
        match session.recv_timeout(EVENT_POLL_TIMEOUT) {
            Ok(SessionEvent::Stats(value)) => stats = value,
            Ok(SessionEvent::SnapshotV2R2(batch, diagnostics)) => {
                effective_payload_bytes = effective_payload_bytes
                    .saturating_add(batch.raw_frame.len().try_into().unwrap_or(u64::MAX));
                let descriptor = streams.stream(batch.stream_id).ok_or_else(|| {
                    HardwareSmokeError::Contract {
                        code: "unexpected_stream",
                        message: format!("received unknown stream {}", batch.stream_id),
                    }
                })?;
                let counts = local_counts.entry(batch.stream_id).or_default();
                counts.0 = counts.0.saturating_add(1);
                counts.1 = counts
                    .1
                    .saturating_add(batch.row_metadata.len().try_into().unwrap_or(u64::MAX));
                if batch.domain != descriptor.domain
                    || batch.consistency_group != descriptor.consistency_group
                    || batch.sample_period_ticks
                        != u32::try_from(tick_hz / u64::from(descriptor.sample_rate_hz))
                            .unwrap_or(u32::MAX)
                {
                    return contract(
                        "stream_timing_mismatch",
                        format!("stream {} timing contract mismatch", batch.stream_id),
                    );
                }
                if diagnostics.row_sequence_reorders > 0
                    || diagnostics.logical_cycle_faults > 0
                    || diagnostics.invalid_snapshot_rows > 0
                {
                    return contract(
                        "stream_sequence_mismatch",
                        format!(
                            "stream {} sequence diagnostics are non-zero",
                            batch.stream_id
                        ),
                    );
                }
                if let Some(previous) = seen.insert(
                    batch.stream_id,
                    batch
                        .first_row_sequence
                        .saturating_add(batch.row_metadata.len() as u64),
                ) {
                    if batch.first_row_sequence != previous {
                        return contract(
                            "stream_row_gap",
                            format!("stream {} row sequence is not continuous", batch.stream_id),
                        );
                    }
                }
                for (offset, row) in batch.row_metadata.iter().enumerate() {
                    let expected_row = batch.first_row_sequence.saturating_add(offset as u64);
                    let expected_cycle =
                        expected_row.saturating_mul(u64::from(descriptor.logical_cycle_step));
                    if row.row_sequence != expected_row
                        || row.logical_cycle_sequence != expected_cycle
                    {
                        return contract(
                            "stream_logical_cycle_mismatch",
                            format!("stream {} row metadata is not affine", batch.stream_id),
                        );
                    }
                }
            }
            Ok(SessionEvent::Error(error) | SessionEvent::RecordingError(error)) => {
                return Err(HardwareSmokeError::Device(error));
            }
            Ok(SessionEvent::State(ConnectionState::Disconnected)) => {
                return Err(HardwareSmokeError::Device(
                    "device disconnected during R2 streaming".to_owned(),
                ));
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(HardwareSmokeError::Device(
                    "session event channel disconnected during R2 streaming".to_owned(),
                ));
            }
        }
    }
    let local_batches = local_counts.values().map(|value| value.0).sum::<u64>();
    let local_rows = local_counts.values().map(|value| value.1).sum::<u64>();
    stats.received_batches = stats.received_batches.max(local_batches);
    stats.received_samples = stats.received_samples.max(local_rows);
    for (stream_id, (batches, rows)) in local_counts {
        if let Some(stream_stats) = stats
            .stream_stats
            .iter_mut()
            .find(|value| value.stream_id == stream_id || value.stream_id == 0)
        {
            stream_stats.stream_id = stream_id;
            stream_stats.received_batches = stream_stats.received_batches.max(batches);
            stream_stats.received_rows = stream_stats.received_rows.max(rows);
        }
    }
    Ok(V2CaptureStats {
        stats,
        effective_payload_bytes,
        measured_streaming_duration: started.elapsed(),
    })
}

fn ensure_clean_v2_r2(
    mode: HardwareSmokeMode,
    stats: &SessionStats,
    streams: &[HardwareSmokeStreamResult],
) -> Result<(), HardwareSmokeError> {
    let checks = [
        ("crc_error", stats.crc_errors),
        ("protocol_error", stats.protocol_errors),
        ("heartbeat_timeout", stats.heartbeat_timeout_count),
        ("row_reorder", stats.row_sequence_reorders),
        ("device_tx_overrun", stats.device_tx_overruns),
    ];
    for (code, value) in checks {
        if value != 0 {
            return contract(code, format!("{code} count is {value}"));
        }
    }
    if mode != HardwareSmokeMode::Handshake {
        let strict = [
            ("invalid_snapshot", stats.invalid_snapshot_rows),
            ("host_drop", stats.host_dropped_v2_rows),
            ("device_drop", stats.device_dropped_samples),
        ];
        for (code, value) in strict {
            if value != 0 {
                return contract(code, format!("{code} count is {value}"));
            }
        }
        if stats.received_batches == 0 {
            return contract("no_sample_batches", "device produced no R2 sample batches");
        }
        for stream in streams {
            if stream.received_batches == 0 || stream.received_rows == 0 {
                return contract(
                    "stream_no_data",
                    format!(
                        "stream_id={} domain={} received_batches={} received_rows={}",
                        stream.stream_id,
                        stream.domain,
                        stream.received_batches,
                        stream.received_rows
                    ),
                );
            }
            if mode == HardwareSmokeMode::LinkStress {
                let expected_min_rows = ((stream.expected_rows as f64
                    * stream.minimum_throughput_ratio)
                    .floor() as u64)
                    .saturating_sub(stream.selected_batch_allowance());
                if stream.received_rows < expected_min_rows {
                    return contract(
                        "stream_throughput_below_minimum",
                        format!(
                            "stream_id={} domain={} expected_rows={} received_rows={} throughput_ratio={:.6} minimum_throughput_ratio={:.6}",
                            stream.stream_id,
                            stream.domain,
                            stream.expected_rows,
                            stream.received_rows,
                            stream.throughput_ratio,
                            stream.minimum_throughput_ratio
                        ),
                    );
                }
            }
        }
    }
    Ok(())
}

fn stream_result(
    domain: SampleDomain,
    stats: StreamSessionStats,
    subscription: &StreamSubscriptionR2,
    sample_rate_hz: u32,
    measured_streaming_duration: Duration,
    invalid_rows: u64,
    device_dropped_rows: u64,
) -> HardwareSmokeStreamResult {
    let expected_rows =
        (f64::from(sample_rate_hz) * measured_streaming_duration.as_secs_f64()).floor() as u64;
    let throughput_ratio = if expected_rows == 0 {
        0.0
    } else {
        stats.received_rows as f64 / expected_rows as f64
    };
    HardwareSmokeStreamResult {
        stream_id: subscription.stream_id,
        domain: domain_name(domain).to_owned(),
        selected_channel_count: subscription.channel_mask.count_ones() as usize,
        selected_channel_mask: subscription.channel_mask,
        selected_batch_samples: subscription.batch_samples,
        received_batches: stats.received_batches,
        expected_rows,
        received_rows: stats.received_rows,
        throughput_ratio,
        minimum_throughput_ratio: MINIMUM_THROUGHPUT_RATIO,
        row_gaps: stats.row_sequence_gaps,
        row_reorders: stats.row_sequence_reorders,
        invalid_rows,
        host_dropped_rows: stats.host_dropped_rows,
        device_dropped_rows,
    }
}

impl HardwareSmokeStreamResult {
    fn selected_batch_allowance(&self) -> u64 {
        u64::from(self.selected_batch_samples)
    }
}

fn domain_name(domain: SampleDomain) -> &'static str {
    match domain {
        SampleDomain::Fast32k => "FAST32K",
        SampleDomain::Control8k => "CTRL8K",
        SampleDomain::Slow1k => "SLOW1K",
    }
}

fn contract<T>(code: &'static str, message: impl Into<String>) -> Result<T, HardwareSmokeError> {
    Err(HardwareSmokeError::Contract {
        code,
        message: message.into(),
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::live::simulator::{Hybrid30kR2FixtureFault, SimulatorConfig, SimulatorHandle};

    fn hybrid30k_budget(
        mode: HardwareSmokeMode,
        channel_set: ChannelSet,
        baud: u32,
    ) -> R2SerialLinkBudget {
        let simulator =
            SimulatorHandle::spawn_hybrid30k_r2("127.0.0.1:0".parse().unwrap()).unwrap();
        let session = LiveSession::connect_v2_r2(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        let (hello, channels, streams, _) = wait_for_v2_r2_handshake(&session).unwrap();
        let profile = MachineProfile::load_named("hybrid30k").unwrap();
        profile
            .validate_compatibility(hello.device_capabilities, &channels, &streams)
            .unwrap();
        let subscriptions = select_r2_subscriptions(
            mode,
            channel_set,
            16,
            hello.max_batch_samples,
            &profile,
            &channels,
            &streams,
        )
        .unwrap();
        let budget =
            estimate_r2_serial_budget(Some(baud), &subscriptions, &streams, &channels).unwrap();
        session.disconnect().unwrap();
        budget
    }

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
            duration: Duration::from_secs(1),
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

    #[test]
    fn hybrid30k_r2_tcp_software_smoke_covers_all_round_one_modes() {
        for mode in [
            HardwareSmokeMode::Handshake,
            HardwareSmokeMode::Ctrl8k,
            HardwareSmokeMode::Multistream,
            HardwareSmokeMode::LinkStress,
        ] {
            let simulator =
                SimulatorHandle::spawn_hybrid30k_r2("127.0.0.1:0".parse().unwrap()).unwrap();
            let result = run_v2_r2(&HardwareSmokeV2R2Config {
                transport: TransportConfig::Tcp {
                    address: simulator.address().to_string(),
                },
                profile: "hybrid30k".to_owned(),
                mode,
                channel_set: ChannelSet::Required,
                duration: if mode == HardwareSmokeMode::LinkStress {
                    Duration::from_secs(5)
                } else {
                    Duration::from_millis(400)
                },
                batch_samples: 4,
            })
            .unwrap_or_else(|error| {
                panic!(
                    "{mode:?} failed: {error}; simulator={:?}",
                    simulator.stats()
                )
            });
            assert!(result.ok);
            assert_eq!(result.crc_errors, 0);
            assert_eq!(result.protocol_errors, 0);
            assert_eq!(result.channel_set, "required");
            assert_eq!(result.link_budget.configured_baud, None);
            if mode != HardwareSmokeMode::Handshake {
                assert!(result.received_batches > 0);
                assert!(result.ready_after_stop);
            }
            if mode == HardwareSmokeMode::Ctrl8k {
                assert_eq!(result.streams.len(), 1);
                assert_eq!(result.streams[0].domain, "CTRL8K");
                assert_eq!(result.streams[0].selected_channel_count, 5);
            }
            if matches!(
                mode,
                HardwareSmokeMode::Multistream | HardwareSmokeMode::LinkStress
            ) {
                assert_eq!(result.streams.len(), 2);
                assert!(result.streams.iter().all(|stream| stream.received_rows > 0));
                assert_eq!(result.streams[0].selected_channel_count, 5);
                assert_eq!(result.streams[1].selected_channel_count, 1);
            }
        }
    }

    #[test]
    fn hybrid30k_link_budget_uses_full_r2_wire_formula() {
        for baud in [115_200, 921_600, 2_000_000, 4_000_000] {
            let budget = hybrid30k_budget(HardwareSmokeMode::Ctrl8k, ChannelSet::Required, baud);
            assert_eq!(
                budget.required_baud,
                (budget.wire_bytes_per_second * 10.0).ceil() as u64
            );
            assert!(
                (budget.estimated_utilization - budget.required_baud as f64 / f64::from(baud))
                    .abs()
                    < f64::EPSILON
            );
            assert_eq!(
                budget.estimated_utilization <= budget.safe_utilization_limit,
                budget.required_baud
                    <= (f64::from(baud) * budget.safe_utilization_limit).floor() as u64
            );
        }
    }

    #[test]
    fn hybrid30k_link_budget_covers_frozen_baud_and_channel_scenarios() {
        let ctrl_115k = hybrid30k_budget(HardwareSmokeMode::Ctrl8k, ChannelSet::Required, 115_200);
        assert!(ctrl_115k.estimated_utilization > ctrl_115k.safe_utilization_limit);
        let error = enforce_serial_link_budget(&ctrl_115k).unwrap_err();
        assert_eq!(error.code(), "link_budget_exceeded");
        let message = error.to_string();
        assert!(message.contains("required_baud="));
        assert!(message.contains("configured_baud=115200"));
        assert!(message.contains("estimated_utilization="));

        let ctrl_921k = hybrid30k_budget(HardwareSmokeMode::Ctrl8k, ChannelSet::Required, 921_600);
        assert_eq!(
            enforce_serial_link_budget(&ctrl_921k).is_ok(),
            ctrl_921k.estimated_utilization <= ctrl_921k.safe_utilization_limit
        );

        let ctrl_2m = hybrid30k_budget(HardwareSmokeMode::Ctrl8k, ChannelSet::Required, 2_000_000);
        assert_eq!(
            enforce_serial_link_budget(&ctrl_2m).is_ok(),
            ctrl_2m.estimated_utilization <= ctrl_2m.safe_utilization_limit
        );

        let ctrl_4m = hybrid30k_budget(HardwareSmokeMode::Ctrl8k, ChannelSet::Required, 4_000_000);
        assert!(ctrl_4m.estimated_utilization <= ctrl_4m.safe_utilization_limit);

        let multi_4m = hybrid30k_budget(
            HardwareSmokeMode::Multistream,
            ChannelSet::Required,
            4_000_000,
        );
        assert!(multi_4m.estimated_utilization <= multi_4m.safe_utilization_limit);

        let all_4m = hybrid30k_budget(HardwareSmokeMode::LinkStress, ChannelSet::All, 4_000_000);
        assert_eq!(
            all_4m.estimated_utilization <= all_4m.safe_utilization_limit,
            all_4m.required_baud <= (4_000_000.0 * all_4m.safe_utilization_limit).floor() as u64
        );

        let tcp_budget = R2SerialLinkBudget {
            configured_baud: None,
            ..ctrl_115k
        };
        enforce_serial_link_budget(&tcp_budget).unwrap();
    }

    #[test]
    fn all_channel_set_selects_all_present_profile_channels() {
        let simulator =
            SimulatorHandle::spawn_hybrid30k_r2("127.0.0.1:0".parse().unwrap()).unwrap();
        let result = run_v2_r2(&HardwareSmokeV2R2Config {
            transport: TransportConfig::Tcp {
                address: simulator.address().to_string(),
            },
            profile: "hybrid30k-r2".to_owned(),
            mode: HardwareSmokeMode::Multistream,
            channel_set: ChannelSet::All,
            duration: Duration::from_millis(400),
            batch_samples: 16,
        })
        .unwrap();
        assert_eq!(result.channel_set, "all");
        assert_eq!(result.streams[0].selected_channel_count, 12);
        assert_eq!(result.streams[1].selected_channel_count, 5);
    }

    #[test]
    fn short_link_stress_is_rejected_before_connecting() {
        let error = run_v2_r2(&HardwareSmokeV2R2Config {
            transport: TransportConfig::Tcp {
                address: "127.0.0.1:1".to_owned(),
            },
            profile: "hybrid30k".to_owned(),
            mode: HardwareSmokeMode::LinkStress,
            channel_set: ChannelSet::Required,
            duration: Duration::from_millis(4_999),
            batch_samples: 16,
        })
        .unwrap_err();
        assert_eq!(error.code(), "invalid_config");
    }

    #[test]
    fn multistream_fails_when_slow_stream_produces_no_data() {
        let simulator = SimulatorHandle::spawn_hybrid30k_r2_with_fault(
            "127.0.0.1:0".parse().unwrap(),
            Hybrid30kR2FixtureFault::SlowStreamNoData,
        )
        .unwrap();
        let error = run_v2_r2(&HardwareSmokeV2R2Config {
            transport: TransportConfig::Tcp {
                address: simulator.address().to_string(),
            },
            profile: "hybrid30k".to_owned(),
            mode: HardwareSmokeMode::Multistream,
            channel_set: ChannelSet::Required,
            duration: Duration::from_millis(400),
            batch_samples: 16,
        })
        .unwrap_err();
        assert_eq!(error.code(), "stream_no_data");
        assert!(error.to_string().contains("stream_id=3"));
        assert!(error.to_string().contains("domain=SLOW1K"));
    }

    #[test]
    fn link_stress_fails_when_slow_stream_runs_at_half_rate() {
        let simulator = SimulatorHandle::spawn_hybrid30k_r2_with_fault(
            "127.0.0.1:0".parse().unwrap(),
            Hybrid30kR2FixtureFault::SlowStreamHalfRate,
        )
        .unwrap();
        let error = run_v2_r2(&HardwareSmokeV2R2Config {
            transport: TransportConfig::Tcp {
                address: simulator.address().to_string(),
            },
            profile: "hybrid30k".to_owned(),
            mode: HardwareSmokeMode::LinkStress,
            channel_set: ChannelSet::Required,
            duration: Duration::from_secs(5),
            batch_samples: 16,
        })
        .unwrap_err();
        assert_eq!(error.code(), "stream_throughput_below_minimum");
        assert!(error.to_string().contains("stream_id=3"));
        assert!(error.to_string().contains("domain=SLOW1K"));
    }
}
