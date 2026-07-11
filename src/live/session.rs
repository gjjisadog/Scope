use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::Mutex,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crossbeam_channel::{after, bounded, select, Receiver, RecvTimeoutError, Sender, TrySendError};
use thiserror::Error;

use super::{
    buffer::{GapReason, LiveGap},
    protocol::{
        decode_configure_result_detail, decode_sample_frame, validate_configure_for_device,
        ChannelTable, CommandResult, Configure, DecodedSampleBatch, Frame, FrameDecoder, Hello,
        HelloAck, Message, ProtocolError, ResultCode, MAX_PAYLOAD_LEN,
    },
    recording::RecordingIngress,
    transport::{TransportConfig, TransportError, TransportStream},
};

const COMMAND_CAPACITY: usize = 32;
const CONTROL_EVENT_CAPACITY: usize = 64;
const EVENT_CAPACITY: usize = 256;
const CONTROL_SEND_TIMEOUT: Duration = Duration::from_millis(100);
const COMMAND_SEND_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Handshaking,
    Configuring,
    Ready,
    Streaming,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionStats {
    pub received_frames: u64,
    pub received_batches: u64,
    pub received_samples: u64,
    pub protocol_errors: u64,
    pub sequence_gaps: u64,
    pub host_dropped_batches: u64,
    pub crc_errors: u64,
    pub malformed_headers: u64,
    pub discarded_bytes: u64,
    pub unknown_messages: u64,
    pub device_dropped_samples: u64,
    pub device_tx_overruns: u64,
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    State(ConnectionState),
    HelloAck(HelloAck),
    ChannelTable(ChannelTable),
    Configured(Configure),
    CommandResult(CommandResult),
    Batch(DecodedSampleBatch),
    Gap(LiveGap),
    Stats(SessionStats),
    RecordingError(String),
    Error(String),
}

enum SessionCommand {
    Configure(Configure),
    Start,
    Stop,
    Ping(u64),
    SetRecording(Option<RecordingIngress>, Sender<()>),
    Disconnect,
}

#[derive(Clone)]
enum PendingCommand {
    Configure(Configure),
    Start,
    Stop,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("live session command channel is closed")]
    CommandChannelClosed,
    #[error("live session command queue remained full")]
    CommandQueueFull,
    #[error("live session worker panicked")]
    WorkerPanicked,
    #[error("live transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("live protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("live session I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("live session control event queue remained full")]
    ControlEventBackpressure,
    #[error("live session recording attachment timed out")]
    RecordingAttachmentTimeout,
}

pub struct LiveSession {
    command_tx: Sender<SessionCommand>,
    control_rx: Receiver<SessionEvent>,
    data_rx: Receiver<SessionEvent>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl LiveSession {
    pub fn connect(config: TransportConfig) -> Result<Self, SessionError> {
        config.validate()?;
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        let (control_tx, control_rx) = bounded(CONTROL_EVENT_CAPACITY);
        let (data_tx, data_rx) = bounded(EVENT_CAPACITY);
        let worker = thread::Builder::new()
            .name("scope-live-session".to_owned())
            .spawn(move || run_worker(config, command_rx, control_tx, data_tx))?;
        Ok(Self {
            command_tx,
            control_rx,
            data_rx,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub fn configure(&self, configure: Configure) -> Result<(), SessionError> {
        self.send(SessionCommand::Configure(configure))
    }

    pub fn start(&self) -> Result<(), SessionError> {
        self.send(SessionCommand::Start)
    }

    pub fn stop(&self) -> Result<(), SessionError> {
        self.send(SessionCommand::Stop)
    }

    pub fn ping(&self, nonce: u64) -> Result<(), SessionError> {
        self.send(SessionCommand::Ping(nonce))
    }

    pub fn set_recording(&self, recording: Option<RecordingIngress>) -> Result<(), SessionError> {
        let (ack_tx, ack_rx) = bounded(1);
        self.send(SessionCommand::SetRecording(recording, ack_tx))?;
        ack_rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| SessionError::RecordingAttachmentTimeout)
    }

    pub fn try_recv(&self) -> Result<SessionEvent, crossbeam_channel::TryRecvError> {
        match self.control_rx.try_recv() {
            Ok(event) => Ok(event),
            Err(crossbeam_channel::TryRecvError::Empty) => self.data_rx.try_recv(),
            Err(crossbeam_channel::TryRecvError::Disconnected) => self.data_rx.try_recv(),
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<SessionEvent, RecvTimeoutError> {
        if let Ok(event) = self.try_recv() {
            return Ok(event);
        }
        let timeout_rx = after(timeout);
        select! {
            recv(self.control_rx) -> event => event.map_err(|_| RecvTimeoutError::Disconnected),
            recv(self.data_rx) -> event => event.map_err(|_| RecvTimeoutError::Disconnected),
            recv(timeout_rx) -> _ => Err(RecvTimeoutError::Timeout),
        }
    }

    pub fn disconnect(&self) -> Result<(), SessionError> {
        self.send(SessionCommand::Disconnect)?;
        let worker = self
            .worker
            .lock()
            .expect("live worker mutex poisoned")
            .take();
        if let Some(worker) = worker {
            worker.join().map_err(|_| SessionError::WorkerPanicked)?;
        }
        Ok(())
    }

    fn send(&self, command: SessionCommand) -> Result<(), SessionError> {
        self.command_tx
            .send_timeout(command, COMMAND_SEND_TIMEOUT)
            .map_err(|error| match error {
                crossbeam_channel::SendTimeoutError::Timeout(_) => SessionError::CommandQueueFull,
                crossbeam_channel::SendTimeoutError::Disconnected(_) => {
                    SessionError::CommandChannelClosed
                }
            })
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        let _ = self.command_tx.try_send(SessionCommand::Disconnect);
        if let Ok(worker) = self.worker.get_mut() {
            // Detach instead of an unbounded join. Explicit disconnect still joins and reports
            // panics; Drop must remain bounded during application shutdown or a stuck driver.
            worker.take();
        }
    }
}

fn run_worker(
    config: TransportConfig,
    command_rx: Receiver<SessionCommand>,
    control_tx: Sender<SessionEvent>,
    data_tx: Sender<SessionEvent>,
) {
    let _ = send_control(
        &control_tx,
        SessionEvent::State(ConnectionState::Connecting),
    );
    let result = worker_loop(config, &command_rx, &control_tx, &data_tx);
    if let Err(error) = result {
        let _ = control_tx.try_send(SessionEvent::Error(error.to_string()));
    }
    let _ = control_tx.try_send(SessionEvent::State(ConnectionState::Disconnected));
}

fn worker_loop(
    config: TransportConfig,
    command_rx: &Receiver<SessionCommand>,
    control_tx: &Sender<SessionEvent>,
    data_tx: &Sender<SessionEvent>,
) -> Result<(), SessionError> {
    let mut transport = config.connect()?;
    send_control(
        control_tx,
        SessionEvent::State(ConnectionState::Handshaking),
    )?;
    let mut out_sequence = 1_u32;
    write_message(
        &mut transport,
        &mut out_sequence,
        0,
        Message::Hello(Hello {
            client_capabilities: 0b111,
            max_payload: MAX_PAYLOAD_LEN as u32,
            client_name: "ScopeAnalyzer".to_owned(),
        }),
    )?;
    let mut decoder = FrameDecoder::default();
    let mut read_buffer = [0_u8; 16 * 1024];
    let mut session_id = 0_u32;
    let mut hello_ack: Option<HelloAck> = None;
    let mut table: Option<ChannelTable> = None;
    let mut pending = HashMap::new();
    let mut stats = SessionStats::default();
    let mut last_frame_sequence: Option<u32> = None;
    let mut next_sample_index: Option<u64> = None;
    let mut last_received = Instant::now();
    let mut last_ping = Instant::now();
    let mut ping_nonce = 1_u64;
    let mut pending_host_gap: Option<LiveGap> = None;
    let mut recording: Option<RecordingIngress> = None;
    let mut streaming = false;

    loop {
        while let Ok(command) = command_rx.try_recv() {
            match command {
                SessionCommand::Configure(configure) => {
                    let requested = configure.clone();
                    let sequence = write_message(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        Message::Configure(configure),
                    )?;
                    pending.insert(sequence, PendingCommand::Configure(requested));
                    send_control(
                        control_tx,
                        SessionEvent::State(ConnectionState::Configuring),
                    )?;
                }
                SessionCommand::Start => {
                    let sequence = write_message(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        Message::Start,
                    )?;
                    pending.insert(sequence, PendingCommand::Start);
                }
                SessionCommand::Stop => {
                    let sequence = write_message(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        Message::Stop,
                    )?;
                    pending.insert(sequence, PendingCommand::Stop);
                }
                SessionCommand::Ping(nonce) => {
                    write_message(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        Message::Ping(nonce),
                    )?;
                }
                SessionCommand::SetRecording(next, ack_tx) => {
                    recording = next;
                    let _ = ack_tx.send(());
                }
                SessionCommand::Disconnect => {
                    if streaming {
                        let _ = write_message(
                            &mut transport,
                            &mut out_sequence,
                            session_id,
                            Message::Stop,
                        );
                    }
                    return Ok(());
                }
            }
        }

        if session_id != 0 && last_received.elapsed() >= Duration::from_secs(3) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "no valid SCP1 frame received for 3 seconds",
            )
            .into());
        }
        if session_id != 0 && last_ping.elapsed() >= Duration::from_secs(1) {
            write_message(
                &mut transport,
                &mut out_sequence,
                session_id,
                Message::Ping(ping_nonce),
            )?;
            ping_nonce = ping_nonce.wrapping_add(1);
            last_ping = Instant::now();
        }

        match transport.read(&mut read_buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => decoder.push(&read_buffer[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(error) => return Err(error.into()),
        }
        let decoder_stats = decoder.stats();
        let decoder_stats_changed = stats.crc_errors != decoder_stats.crc_errors
            || stats.malformed_headers != decoder_stats.malformed_headers
            || stats.discarded_bytes != decoder_stats.discarded_bytes;
        stats.crc_errors = decoder_stats.crc_errors;
        stats.malformed_headers = decoder_stats.malformed_headers;
        stats.discarded_bytes = decoder_stats.discarded_bytes;
        for frame in decoder.drain_frames() {
            last_received = Instant::now();
            stats.received_frames = stats.received_frames.saturating_add(1);
            if session_id != 0 && frame.session_id != session_id {
                stats.protocol_errors = stats.protocol_errors.saturating_add(1);
                send_control(
                    control_tx,
                    SessionEvent::Error(format!(
                        "unexpected session id {}, expected {session_id}",
                        frame.session_id
                    )),
                )?;
                continue;
            }
            if let Some(previous_sequence) = last_frame_sequence {
                let expected = previous_sequence.wrapping_add(1);
                if frame.sequence != expected {
                    stats.sequence_gaps = stats.sequence_gaps.saturating_add(1);
                }
            }
            last_frame_sequence = Some(frame.sequence);
            let message = match Message::decode(frame.message_type, &frame.payload) {
                Ok(message) => message,
                Err(error) => {
                    stats.protocol_errors = stats.protocol_errors.saturating_add(1);
                    if matches!(&error, ProtocolError::UnknownMessageType(_)) {
                        stats.unknown_messages = stats.unknown_messages.saturating_add(1);
                    }
                    send_control(control_tx, SessionEvent::Error(error.to_string()))?;
                    continue;
                }
            };
            match message {
                Message::HelloAck(hello) => {
                    if frame.session_id == 0 {
                        return Err(ProtocolError::InvalidPayload(
                            "HELLO_ACK session id must be non-zero".to_owned(),
                        )
                        .into());
                    }
                    hello_ack = Some(hello.clone());
                    session_id = frame.session_id;
                    send_control(control_tx, SessionEvent::HelloAck(hello))?;
                }
                Message::ChannelTable(channel_table) => {
                    let hello = hello_ack.as_ref().ok_or_else(|| {
                        ProtocolError::InvalidPayload(
                            "CHANNEL_TABLE arrived before HELLO_ACK".to_owned(),
                        )
                    })?;
                    if usize::from(hello.channel_count) != channel_table.channels.len() {
                        return Err(ProtocolError::InvalidPayload(format!(
                            "CHANNEL_TABLE count {} does not match HELLO_ACK count {}",
                            channel_table.channels.len(),
                            hello.channel_count
                        ))
                        .into());
                    }
                    table = Some(channel_table.clone());
                    send_control(control_tx, SessionEvent::ChannelTable(channel_table))?;
                    send_control(control_tx, SessionEvent::State(ConnectionState::Ready))?;
                }
                Message::CommandResult(result) => {
                    if result.result_code == ResultCode::Ok {
                        if let Some(command) = pending.remove(&result.request_sequence) {
                            let state = match command {
                                PendingCommand::Configure(requested) => {
                                    let actual = decode_configure_result_detail(&result.detail)?;
                                    let hello = hello_ack.as_ref().ok_or_else(|| {
                                        ProtocolError::InvalidPayload(
                                            "CONFIGURE completed before HELLO_ACK".to_owned(),
                                        )
                                    })?;
                                    let channel_table = table.as_ref().ok_or_else(|| {
                                        ProtocolError::InvalidPayload(
                                            "CONFIGURE completed before CHANNEL_TABLE".to_owned(),
                                        )
                                    })?;
                                    validate_configure_for_device(&actual, hello, channel_table)?;
                                    if actual.channel_mask & requested.channel_mask
                                        != actual.channel_mask
                                    {
                                        return Err(ProtocolError::InvalidPayload(
                                            "device enabled channels not requested by client"
                                                .to_owned(),
                                        )
                                        .into());
                                    }
                                    send_control(control_tx, SessionEvent::Configured(actual))?;
                                    ConnectionState::Ready
                                }
                                PendingCommand::Stop => {
                                    streaming = false;
                                    ConnectionState::Ready
                                }
                                PendingCommand::Start => {
                                    streaming = true;
                                    ConnectionState::Streaming
                                }
                            };
                            send_control(control_tx, SessionEvent::State(state))?;
                        }
                    } else if pending
                        .remove(&result.request_sequence)
                        .is_some_and(|command| matches!(command, PendingCommand::Configure(_)))
                    {
                        send_control(control_tx, SessionEvent::State(ConnectionState::Ready))?;
                    }
                    send_control(control_tx, SessionEvent::CommandResult(result))?;
                }
                Message::SampleBatch(_) => {
                    let Some(table) = &table else {
                        stats.protocol_errors = stats.protocol_errors.saturating_add(1);
                        continue;
                    };
                    let decoded = match decode_sample_frame(&frame, table) {
                        Ok(decoded) => decoded,
                        Err(error) => {
                            stats.protocol_errors = stats.protocol_errors.saturating_add(1);
                            send_control(
                                control_tx,
                                SessionEvent::RecordingError(error.to_string()),
                            )?;
                            continue;
                        }
                    };
                    if let Some(expected) = next_sample_index {
                        if decoded.first_sample_index > expected {
                            let gap = LiveGap {
                                start_sample_index: expected,
                                missing_samples: decoded.first_sample_index - expected,
                                reason: GapReason::SampleIndexLoss,
                            };
                            if let Some(ingress) = &recording {
                                if let Err(error) =
                                    ingress.try_write_gap(gap, frame.timestamp_ticks)
                                {
                                    recording = None;
                                    send_control(
                                        control_tx,
                                        SessionEvent::RecordingError(error.to_string()),
                                    )?;
                                }
                            }
                            let _ = data_tx.try_send(SessionEvent::Gap(gap));
                        } else if decoded.first_sample_index < expected {
                            stats.protocol_errors = stats.protocol_errors.saturating_add(1);
                            continue;
                        }
                    }
                    if let Some(ingress) = &recording {
                        if let Err(error) = ingress.try_write_sample_frame(frame.clone()) {
                            recording = None;
                            send_control(control_tx, SessionEvent::Error(error.to_string()))?;
                        }
                    }
                    let sample_count = decoded.channels.first().map(Vec::len).unwrap_or(0);
                    next_sample_index = decoded.first_sample_index.checked_add(sample_count as u64);
                    stats.received_batches = stats.received_batches.saturating_add(1);
                    stats.received_samples =
                        stats.received_samples.saturating_add(sample_count as u64);
                    if let Some(gap) = pending_host_gap {
                        match data_tx.try_send(SessionEvent::Gap(gap)) {
                            Ok(()) => pending_host_gap = None,
                            Err(TrySendError::Disconnected(_)) => return Ok(()),
                            Err(TrySendError::Full(_)) => {}
                        }
                    }
                    let dropped_start = decoded.first_sample_index;
                    match data_tx.try_send(SessionEvent::Batch(decoded)) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {
                            stats.host_dropped_batches =
                                stats.host_dropped_batches.saturating_add(1);
                            if let Some(gap) = &mut pending_host_gap {
                                gap.missing_samples =
                                    gap.missing_samples.saturating_add(sample_count as u64);
                            } else {
                                pending_host_gap = Some(LiveGap {
                                    start_sample_index: dropped_start,
                                    missing_samples: sample_count as u64,
                                    reason: GapReason::HostBackpressure,
                                });
                            }
                        }
                        Err(TrySendError::Disconnected(_)) => return Ok(()),
                    }
                    let _ = data_tx.try_send(SessionEvent::Stats(stats));
                }
                Message::Error(error) => {
                    send_control(control_tx, SessionEvent::Error(error.detail))?;
                }
                Message::Status(status) => {
                    stats.device_dropped_samples = status.dropped_samples;
                    stats.device_tx_overruns = u64::from(status.tx_overruns);
                    let _ = data_tx.try_send(SessionEvent::Stats(stats));
                }
                Message::Ping(nonce) => {
                    write_message(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        Message::Pong(nonce),
                    )?;
                }
                Message::Pong(_) => {}
                _ => {}
            }
        }
        if decoder_stats_changed {
            let _ = data_tx.try_send(SessionEvent::Stats(stats));
        }
    }
}

fn send_control(
    control_tx: &Sender<SessionEvent>,
    event: SessionEvent,
) -> Result<(), SessionError> {
    control_tx
        .send_timeout(event, CONTROL_SEND_TIMEOUT)
        .map_err(|_| SessionError::ControlEventBackpressure)
}

fn write_message(
    transport: &mut TransportStream,
    next_sequence: &mut u32,
    session_id: u32,
    message: Message,
) -> Result<u32, SessionError> {
    let sequence = *next_sequence;
    let frame = Frame::new(
        message.message_type(),
        0,
        sequence,
        session_id,
        0,
        message.encode_payload()?,
    );
    transport.write_all(&frame.encode()?)?;
    transport.flush()?;
    *next_sequence = next_sequence.wrapping_add(1);
    Ok(sequence)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::live::{
        protocol::Configure,
        simulator::{SimulatorConfig, SimulatorHandle},
        transport::TransportConfig,
    };

    fn wait_for(
        session: &LiveSession,
        timeout: Duration,
        predicate: impl Fn(&SessionEvent) -> bool,
    ) -> SessionEvent {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = session
                .recv_timeout(remaining)
                .expect("timed out waiting for live session event");
            if predicate(&event) {
                return event;
            }
        }
    }

    #[test]
    fn tcp_simulator_session_handshake_stream_and_stop() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();

        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Ready))
        });
        session
            .configure(Configure {
                sample_rate_hz: 10_000,
                batch_samples: 10,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_for(
            &session,
            Duration::from_secs(2),
            |event| matches!(event, SessionEvent::CommandResult(result) if result.result_code == crate::live::protocol::ResultCode::Ok),
        );
        session.start().unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Streaming))
        });
        let first = wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::Batch(_))
        });
        let second = wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::Batch(_))
        });
        let (SessionEvent::Batch(first), SessionEvent::Batch(second)) = (first, second) else {
            unreachable!();
        };
        assert_eq!(first.channel_ids, vec![0, 1, 2, 3]);
        assert_eq!(first.channels[0].len(), 10);
        assert_eq!(second.first_sample_index, first.first_sample_index + 10);

        session.stop().unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Ready))
        });
        session.disconnect().unwrap();
    }

    #[test]
    fn tcp_simulator_drop_is_reported_as_sample_gap() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
            drop_every: Some(2),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Ready))
        });
        session
            .configure(Configure {
                sample_rate_hz: 10_000,
                batch_samples: 5,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_for(
            &session,
            Duration::from_secs(2),
            |event| matches!(event, SessionEvent::CommandResult(result) if result.result_code == crate::live::protocol::ResultCode::Ok),
        );
        session.start().unwrap();

        let gap = wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::Gap(_))
        });
        let SessionEvent::Gap(gap) = gap else {
            unreachable!();
        };
        assert_eq!(gap.missing_samples, 5);
        session.disconnect().unwrap();
    }

    #[test]
    fn configure_success_reports_the_actual_device_parameters() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Ready))
        });
        let requested = Configure {
            sample_rate_hz: 20_000,
            batch_samples: 25,
            channel_mask: 0b1011,
        };

        session.configure(requested.clone()).unwrap();
        let event = wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::Configured(_))
        });

        assert!(matches!(event, SessionEvent::Configured(actual) if actual == requested));
        session.disconnect().unwrap();
    }

    #[test]
    fn simulator_rejects_a_channel_mask_outside_its_channel_table() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Ready))
        });

        session
            .configure(Configure {
                sample_rate_hz: 10_000,
                batch_samples: 10,
                channel_mask: 1 << 60,
            })
            .unwrap();
        let event = wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::CommandResult(_))
        });

        assert!(matches!(
            event,
            SessionEvent::CommandResult(result)
                if result.result_code == crate::live::protocol::ResultCode::InvalidArgument
        ));
        session.disconnect().unwrap();
    }

    #[test]
    fn corrupt_frames_are_counted_without_reaching_batch_consumers() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
            corrupt_every: Some(2),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Ready))
        });
        session
            .configure(Configure {
                sample_rate_hz: 10_000,
                batch_samples: 10,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::Configured(_))
        });
        session.start().unwrap();

        let stats = wait_for(
            &session,
            Duration::from_secs(2),
            |event| matches!(event, SessionEvent::Stats(stats) if stats.crc_errors > 0),
        );

        assert!(matches!(stats, SessionEvent::Stats(stats) if stats.crc_errors > 0));
        session.disconnect().unwrap();
    }

    #[test]
    fn disconnect_sends_stop_to_a_streaming_device() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Ready))
        });
        session
            .configure(Configure {
                sample_rate_hz: 10_000,
                batch_samples: 10,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::Configured(_))
        });
        session.start().unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Streaming))
        });

        session.disconnect().unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while simulator.stats().stop_requests == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(simulator.stats().stop_requests, 1);
    }

    #[test]
    fn disconnect_remains_bounded_when_display_events_are_backpressured() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
            drop_every: Some(2),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = Arc::new(
            LiveSession::connect(TransportConfig::Tcp {
                address: simulator.address().to_string(),
            })
            .unwrap(),
        );
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Ready))
        });
        session
            .configure(Configure {
                sample_rate_hz: 10_000,
                batch_samples: 10,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::Configured(_))
        });
        session.start().unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Streaming))
        });
        std::thread::sleep(Duration::from_millis(600));
        let (done_tx, done_rx) = crossbeam_channel::bounded(1);
        let worker_session = Arc::clone(&session);
        std::thread::spawn(move || {
            let _ = done_tx.send(worker_session.disconnect());
        });

        assert!(done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("disconnect must not block on a full display queue")
            .is_ok());
    }

    #[test]
    fn simulator_accepts_a_fresh_session_after_disconnect() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            ..SimulatorConfig::default()
        })
        .unwrap();

        for _ in 0..2 {
            let session = LiveSession::connect(TransportConfig::Tcp {
                address: simulator.address().to_string(),
            })
            .unwrap();
            wait_for(&session, Duration::from_secs(2), |event| {
                matches!(event, SessionEvent::State(ConnectionState::Ready))
            });
            session.disconnect().unwrap();
        }

        assert_eq!(simulator.stats().connections, 2);
        assert_eq!(simulator.stats().hello_requests, 2);
    }

    #[test]
    fn client_and_device_exchange_bidirectional_heartbeats() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Ready))
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        while (simulator.stats().ping_requests == 0
            || simulator.stats().pings_sent == 0
            || simulator.stats().pongs_received == 0)
            && Instant::now() < deadline
        {
            let _ = session.recv_timeout(Duration::from_millis(50));
        }

        let stats = simulator.stats();
        assert!(stats.ping_requests > 0, "device must receive client PING");
        assert!(stats.pings_sent > 0, "device must initiate PING");
        assert!(stats.pongs_received > 0, "client must answer device PING");
        session.disconnect().unwrap();
    }

    #[test]
    fn control_frames_interleaved_with_samples_do_not_report_sequence_gaps() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Ready))
        });
        session
            .configure(Configure {
                sample_rate_hz: 100,
                batch_samples: 10,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::Configured(_))
        });
        session.start().unwrap();

        let deadline = Instant::now() + Duration::from_millis(1_400);
        let mut latest = SessionStats::default();
        while Instant::now() < deadline {
            if let Ok(SessionEvent::Stats(stats)) = session.recv_timeout(Duration::from_millis(50))
            {
                latest = stats;
            }
        }

        assert!(latest.received_batches >= 8);
        assert_eq!(latest.sequence_gaps, 0);
        assert!(simulator.stats().pings_sent > 0);
        session.disconnect().unwrap();
    }
}
