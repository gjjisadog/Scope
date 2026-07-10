use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::Mutex,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender, TrySendError};
use thiserror::Error;

use super::{
    buffer::{GapReason, LiveGap},
    protocol::{
        decode_sample_frame, ChannelTable, CommandResult, Configure, DecodedSampleBatch, Frame,
        FrameDecoder, Hello, HelloAck, Message, ProtocolError, ResultCode, MAX_PAYLOAD_LEN,
    },
    transport::{TransportConfig, TransportError, TransportStream},
};

const COMMAND_CAPACITY: usize = 32;
const EVENT_CAPACITY: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Handshaking,
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
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    State(ConnectionState),
    HelloAck(HelloAck),
    ChannelTable(ChannelTable),
    CommandResult(CommandResult),
    Batch(DecodedSampleBatch),
    Gap(LiveGap),
    Stats(SessionStats),
    Error(String),
}

enum SessionCommand {
    Configure(Configure),
    Start,
    Stop,
    Ping(u64),
    Disconnect,
}

#[derive(Clone, Copy)]
enum PendingCommand {
    Configure,
    Start,
    Stop,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("live session command channel is closed")]
    CommandChannelClosed,
    #[error("live session worker panicked")]
    WorkerPanicked,
    #[error("live transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("live protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("live session I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct LiveSession {
    command_tx: Sender<SessionCommand>,
    event_rx: Receiver<SessionEvent>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl LiveSession {
    pub fn connect(config: TransportConfig) -> Result<Self, SessionError> {
        config.validate()?;
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let worker = thread::Builder::new()
            .name("scope-live-session".to_owned())
            .spawn(move || run_worker(config, command_rx, event_tx))?;
        Ok(Self {
            command_tx,
            event_rx,
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

    pub fn try_recv(&self) -> Result<SessionEvent, crossbeam_channel::TryRecvError> {
        self.event_rx.try_recv()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<SessionEvent, RecvTimeoutError> {
        self.event_rx.recv_timeout(timeout)
    }

    pub fn disconnect(&self) -> Result<(), SessionError> {
        let _ = self.send(SessionCommand::Disconnect);
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
            .send(command)
            .map_err(|_| SessionError::CommandChannelClosed)
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        let _ = self.command_tx.send(SessionCommand::Disconnect);
        if let Ok(worker) = self.worker.get_mut() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

fn run_worker(
    config: TransportConfig,
    command_rx: Receiver<SessionCommand>,
    event_tx: Sender<SessionEvent>,
) {
    let _ = event_tx.send(SessionEvent::State(ConnectionState::Connecting));
    let result = worker_loop(config, &command_rx, &event_tx);
    if let Err(error) = result {
        let _ = event_tx.send(SessionEvent::Error(error.to_string()));
    }
    let _ = event_tx.send(SessionEvent::State(ConnectionState::Disconnected));
}

fn worker_loop(
    config: TransportConfig,
    command_rx: &Receiver<SessionCommand>,
    event_tx: &Sender<SessionEvent>,
) -> Result<(), SessionError> {
    let mut transport = config.connect()?;
    let _ = event_tx.send(SessionEvent::State(ConnectionState::Handshaking));
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
    let mut table: Option<ChannelTable> = None;
    let mut pending = HashMap::new();
    let mut stats = SessionStats::default();
    let mut last_batch_sequence: Option<u32> = None;
    let mut next_sample_index: Option<u64> = None;
    let mut last_received = Instant::now();
    let mut last_ping = Instant::now();
    let mut ping_nonce = 1_u64;
    let mut pending_host_gap: Option<LiveGap> = None;

    loop {
        while let Ok(command) = command_rx.try_recv() {
            match command {
                SessionCommand::Configure(configure) => {
                    let sequence = write_message(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        Message::Configure(configure),
                    )?;
                    pending.insert(sequence, PendingCommand::Configure);
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
                SessionCommand::Disconnect => return Ok(()),
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
        for frame in decoder.drain_frames() {
            last_received = Instant::now();
            stats.received_frames = stats.received_frames.saturating_add(1);
            if session_id != 0 && frame.session_id != session_id {
                stats.protocol_errors = stats.protocol_errors.saturating_add(1);
                let _ = event_tx.send(SessionEvent::Error(format!(
                    "unexpected session id {}, expected {session_id}",
                    frame.session_id
                )));
                continue;
            }
            let message = match Message::decode(frame.message_type, &frame.payload) {
                Ok(message) => message,
                Err(error) => {
                    stats.protocol_errors = stats.protocol_errors.saturating_add(1);
                    let _ = event_tx.send(SessionEvent::Error(error.to_string()));
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
                    session_id = frame.session_id;
                    let _ = event_tx.send(SessionEvent::HelloAck(hello));
                }
                Message::ChannelTable(channel_table) => {
                    table = Some(channel_table.clone());
                    let _ = event_tx.send(SessionEvent::ChannelTable(channel_table));
                    let _ = event_tx.send(SessionEvent::State(ConnectionState::Ready));
                }
                Message::CommandResult(result) => {
                    if result.result_code == ResultCode::Ok {
                        if let Some(command) = pending.remove(&result.request_sequence) {
                            let state = match command {
                                PendingCommand::Configure | PendingCommand::Stop => {
                                    ConnectionState::Ready
                                }
                                PendingCommand::Start => ConnectionState::Streaming,
                            };
                            let _ = event_tx.send(SessionEvent::State(state));
                        }
                    }
                    let _ = event_tx.send(SessionEvent::CommandResult(result));
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
                            let _ = event_tx.send(SessionEvent::Error(error.to_string()));
                            continue;
                        }
                    };
                    if let Some(previous_sequence) = last_batch_sequence {
                        let expected = previous_sequence.wrapping_add(1);
                        if frame.sequence != expected {
                            stats.sequence_gaps = stats.sequence_gaps.saturating_add(1);
                        }
                    }
                    if let Some(expected) = next_sample_index {
                        if decoded.first_sample_index > expected {
                            let gap = LiveGap {
                                start_sample_index: expected,
                                missing_samples: decoded.first_sample_index - expected,
                                reason: GapReason::SampleIndexLoss,
                            };
                            let _ = event_tx.send(SessionEvent::Gap(gap));
                        } else if decoded.first_sample_index < expected {
                            stats.protocol_errors = stats.protocol_errors.saturating_add(1);
                            continue;
                        }
                    }
                    let sample_count = decoded.channels.first().map(Vec::len).unwrap_or(0);
                    next_sample_index = decoded.first_sample_index.checked_add(sample_count as u64);
                    last_batch_sequence = Some(frame.sequence);
                    stats.received_batches = stats.received_batches.saturating_add(1);
                    stats.received_samples =
                        stats.received_samples.saturating_add(sample_count as u64);
                    if let Some(gap) = pending_host_gap {
                        match event_tx.try_send(SessionEvent::Gap(gap)) {
                            Ok(()) => pending_host_gap = None,
                            Err(TrySendError::Disconnected(_)) => return Ok(()),
                            Err(TrySendError::Full(_)) => {}
                        }
                    }
                    let dropped_start = decoded.first_sample_index;
                    match event_tx.try_send(SessionEvent::Batch(decoded)) {
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
                    let _ = event_tx.try_send(SessionEvent::Stats(stats));
                }
                Message::Error(error) => {
                    let _ = event_tx.send(SessionEvent::Error(error.detail));
                }
                Message::Status(_) | Message::Pong(_) | Message::Ping(_) => {}
                _ => {}
            }
        }
    }
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
    use std::time::{Duration, Instant};

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
}
