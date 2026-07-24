use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use thiserror::Error;

use super::protocol::{
    encode_configure_result_detail, validate_configure_for_device, ChannelDescriptor, ChannelKind,
    ChannelTable, CommandResult, Configure, DeviceState, Frame, FrameDecoder, HelloAck, Message,
    ResultCode, SampleBatch, Status, WireFormat, MAX_PAYLOAD_LEN, MSG_CONFIGURE, MSG_HELLO,
    MSG_PING, MSG_START, MSG_STOP,
};
use super::protocol_v2::{
    capture_integrity_summary, ArmCapture, CaptureBegin, CaptureData, CaptureEnd, CapturePhase,
    CaptureState, CaptureStatus, ConfigureStream, MessageV2, SampleDomain, StreamChannelBinding,
    StreamDescriptor, StreamSampleBatch, StreamTable, CAPABILITY_V2_STREAMS,
};
use super::snapshot::{
    SnapshotMeta, ADC_SAMPLE_VALID, APPLIED_SEQUENCE_VALID, CLA_RESULT_VALID, FROZEN_ROW,
    SNAPSHOT_VALID, SOURCE_SEQUENCE_VALID,
};

/// Frozen V1 simulator clock. Keep this separate so V2 hardening does not
/// alter the existing V1 simulator timing path.
const V1_SIMULATOR_TICK_HZ: u64 = 1_000_000;
/// All V2 fixed domains divide this clock exactly: 32 kHz -> 1000 ticks,
/// 8 kHz -> 4000 ticks, and 1 kHz -> 32000 ticks.
const V2_SIMULATOR_TICK_HZ: u64 = 32_000_000;
const SIMULATOR_SESSION_ID: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SimulatorProtocol {
    #[default]
    V1,
    V2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum V2Preset {
    Normal30k,
    CausalDelay30k,
    ClaStale30k,
    RowGap30k,
    RowReorder30k,
    PhaseMismatch30k,
    GroupMismatch30k,
    UnfrozenRow30k,
    CaptureManual30k,
    CaptureEdge30k,
    CaptureFault30k,
    CaptureTimeout30k,
    CaptureChunkLoss30k,
    CaptureChunkReorder30k,
    DeviceReset30k,
}

impl V2Preset {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "30k-normal" => Some(Self::Normal30k),
            "30k-causal-delay" => Some(Self::CausalDelay30k),
            "30k-cla-stale" => Some(Self::ClaStale30k),
            "30k-row-gap" => Some(Self::RowGap30k),
            "30k-row-reorder" => Some(Self::RowReorder30k),
            "30k-phase-mismatch" => Some(Self::PhaseMismatch30k),
            "30k-group-mismatch" => Some(Self::GroupMismatch30k),
            "30k-unfrozen-row" => Some(Self::UnfrozenRow30k),
            "30k-capture-manual" => Some(Self::CaptureManual30k),
            "30k-capture-edge" => Some(Self::CaptureEdge30k),
            "30k-capture-fault" => Some(Self::CaptureFault30k),
            "30k-capture-timeout" => Some(Self::CaptureTimeout30k),
            "30k-capture-chunk-loss" => Some(Self::CaptureChunkLoss30k),
            "30k-capture-chunk-reorder" => Some(Self::CaptureChunkReorder30k),
            "30k-device-reset" => Some(Self::DeviceReset30k),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Normal30k => "30k-normal",
            Self::CausalDelay30k => "30k-causal-delay",
            Self::ClaStale30k => "30k-cla-stale",
            Self::RowGap30k => "30k-row-gap",
            Self::RowReorder30k => "30k-row-reorder",
            Self::PhaseMismatch30k => "30k-phase-mismatch",
            Self::GroupMismatch30k => "30k-group-mismatch",
            Self::UnfrozenRow30k => "30k-unfrozen-row",
            Self::CaptureManual30k => "30k-capture-manual",
            Self::CaptureEdge30k => "30k-capture-edge",
            Self::CaptureFault30k => "30k-capture-fault",
            Self::CaptureTimeout30k => "30k-capture-timeout",
            Self::CaptureChunkLoss30k => "30k-capture-chunk-loss",
            Self::CaptureChunkReorder30k => "30k-capture-chunk-reorder",
            Self::DeviceReset30k => "30k-device-reset",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SimulatorConfig {
    pub listen: SocketAddr,
    pub sample_rate_hz: u32,
    pub batch_samples: u16,
    pub accelerated: bool,
    pub seed: u64,
    pub drop_every: Option<u64>,
    pub corrupt_every: Option<u64>,
    pub disconnect_after: Option<u64>,
    pub protocol: SimulatorProtocol,
    pub preset: Option<V2Preset>,
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:19090".parse().expect("valid default address"),
            sample_rate_hz: 10_000,
            batch_samples: 100,
            accelerated: false,
            seed: 1,
            drop_every: None,
            corrupt_every: None,
            disconnect_after: None,
            protocol: SimulatorProtocol::V1,
            preset: None,
        }
    }
}

impl SimulatorConfig {
    pub fn validate(&self) -> Result<(), SimulatorError> {
        let tick_hz = match self.protocol {
            SimulatorProtocol::V1 => V1_SIMULATOR_TICK_HZ,
            SimulatorProtocol::V2 => V2_SIMULATOR_TICK_HZ,
        };
        if self.sample_rate_hz == 0 || self.sample_rate_hz as u64 > tick_hz {
            return Err(SimulatorError::InvalidConfig(format!(
                "sample rate must be within 1..={tick_hz}"
            )));
        }
        if self.batch_samples == 0 || self.batch_samples > 4096 {
            return Err(SimulatorError::InvalidConfig(
                "batch samples must be within 1..=4096".to_owned(),
            ));
        }
        for (label, value) in [
            ("drop_every", self.drop_every),
            ("corrupt_every", self.corrupt_every),
            ("disconnect_after", self.disconnect_after),
        ] {
            if value == Some(0) {
                return Err(SimulatorError::InvalidConfig(format!(
                    "{label} must be greater than zero"
                )));
            }
        }
        if self.protocol == SimulatorProtocol::V1 && self.preset.is_some() {
            return Err(SimulatorError::InvalidConfig(
                "V2 preset requires --protocol v2".to_owned(),
            ));
        }
        Ok(())
    }
}

pub struct SimulatorHandle {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<SimulatorStats>>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SimulatorStats {
    pub connections: u64,
    pub hello_requests: u64,
    pub configure_requests: u64,
    pub start_requests: u64,
    pub stop_requests: u64,
    pub ping_requests: u64,
    pub pings_sent: u64,
    pub pongs_received: u64,
    pub emitted_batches: u64,
    pub v2_stream_table_requests: u64,
    pub capture_requests: u64,
}

impl SimulatorHandle {
    pub fn spawn(config: SimulatorConfig) -> Result<Self, SimulatorError> {
        config.validate()?;
        let listener = TcpListener::bind(config.listen)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let stats = Arc::new(Mutex::new(SimulatorStats::default()));
        let worker_stats = Arc::clone(&stats);
        let worker = thread::Builder::new()
            .name("scope-dsp-simulator".to_owned())
            .spawn(move || run_listener(listener, config, worker_stop, worker_stats))?;
        Ok(Self {
            address,
            stop,
            stats,
            worker: Some(worker),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn stats(&self) -> SimulatorStats {
        self.stats.lock().map(|stats| *stats).unwrap_or_default()
    }

    pub fn stop(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for SimulatorHandle {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

#[derive(Debug, Error)]
pub enum SimulatorError {
    #[error("invalid simulator configuration: {0}")]
    InvalidConfig(String),
    #[error("simulator I/O error: {0}")]
    Io(#[from] std::io::Error),
}

fn run_listener(
    listener: TcpListener,
    config: SimulatorConfig,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<SimulatorStats>>,
) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                update_stats(&stats, |stats| {
                    stats.connections = stats.connections.saturating_add(1)
                });
                let _ = match config.protocol {
                    SimulatorProtocol::V1 => serve_client(stream, &config, &stop, &stats),
                    SimulatorProtocol::V2 => serve_v2_client(stream, &config, &stop, &stats),
                };
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }
}

fn serve_client(
    mut stream: TcpStream,
    config: &SimulatorConfig,
    stop: &AtomicBool,
    stats: &Mutex<SimulatorStats>,
) -> Result<(), SimulatorError> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_millis(5)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    let mut decoder = FrameDecoder::default();
    let mut read_buffer = [0_u8; 16 * 1024];
    let mut out_sequence = 1_u32;
    let mut configured = Configure {
        sample_rate_hz: config.sample_rate_hz,
        batch_samples: config.batch_samples,
        channel_mask: 0b1111,
    };
    let mut state = DeviceState::Idle;
    let mut first_sample_index = 0_u64;
    let mut emitted_batches = 0_u64;
    let table = simulator_channel_table();
    let mut last_ping = Instant::now();
    let mut ping_nonce = 1_u64;

    while !stop.load(Ordering::Relaxed) {
        match stream.read(&mut read_buffer) {
            Ok(0) => break,
            Ok(count) => {
                decoder.push(&read_buffer[..count]);
                for frame in decoder.drain_frames() {
                    let message = match Message::decode(frame.message_type, &frame.payload) {
                        Ok(message) => message,
                        Err(_) => continue,
                    };
                    match message {
                        Message::Hello(_) if frame.message_type == MSG_HELLO => {
                            update_stats(stats, |stats| {
                                stats.hello_requests = stats.hello_requests.saturating_add(1)
                            });
                            let hello = simulator_hello_ack(&table);
                            send_message(
                                &mut stream,
                                &mut out_sequence,
                                Message::HelloAck(hello),
                                0,
                            )?;
                            send_message(
                                &mut stream,
                                &mut out_sequence,
                                Message::ChannelTable(table.clone()),
                                0,
                            )?;
                        }
                        Message::Configure(request) if frame.message_type == MSG_CONFIGURE => {
                            update_stats(stats, |stats| {
                                stats.configure_requests =
                                    stats.configure_requests.saturating_add(1)
                            });
                            match validate_configure_for_device(
                                &request,
                                &simulator_hello_ack(&table),
                                &table,
                            ) {
                                Ok(()) => {
                                    configured = request;
                                    state = DeviceState::Configured;
                                    send_result_with_detail(
                                        &mut stream,
                                        &mut out_sequence,
                                        frame.sequence,
                                        ResultCode::Ok,
                                        encode_configure_result_detail(&configured),
                                    )?;
                                }
                                Err(error) => send_result_with_detail(
                                    &mut stream,
                                    &mut out_sequence,
                                    frame.sequence,
                                    ResultCode::InvalidArgument,
                                    error.to_string(),
                                )?,
                            }
                        }
                        Message::Start if frame.message_type == MSG_START => {
                            update_stats(stats, |stats| {
                                stats.start_requests = stats.start_requests.saturating_add(1)
                            });
                            let result = if state == DeviceState::Configured {
                                state = DeviceState::Streaming;
                                ResultCode::Ok
                            } else {
                                ResultCode::InvalidState
                            };
                            send_result(&mut stream, &mut out_sequence, frame.sequence, result)?;
                        }
                        Message::Stop if frame.message_type == MSG_STOP => {
                            update_stats(stats, |stats| {
                                stats.stop_requests = stats.stop_requests.saturating_add(1)
                            });
                            let result = if state == DeviceState::Streaming {
                                state = DeviceState::Configured;
                                ResultCode::Ok
                            } else {
                                ResultCode::InvalidState
                            };
                            send_result(&mut stream, &mut out_sequence, frame.sequence, result)?;
                        }
                        Message::Ping(nonce) if frame.message_type == MSG_PING => {
                            update_stats(stats, |stats| {
                                stats.ping_requests = stats.ping_requests.saturating_add(1)
                            });
                            send_message(&mut stream, &mut out_sequence, Message::Pong(nonce), 0)?;
                        }
                        Message::Pong(_) => update_stats(stats, |stats| {
                            stats.pongs_received = stats.pongs_received.saturating_add(1)
                        }),
                        _ => {}
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }

        if last_ping.elapsed() >= Duration::from_secs(1) {
            send_message(&mut stream, &mut out_sequence, Message::Ping(ping_nonce), 0)?;
            ping_nonce = ping_nonce.wrapping_add(1);
            last_ping = Instant::now();
            update_stats(stats, |stats| {
                stats.pings_sent = stats.pings_sent.saturating_add(1)
            });
        }

        if state == DeviceState::Streaming {
            emitted_batches = emitted_batches.saturating_add(1);
            update_stats(stats, |stats| {
                stats.emitted_batches = stats.emitted_batches.saturating_add(1)
            });
            if config.disconnect_after == Some(emitted_batches) {
                break;
            }
            if !is_periodic_fault(config.drop_every, emitted_batches) {
                let frame = sample_frame(
                    out_sequence,
                    &configured,
                    first_sample_index,
                    &table,
                    config.seed,
                )?;
                out_sequence = out_sequence.wrapping_add(1);
                let mut bytes = frame
                    .encode()
                    .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?;
                if is_periodic_fault(config.corrupt_every, emitted_batches) && bytes.len() > 29 {
                    bytes[29] ^= 0x55;
                }
                stream.write_all(&bytes)?;
            } else {
                out_sequence = out_sequence.wrapping_add(1);
            }
            first_sample_index = first_sample_index
                .checked_add(u64::from(configured.batch_samples))
                .ok_or_else(|| SimulatorError::InvalidConfig("sample index overflow".to_owned()))?;
            if !config.accelerated {
                thread::sleep(Duration::from_secs_f64(
                    f64::from(configured.batch_samples) / f64::from(configured.sample_rate_hz),
                ));
            } else {
                thread::yield_now();
            }
        }
    }
    Ok(())
}

fn serve_v2_client(
    mut stream: TcpStream,
    config: &SimulatorConfig,
    stop: &AtomicBool,
    stats: &Mutex<SimulatorStats>,
) -> Result<(), SimulatorError> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_millis(5)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    let mut decoder = FrameDecoder::default();
    let mut read_buffer = [0_u8; 16 * 1024];
    let mut out_sequence = 1_u32;
    let mut configured: Option<ConfigureStream> = None;
    let mut streaming = false;
    let mut row_sequence = 1_u64;
    let mut emitted_batches = 0_u64;
    let mut armed_capture: Option<ArmCapture> = None;
    let mut session_established = false;
    let mut last_ping = Instant::now();
    let mut ping_nonce = 1_u64;
    let channels = v2_channel_table();
    let streams = v2_stream_table();
    let preset = config.preset.unwrap_or(V2Preset::Normal30k);

    while !stop.load(Ordering::Relaxed) {
        match stream.read(&mut read_buffer) {
            Ok(0) => break,
            Ok(count) => {
                decoder.push(&read_buffer[..count]);
                for frame in decoder.drain_frames() {
                    if frame.version != super::protocol::PROTOCOL_VERSION_V2 {
                        continue;
                    }
                    match frame.message_type {
                        MSG_HELLO => {
                            let Ok(Message::Hello(hello)) =
                                Message::decode(frame.message_type, &frame.payload)
                            else {
                                continue;
                            };
                            if hello.client_capabilities & CAPABILITY_V2_STREAMS == 0 {
                                continue;
                            }
                            update_stats(stats, |value| {
                                value.hello_requests = value.hello_requests.saturating_add(1);
                                value.v2_stream_table_requests =
                                    value.v2_stream_table_requests.saturating_add(1);
                            });
                            send_v2_common(
                                &mut stream,
                                &mut out_sequence,
                                Message::HelloAck(v2_hello_ack(&channels)),
                            )?;
                            send_v2_common(
                                &mut stream,
                                &mut out_sequence,
                                Message::ChannelTable(channels.clone()),
                            )?;
                            send_v2_message(
                                &mut stream,
                                &mut out_sequence,
                                MessageV2::StreamTable(streams.clone()),
                            )?;
                            send_v2_common(
                                &mut stream,
                                &mut out_sequence,
                                status_message(DeviceState::Idle, 0),
                            )?;
                            session_established = true;
                        }
                        super::protocol_v2::MSG_CONFIGURE_STREAM => {
                            let Ok(MessageV2::ConfigureStream(request)) =
                                MessageV2::decode(frame.message_type, &frame.payload)
                            else {
                                continue;
                            };
                            update_stats(stats, |value| {
                                value.configure_requests =
                                    value.configure_requests.saturating_add(1)
                            });
                            let result = super::protocol_v2::validate_configure_stream_for_device(
                                &request,
                                &streams,
                                &channels,
                                4096,
                                MAX_PAYLOAD_LEN as u32,
                            );
                            match result {
                                Ok(()) => {
                                    configured = Some(request);
                                    send_v2_common(
                                        &mut stream,
                                        &mut out_sequence,
                                        Message::CommandResult(CommandResult {
                                            request_sequence: frame.sequence,
                                            result_code: ResultCode::Ok,
                                            detail: "ok".to_owned(),
                                        }),
                                    )?;
                                    send_v2_common(
                                        &mut stream,
                                        &mut out_sequence,
                                        status_message(DeviceState::Configured, emitted_batches),
                                    )?;
                                }
                                Err(error) => send_v2_common(
                                    &mut stream,
                                    &mut out_sequence,
                                    Message::CommandResult(CommandResult {
                                        request_sequence: frame.sequence,
                                        result_code: ResultCode::InvalidArgument,
                                        detail: error.to_string(),
                                    }),
                                )?,
                            }
                        }
                        MSG_START => {
                            update_stats(stats, |value| {
                                value.start_requests = value.start_requests.saturating_add(1)
                            });
                            streaming = configured.is_some();
                            send_v2_common(
                                &mut stream,
                                &mut out_sequence,
                                Message::CommandResult(CommandResult {
                                    request_sequence: frame.sequence,
                                    result_code: if streaming {
                                        ResultCode::Ok
                                    } else {
                                        ResultCode::InvalidState
                                    },
                                    detail: if streaming { "ok" } else { "not configured" }
                                        .to_owned(),
                                }),
                            )?;
                            if streaming {
                                send_v2_common(
                                    &mut stream,
                                    &mut out_sequence,
                                    status_message(DeviceState::Streaming, emitted_batches),
                                )?;
                            }
                        }
                        MSG_STOP => {
                            update_stats(stats, |value| {
                                value.stop_requests = value.stop_requests.saturating_add(1)
                            });
                            streaming = false;
                            send_v2_common(
                                &mut stream,
                                &mut out_sequence,
                                Message::CommandResult(CommandResult {
                                    request_sequence: frame.sequence,
                                    result_code: ResultCode::Ok,
                                    detail: "ok".to_owned(),
                                }),
                            )?;
                            send_v2_common(
                                &mut stream,
                                &mut out_sequence,
                                status_message(DeviceState::Configured, emitted_batches),
                            )?;
                        }
                        MSG_PING => {
                            if let Ok(Message::Ping(nonce)) =
                                Message::decode(frame.message_type, &frame.payload)
                            {
                                send_v2_common(
                                    &mut stream,
                                    &mut out_sequence,
                                    Message::Pong(nonce),
                                )?;
                            }
                        }
                        super::protocol::MSG_PONG => {
                            if Message::decode(frame.message_type, &frame.payload).is_ok() {
                                update_stats(stats, |value| {
                                    value.pongs_received = value.pongs_received.saturating_add(1)
                                });
                            }
                        }
                        super::protocol_v2::MSG_ARM_CAPTURE => {
                            let Ok(MessageV2::ArmCapture(capture)) =
                                MessageV2::decode(frame.message_type, &frame.payload)
                            else {
                                continue;
                            };
                            update_stats(stats, |value| {
                                value.capture_requests = value.capture_requests.saturating_add(1)
                            });
                            armed_capture = Some(capture.clone());
                            send_v2_message(
                                &mut stream,
                                &mut out_sequence,
                                MessageV2::CaptureStatus(CaptureStatus {
                                    capture_id: capture.capture_id,
                                    state: CaptureState::Armed,
                                    captured_rows: 0,
                                    dropped_rows: 0,
                                }),
                            )?;
                            if matches!(
                                preset,
                                V2Preset::CaptureEdge30k
                                    | V2Preset::CaptureFault30k
                                    | V2Preset::CaptureTimeout30k
                                    | V2Preset::DeviceReset30k
                            ) {
                                upload_simulated_capture(
                                    &mut stream,
                                    &mut out_sequence,
                                    &streams,
                                    &channels,
                                    &capture,
                                    preset,
                                    row_sequence,
                                )?;
                                armed_capture = None;
                            }
                        }
                        super::protocol_v2::MSG_MANUAL_TRIGGER => {
                            let Ok(MessageV2::ManualTrigger(trigger)) =
                                MessageV2::decode(frame.message_type, &frame.payload)
                            else {
                                continue;
                            };
                            if let Some(capture) = armed_capture
                                .take()
                                .filter(|capture| capture.capture_id == trigger.capture_id)
                            {
                                upload_simulated_capture(
                                    &mut stream,
                                    &mut out_sequence,
                                    &streams,
                                    &channels,
                                    &capture,
                                    preset,
                                    row_sequence,
                                )?;
                            }
                        }
                        super::protocol_v2::MSG_CANCEL_CAPTURE => {
                            let Ok(MessageV2::CancelCapture(cancel)) =
                                MessageV2::decode(frame.message_type, &frame.payload)
                            else {
                                continue;
                            };
                            if armed_capture
                                .as_ref()
                                .is_some_and(|capture| capture.capture_id == cancel.capture_id)
                            {
                                armed_capture = None;
                                send_v2_message(
                                    &mut stream,
                                    &mut out_sequence,
                                    MessageV2::CaptureStatus(CaptureStatus {
                                        capture_id: cancel.capture_id,
                                        state: CaptureState::Cancelled,
                                        captured_rows: 0,
                                        dropped_rows: 0,
                                    }),
                                )?;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }

        if streaming {
            let configure = configured
                .as_ref()
                .expect("streaming requires configuration");
            emitted_batches = emitted_batches.saturating_add(1);
            update_stats(stats, |value| {
                value.emitted_batches = value.emitted_batches.saturating_add(1)
            });
            let mut batch = v2_sample_batch(&streams, &channels, configure, row_sequence, preset)?;
            if preset == V2Preset::RowReorder30k && emitted_batches >= 2 {
                for row in &mut batch.row_metadata {
                    row.row_sequence = row
                        .row_sequence
                        .saturating_sub(u64::from(configure.batch_samples));
                }
                batch.first_row_sequence = batch.row_metadata[0].row_sequence;
            }
            let timestamp_ticks = row_sequence
                .checked_mul(u64::from(batch.sample_period_ticks))
                .ok_or_else(|| SimulatorError::InvalidConfig("V2 timestamp overflow".to_owned()))?;
            let mut frame = MessageV2::StreamSampleBatch(batch)
                .into_frame(0, out_sequence, SIMULATOR_SESSION_ID, timestamp_ticks)
                .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?;
            if preset == V2Preset::PhaseMismatch30k {
                frame.payload[7] = CapturePhase::ControlCycleEnd as u8;
            }
            if preset == V2Preset::GroupMismatch30k {
                frame.payload[8..10].copy_from_slice(&2_u16.to_le_bytes());
            }
            out_sequence = out_sequence.wrapping_add(1);
            stream.write_all(
                &frame
                    .encode()
                    .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?,
            )?;
            row_sequence = row_sequence
                .checked_add(u64::from(configure.batch_samples))
                .ok_or_else(|| {
                    SimulatorError::InvalidConfig("V2 row sequence overflow".to_owned())
                })?;
            if preset == V2Preset::RowGap30k && emitted_batches == 1 {
                row_sequence = row_sequence.saturating_add(1);
            }
            if !config.accelerated {
                thread::sleep(Duration::from_secs_f64(
                    f64::from(configure.batch_samples) / 32_000.0,
                ));
            } else {
                thread::yield_now();
            }
        }
        if session_established && last_ping.elapsed() >= Duration::from_secs(1) {
            send_v2_common(&mut stream, &mut out_sequence, Message::Ping(ping_nonce))?;
            update_stats(stats, |value| {
                value.pings_sent = value.pings_sent.saturating_add(1)
            });
            ping_nonce = ping_nonce.wrapping_add(1);
            last_ping = Instant::now();
        }
    }
    Ok(())
}

fn simulator_channel_table() -> ChannelTable {
    ChannelTable {
        revision: 1,
        channels: vec![
            descriptor(0, "Sine A", "V", WireFormat::I16, 0.001),
            descriptor(1, "Sine B", "V", WireFormat::I16, 0.001),
            descriptor(2, "Saw", "V", WireFormat::I16, 0.001),
            ChannelDescriptor {
                channel_id: 3,
                kind: ChannelKind::Digital,
                wire_format: WireFormat::U8,
                scale: 1.0,
                offset: 0.0,
                unit: String::new(),
                name: "Digital".to_owned(),
            },
        ],
    }
}

fn v2_channel_table() -> ChannelTable {
    ChannelTable {
        revision: 2,
        channels: vec![
            descriptor(0, "ADC current", "A", WireFormat::I16, 0.001),
            descriptor(1, "CLA input", "A", WireFormat::I16, 0.001),
            descriptor(2, "CLA result", "A", WireFormat::I16, 0.001),
            descriptor(3, "CLA completed sequence", "count", WireFormat::I32, 1.0),
            descriptor(4, "Current reference", "A", WireFormat::I16, 0.001),
            descriptor(5, "Control output", "V", WireFormat::I16, 0.001),
            descriptor(6, "Command sequence", "count", WireFormat::I32, 1.0),
            descriptor(7, "Applied sequence", "count", WireFormat::I32, 1.0),
            descriptor(8, "Run state", "", WireFormat::U8, 1.0),
            descriptor(9, "Fault flags", "bits", WireFormat::I32, 1.0),
            descriptor(10, "Logic tick", "count", WireFormat::I32, 1.0),
        ],
    }
}

fn v2_stream_table() -> StreamTable {
    StreamTable {
        revision: 2,
        streams: vec![
            StreamDescriptor {
                stream_id: 1,
                domain: SampleDomain::Fast32k,
                capture_phase: CapturePhase::AfterClaComplete,
                sample_rate_hz: 32_000,
                consistency_group: 1,
                channel_ids: vec![0, 1, 2, 3],
            },
            StreamDescriptor {
                stream_id: 2,
                domain: SampleDomain::Control8k,
                capture_phase: CapturePhase::ControlCycleEnd,
                sample_rate_hz: 8_000,
                consistency_group: 1,
                channel_ids: vec![4, 5, 6, 7],
            },
            StreamDescriptor {
                stream_id: 3,
                domain: SampleDomain::Slow1k,
                capture_phase: CapturePhase::LogicTaskEnd,
                sample_rate_hz: 1_000,
                consistency_group: 1,
                channel_ids: vec![8, 9, 10],
            },
        ],
        bindings: vec![
            StreamChannelBinding {
                channel_id: 0,
                stream_id: 1,
                owner: super::protocol_v2::SignalOwner::Cpu1,
                role: super::protocol_v2::SignalRole::PhysicalSample,
            },
            StreamChannelBinding {
                channel_id: 1,
                stream_id: 1,
                owner: super::protocol_v2::SignalOwner::Cpu1Cla1,
                role: super::protocol_v2::SignalRole::ControlInput,
            },
            StreamChannelBinding {
                channel_id: 2,
                stream_id: 1,
                owner: super::protocol_v2::SignalOwner::Cpu1Cla1,
                role: super::protocol_v2::SignalRole::ControlOutput,
            },
            StreamChannelBinding {
                channel_id: 3,
                stream_id: 1,
                owner: super::protocol_v2::SignalOwner::Cpu1Cla1,
                role: super::protocol_v2::SignalRole::Metadata,
            },
            StreamChannelBinding {
                channel_id: 4,
                stream_id: 2,
                owner: super::protocol_v2::SignalOwner::Cpu1,
                role: super::protocol_v2::SignalRole::ControlInput,
            },
            StreamChannelBinding {
                channel_id: 5,
                stream_id: 2,
                owner: super::protocol_v2::SignalOwner::Cpu1,
                role: super::protocol_v2::SignalRole::ControlOutput,
            },
            StreamChannelBinding {
                channel_id: 6,
                stream_id: 2,
                owner: super::protocol_v2::SignalOwner::Cpu1,
                role: super::protocol_v2::SignalRole::Command,
            },
            StreamChannelBinding {
                channel_id: 7,
                stream_id: 2,
                owner: super::protocol_v2::SignalOwner::Cpu1,
                role: super::protocol_v2::SignalRole::AppliedCommand,
            },
            StreamChannelBinding {
                channel_id: 8,
                stream_id: 3,
                owner: super::protocol_v2::SignalOwner::Cpu2,
                role: super::protocol_v2::SignalRole::State,
            },
            StreamChannelBinding {
                channel_id: 9,
                stream_id: 3,
                owner: super::protocol_v2::SignalOwner::Cpu2,
                role: super::protocol_v2::SignalRole::Fault,
            },
            StreamChannelBinding {
                channel_id: 10,
                stream_id: 3,
                owner: super::protocol_v2::SignalOwner::Cpu2,
                role: super::protocol_v2::SignalRole::Metadata,
            },
        ],
        causal_relations: Vec::new(),
    }
}

fn v2_hello_ack(table: &ChannelTable) -> HelloAck {
    HelloAck {
        device_capabilities: CAPABILITY_V2_STREAMS,
        max_payload: MAX_PAYLOAD_LEN as u32,
        tick_hz: V2_SIMULATOR_TICK_HZ,
        channel_count: table.channels.len() as u16,
        max_batch_samples: 4096,
        device_id: *b"SCOPE-SIM-V2----",
        firmware_name: "scope-dsp-simulator-v2".to_owned(),
    }
}

fn v2_sample_batch(
    streams: &StreamTable,
    channels: &ChannelTable,
    configure: &ConfigureStream,
    first_row_sequence: u64,
    preset: V2Preset,
) -> Result<StreamSampleBatch, SimulatorError> {
    let descriptor = streams
        .stream(configure.stream_id)
        .ok_or_else(|| SimulatorError::InvalidConfig("unknown V2 stream".to_owned()))?;
    let channel_ids = descriptor
        .channel_ids
        .iter()
        .copied()
        .filter(|channel_id| configure.channel_mask & (1_u64 << channel_id) != 0)
        .collect::<Vec<_>>();
    let mut sample_data = Vec::new();
    let mut row_metadata = Vec::with_capacity(usize::from(configure.batch_samples));
    for offset in 0..configure.batch_samples {
        let row = first_row_sequence.saturating_add(u64::from(offset));
        for channel_id in &channel_ids {
            let channel = channels.channel(*channel_id).ok_or_else(|| {
                SimulatorError::InvalidConfig("missing V2 channel descriptor".to_owned())
            })?;
            let value =
                i32::try_from(row.saturating_add(u64::from(*channel_id))).unwrap_or(i32::MAX);
            match channel.wire_format {
                WireFormat::I16 => sample_data.extend_from_slice(&(value as i16).to_le_bytes()),
                WireFormat::I32 => sample_data.extend_from_slice(&value.to_le_bytes()),
                WireFormat::F32 => sample_data.extend_from_slice(&(value as f32).to_le_bytes()),
                WireFormat::U8 => sample_data.push((value & 1) as u8),
            }
        }
        let mut valid_flags = SNAPSHOT_VALID
            | SOURCE_SEQUENCE_VALID
            | APPLIED_SEQUENCE_VALID
            | ADC_SAMPLE_VALID
            | FROZEN_ROW;
        if descriptor.domain == SampleDomain::Fast32k {
            valid_flags |= CLA_RESULT_VALID;
        }
        if preset == V2Preset::ClaStale30k && descriptor.domain == SampleDomain::Fast32k {
            valid_flags &= !CLA_RESULT_VALID;
        }
        if preset == V2Preset::UnfrozenRow30k {
            valid_flags &= !FROZEN_ROW;
        }
        row_metadata.push(SnapshotMeta {
            row_sequence: row,
            source_sequence: row,
            applied_sequence: row.saturating_sub(1),
            valid_flags,
        });
    }
    Ok(StreamSampleBatch {
        stream_id: descriptor.stream_id,
        stream_revision: streams.revision,
        domain: descriptor.domain,
        capture_phase: descriptor.capture_phase,
        consistency_group: descriptor.consistency_group,
        first_row_sequence,
        sample_period_ticks: u32::try_from(
            V2_SIMULATOR_TICK_HZ / u64::from(descriptor.sample_rate_hz),
        )
        .map_err(|_| {
            SimulatorError::InvalidConfig("V2 sample period does not fit u32".to_owned())
        })?,
        row_count: configure.batch_samples,
        channel_ids,
        sample_data,
        row_metadata,
    })
}

fn upload_simulated_capture(
    stream: &mut TcpStream,
    out_sequence: &mut u32,
    streams: &StreamTable,
    channels: &ChannelTable,
    capture: &ArmCapture,
    preset: V2Preset,
    first_row_sequence: u64,
) -> Result<(), SimulatorError> {
    if preset == V2Preset::DeviceReset30k {
        return send_v2_message(
            stream,
            out_sequence,
            MessageV2::CaptureStatus(CaptureStatus {
                capture_id: capture.capture_id,
                state: CaptureState::DeviceReset,
                captured_rows: 0,
                dropped_rows: 0,
            }),
        );
    }
    if preset == V2Preset::CaptureTimeout30k {
        return send_v2_message(
            stream,
            out_sequence,
            MessageV2::CaptureStatus(CaptureStatus {
                capture_id: capture.capture_id,
                state: CaptureState::Timeout,
                captured_rows: 0,
                dropped_rows: 0,
            }),
        );
    }
    let source = streams
        .stream(capture.stream_id)
        .ok_or_else(|| SimulatorError::InvalidConfig("capture stream is unknown".to_owned()))?;
    let mask = source
        .channel_ids
        .iter()
        .fold(0_u64, |mask, id| mask | (1_u64 << id));
    let configure = ConfigureStream {
        stream_id: capture.stream_id,
        batch_samples: 1,
        channel_mask: mask,
    };
    let first = v2_sample_batch(
        streams,
        channels,
        &configure,
        first_row_sequence,
        V2Preset::Normal30k,
    )?;
    let second = v2_sample_batch(
        streams,
        channels,
        &configure,
        first_row_sequence.saturating_add(1),
        V2Preset::Normal30k,
    )?;
    send_v2_message(
        stream,
        out_sequence,
        MessageV2::CaptureStatus(CaptureStatus {
            capture_id: capture.capture_id,
            state: CaptureState::Uploading,
            captured_rows: 2,
            dropped_rows: 0,
        }),
    )?;
    send_v2_message(
        stream,
        out_sequence,
        MessageV2::CaptureBegin(CaptureBegin {
            capture_id: capture.capture_id,
            stream_id: capture.stream_id,
            row_count: 2,
            trigger_row_seq: first_row_sequence,
        }),
    )?;
    let first_data = MessageV2::CaptureData(CaptureData {
        capture_id: capture.capture_id,
        block_index: 0,
        batch: first,
    });
    let second_data = MessageV2::CaptureData(CaptureData {
        capture_id: capture.capture_id,
        block_index: 1,
        batch: second,
    });
    let integrity_blocks = match (&first_data, &second_data) {
        (MessageV2::CaptureData(first), MessageV2::CaptureData(second)) => {
            vec![first.clone(), second.clone()]
        }
        _ => unreachable!("capture data variants are constructed above"),
    };
    let integrity_summary = capture_integrity_summary(capture.capture_id, &integrity_blocks)
        .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?;
    match preset {
        V2Preset::CaptureChunkLoss30k => send_v2_message(stream, out_sequence, second_data)?,
        V2Preset::CaptureChunkReorder30k => {
            send_v2_message(stream, out_sequence, second_data)?;
            send_v2_message(stream, out_sequence, first_data)?;
        }
        _ => {
            send_v2_message(stream, out_sequence, first_data)?;
            send_v2_message(stream, out_sequence, second_data)?;
        }
    }
    send_v2_message(
        stream,
        out_sequence,
        MessageV2::CaptureEnd(CaptureEnd {
            capture_id: capture.capture_id,
            state: CaptureState::Complete,
            uploaded_rows: 2,
            dropped_rows: 0,
            total_blocks: 2,
            total_samples: 2,
            integrity_summary,
        }),
    )
}

fn simulator_hello_ack(table: &ChannelTable) -> HelloAck {
    HelloAck {
        device_capabilities: 0,
        max_payload: MAX_PAYLOAD_LEN as u32,
        tick_hz: V1_SIMULATOR_TICK_HZ,
        channel_count: table.channels.len() as u16,
        max_batch_samples: 4096,
        device_id: *b"SCOPE-SIM-V1----",
        firmware_name: "scope-dsp-simulator".to_owned(),
    }
}

fn descriptor(
    channel_id: u16,
    name: &str,
    unit: &str,
    wire_format: WireFormat,
    scale: f32,
) -> ChannelDescriptor {
    ChannelDescriptor {
        channel_id,
        kind: ChannelKind::Analog,
        wire_format,
        scale,
        offset: 0.0,
        unit: unit.to_owned(),
        name: name.to_owned(),
    }
}

fn sample_frame(
    sequence: u32,
    configure: &Configure,
    first_sample_index: u64,
    table: &ChannelTable,
    seed: u64,
) -> Result<Frame, SimulatorError> {
    let channel_ids = table
        .channels
        .iter()
        .filter(|channel| configure.channel_mask & (1_u64 << channel.channel_id) != 0)
        .map(|channel| channel.channel_id)
        .collect::<Vec<_>>();
    if channel_ids.is_empty() {
        return Err(SimulatorError::InvalidConfig(
            "configured channel mask is empty".to_owned(),
        ));
    }
    let sample_period_ticks = u32::try_from(
        V1_SIMULATOR_TICK_HZ / u64::from(configure.sample_rate_hz),
    )
    .map_err(|_| SimulatorError::InvalidConfig("sample period does not fit u32".to_owned()))?;
    if sample_period_ticks == 0 {
        return Err(SimulatorError::InvalidConfig(
            "sample period rounded to zero".to_owned(),
        ));
    }
    let mut sample_data = Vec::new();
    for offset in 0..configure.batch_samples {
        let index = first_sample_index + u64::from(offset);
        let seeded_index = index.wrapping_add(seed);
        let phase = std::f64::consts::TAU * 50.0 * seeded_index as f64
            / f64::from(configure.sample_rate_hz);
        for channel_id in &channel_ids {
            match channel_id {
                0 => {
                    sample_data.extend_from_slice(&((phase.sin() * 10_000.0) as i16).to_le_bytes())
                }
                1 => sample_data.extend_from_slice(
                    &(((phase - std::f64::consts::TAU / 3.0).sin() * 10_000.0) as i16)
                        .to_le_bytes(),
                ),
                2 => {
                    let saw = ((index % 200) as i16 - 100) * 100;
                    sample_data.extend_from_slice(&saw.to_le_bytes());
                }
                3 => sample_data.push(u8::from((index / 50).is_multiple_of(2))),
                _ => {
                    return Err(SimulatorError::InvalidConfig(format!(
                        "unknown simulator channel {channel_id}"
                    )))
                }
            }
        }
    }
    let timestamp_ticks = first_sample_index
        .checked_mul(u64::from(sample_period_ticks))
        .ok_or_else(|| SimulatorError::InvalidConfig("timestamp overflow".to_owned()))?;
    let message = Message::SampleBatch(SampleBatch {
        channel_table_revision: table.revision,
        first_sample_index,
        sample_period_ticks,
        sample_count: configure.batch_samples,
        channel_ids,
        sample_data,
    });
    let payload = message
        .encode_payload()
        .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?;
    Ok(Frame::new(
        message.message_type(),
        0,
        sequence,
        SIMULATOR_SESSION_ID,
        timestamp_ticks,
        payload,
    ))
}

fn send_result(
    stream: &mut TcpStream,
    sequence: &mut u32,
    request_sequence: u32,
    result_code: ResultCode,
) -> Result<(), SimulatorError> {
    send_result_with_detail(
        stream,
        sequence,
        request_sequence,
        result_code,
        if result_code == ResultCode::Ok {
            "ok".to_owned()
        } else {
            "invalid state".to_owned()
        },
    )
}

fn send_result_with_detail(
    stream: &mut TcpStream,
    sequence: &mut u32,
    request_sequence: u32,
    result_code: ResultCode,
    detail: String,
) -> Result<(), SimulatorError> {
    send_message(
        stream,
        sequence,
        Message::CommandResult(CommandResult {
            request_sequence,
            result_code,
            detail,
        }),
        0,
    )
}

fn update_stats(stats: &Mutex<SimulatorStats>, update: impl FnOnce(&mut SimulatorStats)) {
    if let Ok(mut stats) = stats.lock() {
        update(&mut stats);
    }
}

fn send_v2_common(
    stream: &mut TcpStream,
    sequence: &mut u32,
    message: Message,
) -> Result<(), SimulatorError> {
    let frame = Frame::new_v2(
        message.message_type(),
        0,
        *sequence,
        SIMULATOR_SESSION_ID,
        0,
        message
            .encode_payload()
            .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?,
    );
    *sequence = sequence.wrapping_add(1);
    stream.write_all(
        &frame
            .encode()
            .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?,
    )?;
    Ok(())
}

fn send_v2_message(
    stream: &mut TcpStream,
    sequence: &mut u32,
    message: MessageV2,
) -> Result<(), SimulatorError> {
    let frame = message
        .into_frame(0, *sequence, SIMULATOR_SESSION_ID, 0)
        .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?;
    *sequence = sequence.wrapping_add(1);
    stream.write_all(
        &frame
            .encode()
            .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?,
    )?;
    Ok(())
}

fn send_message(
    stream: &mut TcpStream,
    sequence: &mut u32,
    message: Message,
    timestamp_ticks: u64,
) -> Result<(), SimulatorError> {
    let frame = Frame::new(
        message.message_type(),
        0,
        *sequence,
        SIMULATOR_SESSION_ID,
        timestamp_ticks,
        message
            .encode_payload()
            .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?,
    );
    *sequence = sequence.wrapping_add(1);
    stream.write_all(
        &frame
            .encode()
            .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?,
    )?;
    Ok(())
}

fn is_periodic_fault(period: Option<u64>, count: u64) -> bool {
    period.is_some_and(|period| count.is_multiple_of(period))
}

#[allow(dead_code)]
fn status_message(state: DeviceState, produced_samples: u64) -> Message {
    Message::Status(Status {
        state,
        produced_samples,
        dropped_samples: 0,
        tx_overruns: 0,
    })
}

#[cfg(test)]
mod v2_tests {
    use super::*;

    const PRESETS: [(&str, V2Preset); 15] = [
        ("30k-normal", V2Preset::Normal30k),
        ("30k-causal-delay", V2Preset::CausalDelay30k),
        ("30k-cla-stale", V2Preset::ClaStale30k),
        ("30k-row-gap", V2Preset::RowGap30k),
        ("30k-row-reorder", V2Preset::RowReorder30k),
        ("30k-phase-mismatch", V2Preset::PhaseMismatch30k),
        ("30k-group-mismatch", V2Preset::GroupMismatch30k),
        ("30k-unfrozen-row", V2Preset::UnfrozenRow30k),
        ("30k-capture-manual", V2Preset::CaptureManual30k),
        ("30k-capture-edge", V2Preset::CaptureEdge30k),
        ("30k-capture-fault", V2Preset::CaptureFault30k),
        ("30k-capture-timeout", V2Preset::CaptureTimeout30k),
        ("30k-capture-chunk-loss", V2Preset::CaptureChunkLoss30k),
        (
            "30k-capture-chunk-reorder",
            V2Preset::CaptureChunkReorder30k,
        ),
        ("30k-device-reset", V2Preset::DeviceReset30k),
    ];

    #[test]
    fn every_30k_preset_has_a_stable_name_and_deterministic_rows() {
        let streams = v2_stream_table();
        let channels = v2_channel_table();
        let configure = ConfigureStream {
            stream_id: 1,
            batch_samples: 2,
            channel_mask: 0b1111,
        };
        for (name, preset) in PRESETS {
            assert_eq!(V2Preset::parse(name), Some(preset));
            assert_eq!(preset.name(), name);
            assert_eq!(
                v2_sample_batch(&streams, &channels, &configure, 100, preset).unwrap(),
                v2_sample_batch(&streams, &channels, &configure, 100, preset).unwrap(),
                "preset {name} must not depend on random probability"
            );
        }
    }

    #[test]
    fn stale_and_unfrozen_presets_encode_expected_diagnostic_conditions() {
        let streams = v2_stream_table();
        let channels = v2_channel_table();
        let configure = ConfigureStream {
            stream_id: 1,
            batch_samples: 1,
            channel_mask: 0b1111,
        };
        let stale =
            v2_sample_batch(&streams, &channels, &configure, 1, V2Preset::ClaStale30k).unwrap();
        assert_eq!(stale.row_metadata[0].valid_flags & CLA_RESULT_VALID, 0);
        let unfrozen =
            v2_sample_batch(&streams, &channels, &configure, 1, V2Preset::UnfrozenRow30k).unwrap();
        assert_eq!(unfrozen.row_metadata[0].valid_flags & FROZEN_ROW, 0);
    }
}
