use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crossbeam_channel::{after, bounded, select, Receiver, RecvTimeoutError, Sender, TrySendError};
use thiserror::Error;

use super::{
    buffer::{GapReason, LiveBuffer, LiveGap, LiveSnapshot},
    hardware_capture::{AssembledCapture, CaptureAssembler},
    protocol::{
        decode_configure_result_detail, decode_sample_frame, validate_configure_for_device,
        ChannelTable, CommandResult, Configure, DecodedSampleBatch, Frame, FrameDecoder, Hello,
        HelloAck, Message, ProtocolError, ResultCode, MAX_PAYLOAD_LEN, PROTOCOL_VERSION_V2,
    },
    protocol_v2::{
        decode_stream_sample_frame, ArmCapture, CancelCapture, CaptureStatus, ConfigureStream,
        DecodedStreamSampleBatch, ManualTrigger, MessageV2, StreamTable, CAPABILITY_V2_STREAMS,
        MSG_CAPTURE_BEGIN, MSG_CAPTURE_DATA, MSG_CAPTURE_END, MSG_STREAM_SAMPLE_BATCH,
    },
    recording::{RecordingError, RecordingIngress},
    snapshot::{SnapshotDiagnostics, SnapshotValidator},
    transport::{TransportConfig, TransportError, TransportStream},
    trigger::{TriggerCapture, TriggerConfig, TriggerEngine},
};

const COMMAND_CAPACITY: usize = 32;
const CONTROL_EVENT_CAPACITY: usize = 64;
const EVENT_CAPACITY: usize = 256;
const CONTROL_SEND_TIMEOUT: Duration = Duration::from_millis(100);
const COMMAND_SEND_TIMEOUT: Duration = Duration::from_millis(100);
const DISPLAY_SNAPSHOT_INTERVAL: Duration = Duration::from_millis(33);
const DISPLAY_SNAPSHOT_POINTS: usize = 8_000;

#[derive(Clone, Debug)]
pub struct AcquisitionConfig {
    pub history_seconds: u32,
    pub trigger: TriggerConfig,
}

impl Default for AcquisitionConfig {
    fn default() -> Self {
        Self {
            history_seconds: 1,
            trigger: TriggerConfig::default(),
        }
    }
}

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
pub enum LiveProtocol {
    #[default]
    V1,
    V2,
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
    pub row_sequence_gaps: u64,
    pub row_sequence_reorders: u64,
    pub source_sequence_faults: u64,
    pub applied_sequence_faults: u64,
    pub invalid_snapshot_rows: u64,
    pub missing_causal_source: u64,
    pub causal_source_mismatch: u64,
    pub causal_application_mismatch: u64,
    pub causal_sequence_reorder: u64,
    pub causal_group_mismatch: u64,
    pub host_dropped_v2_batches: u64,
    pub host_dropped_v2_rows: u64,
    pub v2_snapshot_queue_overruns: u64,
    pub capture_processing_overruns: u64,
    pub last_dropped_v2_stream_id: Option<u16>,
    pub last_dropped_v2_first_row: Option<u64>,
    pub last_dropped_v2_last_row: Option<u64>,
    pub last_pong_nonce: Option<u64>,
    pub heartbeat_round_trip_count: u64,
    pub heartbeat_timeout_count: u64,
    pub device_state: Option<super::protocol::DeviceState>,
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    State(ConnectionState),
    HelloAck(HelloAck),
    ChannelTable(ChannelTable),
    StreamTable(StreamTable),
    Configured(Configure),
    ConfiguredV2(ConfigureStream),
    CommandResult(CommandResult),
    /// Retained for source compatibility; acquisition workers no longer emit
    /// a batch to the UI event queue.
    Batch(DecodedSampleBatch),
    SnapshotV2(DecodedStreamSampleBatch, SnapshotDiagnostics),
    CaptureStatus(CaptureStatus),
    CaptureComplete(AssembledCapture),
    CaptureFailure(String),
    Gap(LiveGap),
    DisplaySnapshot(Arc<LiveSnapshot>),
    TriggerCapture(TriggerCapture, TriggerConfig),
    TriggerArmed(bool),
    Stats(SessionStats),
    RecordingError(String),
    Error(String),
}

enum SessionCommand {
    Configure(Configure, AcquisitionConfig),
    ConfigureStream(ConfigureStream),
    SetTriggerConfig(TriggerConfig),
    ArmTrigger,
    Start,
    Stop,
    Ping(u64),
    ArmCapture(ArmCapture),
    ManualTrigger(ManualTrigger),
    CancelCapture(CancelCapture),
    SetRecording(Option<RecordingIngress>, Sender<()>),
    Disconnect,
}

#[derive(Clone)]
enum PendingCommand {
    Configure(Configure, AcquisitionConfig),
    ConfigureStream(ConfigureStream),
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
    #[error("live acquisition worker error: {0}")]
    Acquisition(String),
}

pub struct LiveSession {
    command_tx: Sender<SessionCommand>,
    control_rx: Receiver<SessionEvent>,
    data_rx: Receiver<SessionEvent>,
    worker: Mutex<Option<JoinHandle<()>>>,
    disconnect_requested: Arc<AtomicBool>,
}

impl LiveSession {
    pub fn connect(config: TransportConfig) -> Result<Self, SessionError> {
        Self::connect_with_protocol(config, LiveProtocol::V1)
    }

    /// Explicit V2 entry point. The desktop GUI keeps calling `connect()` and
    /// therefore remains on SCP1 V1 by default.
    pub fn connect_v2(config: TransportConfig) -> Result<Self, SessionError> {
        Self::connect_with_protocol(config, LiveProtocol::V2)
    }

    fn connect_with_protocol(
        config: TransportConfig,
        protocol: LiveProtocol,
    ) -> Result<Self, SessionError> {
        config.validate()?;
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        let (control_tx, control_rx) = bounded(CONTROL_EVENT_CAPACITY);
        let (data_tx, data_rx) = bounded(EVENT_CAPACITY);
        let disconnect_requested = Arc::new(AtomicBool::new(false));
        let worker_disconnect_requested = Arc::clone(&disconnect_requested);
        let worker = thread::Builder::new()
            .name("scope-live-session".to_owned())
            .spawn(move || {
                run_worker(
                    config,
                    protocol,
                    command_rx,
                    control_tx,
                    data_tx,
                    worker_disconnect_requested,
                )
            })?;
        Ok(Self {
            command_tx,
            control_rx,
            data_rx,
            worker: Mutex::new(Some(worker)),
            disconnect_requested,
        })
    }

    pub fn configure(&self, configure: Configure) -> Result<(), SessionError> {
        self.configure_with_acquisition(configure, AcquisitionConfig::default())
    }

    pub fn configure_with_acquisition(
        &self,
        configure: Configure,
        acquisition: AcquisitionConfig,
    ) -> Result<(), SessionError> {
        self.send(SessionCommand::Configure(configure, acquisition))
    }

    pub fn configure_stream(&self, configure: ConfigureStream) -> Result<(), SessionError> {
        self.send(SessionCommand::ConfigureStream(configure))
    }

    pub fn set_trigger_config(&self, config: TriggerConfig) -> Result<(), SessionError> {
        self.send(SessionCommand::SetTriggerConfig(config))
    }

    pub fn arm_trigger(&self) -> Result<(), SessionError> {
        self.send(SessionCommand::ArmTrigger)
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

    pub fn arm_capture(&self, capture: ArmCapture) -> Result<(), SessionError> {
        self.send(SessionCommand::ArmCapture(capture))
    }

    pub fn manual_trigger(&self, trigger: ManualTrigger) -> Result<(), SessionError> {
        self.send(SessionCommand::ManualTrigger(trigger))
    }

    pub fn cancel_capture(&self, cancel: CancelCapture) -> Result<(), SessionError> {
        self.send(SessionCommand::CancelCapture(cancel))
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
        // The atomic flag is the reliable stop path. The bounded command queue
        // may be full while the worker is draining a burst of decoded frames,
        // so a best-effort wake-up must not turn a bounded disconnect into a
        // spurious CommandQueueFull error.
        let _ = self.command_tx.try_send(SessionCommand::Disconnect);
        // Queue the graceful command before publishing the fallback flag. If
        // the worker is between event batches, this lets it write STOP through
        // the normal command path instead of observing the flag and exiting
        // before it drains the queue.
        self.disconnect_requested.store(true, Ordering::Release);
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
        self.disconnect_requested.store(true, Ordering::Release);
        let _ = self.command_tx.try_send(SessionCommand::Disconnect);
        if let Ok(worker) = self.worker.get_mut() {
            // Detach instead of an unbounded join. Explicit disconnect still joins and reports
            // panics; Drop must remain bounded during application shutdown or a stuck driver.
            worker.take();
        }
    }
}

struct AcquisitionWorker {
    buffer: LiveBuffer,
    trigger: TriggerEngine,
    last_snapshot: Instant,
}

impl AcquisitionWorker {
    fn new(
        configure: &Configure,
        hello: &HelloAck,
        table: &ChannelTable,
        config: AcquisitionConfig,
    ) -> Result<Self, SessionError> {
        let channel_ids = table
            .channels
            .iter()
            .filter(|channel| configure.channel_mask & (1_u64 << channel.channel_id) != 0)
            .map(|channel| channel.channel_id)
            .collect::<Vec<_>>();
        let capacity = u64::from(configure.sample_rate_hz)
            .checked_mul(u64::from(config.history_seconds))
            .ok_or_else(|| SessionError::Acquisition("live history capacity overflow".to_owned()))?
            .clamp(1, 5_000_000) as usize;
        if !channel_ids.contains(&config.trigger.source_channel) {
            return Err(SessionError::Acquisition(format!(
                "trigger source channel {} is not in the configured stream",
                config.trigger.source_channel
            )));
        }
        Ok(Self {
            buffer: LiveBuffer::new(channel_ids, capacity, hello.tick_hz)
                .map_err(|error| SessionError::Acquisition(error.to_string()))?,
            trigger: TriggerEngine::new(config.trigger)
                .map_err(|error| SessionError::Acquisition(error.to_string()))?,
            last_snapshot: Instant::now() - DISPLAY_SNAPSHOT_INTERVAL,
        })
    }

    fn set_trigger_config(&mut self, config: TriggerConfig) -> Result<(), SessionError> {
        if !self.buffer.channel_ids().contains(&config.source_channel) {
            return Err(SessionError::Acquisition(format!(
                "trigger source channel {} is not in the configured stream",
                config.source_channel
            )));
        }
        self.trigger
            .set_config(config)
            .map_err(|error| SessionError::Acquisition(error.to_string()))
    }

    fn arm_trigger(&mut self) {
        self.trigger.arm();
    }

    fn on_gap(&mut self, gap: LiveGap) {
        self.trigger.on_gap();
        self.buffer
            .push_gap(gap.start_sample_index, gap.missing_samples, gap.reason);
    }

    fn process(
        &mut self,
        batch: DecodedSampleBatch,
    ) -> Result<(Vec<TriggerCapture>, Option<Arc<LiveSnapshot>>), SessionError> {
        let captures = self
            .trigger
            .feed_all(&batch)
            .map_err(|error| SessionError::Acquisition(error.to_string()))?;
        self.buffer
            .push_batch(batch)
            .map_err(|error| SessionError::Acquisition(error.to_string()))?;
        let snapshot = if self.last_snapshot.elapsed() >= DISPLAY_SNAPSHOT_INTERVAL {
            self.last_snapshot = Instant::now();
            Some(Arc::new(self.buffer.snapshot(DISPLAY_SNAPSHOT_POINTS)))
        } else {
            None
        };
        Ok((captures, snapshot))
    }
}

fn run_worker(
    config: TransportConfig,
    protocol: LiveProtocol,
    command_rx: Receiver<SessionCommand>,
    control_tx: Sender<SessionEvent>,
    data_tx: Sender<SessionEvent>,
    disconnect_requested: Arc<AtomicBool>,
) {
    let _ = send_control(
        &control_tx,
        SessionEvent::State(ConnectionState::Connecting),
    );
    let result = match protocol {
        LiveProtocol::V1 => worker_loop(
            config,
            &command_rx,
            &control_tx,
            &data_tx,
            &disconnect_requested,
        ),
        LiveProtocol::V2 => worker_loop_v2(
            config,
            &command_rx,
            &control_tx,
            &data_tx,
            &disconnect_requested,
        ),
    };
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
    disconnect_requested: &AtomicBool,
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
    let mut recording: Option<RecordingIngress> = None;
    let mut acquisition: Option<AcquisitionWorker> = None;
    let mut streaming = false;
    let mut disconnecting = false;
    let mut disconnect_deadline = None;

    loop {
        while let Ok(command) = command_rx.try_recv() {
            match command {
                SessionCommand::Configure(configure, acquisition_config) => {
                    let requested = configure.clone();
                    let sequence = write_message(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        Message::Configure(configure),
                    )?;
                    pending.insert(
                        sequence,
                        PendingCommand::Configure(requested, acquisition_config),
                    );
                    send_control(
                        control_tx,
                        SessionEvent::State(ConnectionState::Configuring),
                    )?;
                }
                SessionCommand::ConfigureStream(_)
                | SessionCommand::ArmCapture(_)
                | SessionCommand::ManualTrigger(_)
                | SessionCommand::CancelCapture(_) => {
                    return Err(SessionError::Acquisition(
                        "SCP1 V2 command sent to a V1 session".to_owned(),
                    ));
                }
                SessionCommand::SetTriggerConfig(config) => {
                    if let Some(worker) = &mut acquisition {
                        worker.set_trigger_config(config)?;
                        send_control(
                            control_tx,
                            SessionEvent::TriggerArmed(worker.trigger.is_armed()),
                        )?;
                    }
                }
                SessionCommand::ArmTrigger => {
                    if let Some(worker) = &mut acquisition {
                        worker.arm_trigger();
                        send_control(control_tx, SessionEvent::TriggerArmed(true))?;
                    }
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
                    if streaming && !disconnecting {
                        let sequence = write_message(
                            &mut transport,
                            &mut out_sequence,
                            session_id,
                            Message::Stop,
                        )?;
                        pending.insert(sequence, PendingCommand::Stop);
                        disconnecting = true;
                        disconnect_deadline = Some(Instant::now() + Duration::from_millis(500));
                    } else if !streaming {
                        return Ok(());
                    }
                }
            }
        }

        if disconnect_requested.load(Ordering::Acquire) && !disconnecting {
            if streaming {
                let sequence =
                    write_message(&mut transport, &mut out_sequence, session_id, Message::Stop)?;
                pending.insert(sequence, PendingCommand::Stop);
                disconnecting = true;
                disconnect_deadline = Some(Instant::now() + Duration::from_millis(500));
            } else {
                return Ok(());
            }
        }
        if disconnect_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(());
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
            if disconnect_requested.load(Ordering::Acquire) && !disconnecting {
                if streaming {
                    let sequence = write_message(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        Message::Stop,
                    )?;
                    pending.insert(sequence, PendingCommand::Stop);
                    disconnecting = true;
                    disconnect_deadline = Some(Instant::now() + Duration::from_millis(500));
                } else {
                    return Ok(());
                }
            }
            if disconnect_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(());
            }
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
                    let mut disconnect_complete = false;
                    if result.result_code == ResultCode::Ok {
                        if let Some(command) = pending.remove(&result.request_sequence) {
                            let state = match command {
                                PendingCommand::Configure(requested, acquisition_config) => {
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
                                    acquisition = Some(AcquisitionWorker::new(
                                        &actual,
                                        hello,
                                        channel_table,
                                        acquisition_config,
                                    )?);
                                    send_control(control_tx, SessionEvent::Configured(actual))?;
                                    ConnectionState::Ready
                                }
                                PendingCommand::ConfigureStream(_) => ConnectionState::Ready,
                                PendingCommand::Stop => {
                                    streaming = false;
                                    disconnect_complete = disconnecting;
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
                        .is_some_and(|command| matches!(command, PendingCommand::Configure(_, _)))
                    {
                        send_control(control_tx, SessionEvent::State(ConnectionState::Ready))?;
                    }
                    send_control(control_tx, SessionEvent::CommandResult(result))?;
                    if disconnect_complete {
                        return Ok(());
                    }
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
                                    send_control(
                                        control_tx,
                                        stop_recording_after_write_error(&mut recording, error),
                                    )?;
                                }
                            }
                            if let Some(worker) = &mut acquisition {
                                worker.on_gap(gap);
                            }
                        } else if decoded.first_sample_index < expected {
                            stats.protocol_errors = stats.protocol_errors.saturating_add(1);
                            continue;
                        }
                    }
                    if let Some(ingress) = &recording {
                        if let Err(error) = ingress.try_write_sample_frame(frame.clone()) {
                            send_control(
                                control_tx,
                                stop_recording_after_write_error(&mut recording, error),
                            )?;
                        }
                    }
                    let sample_count = decoded.channels.first().map(Vec::len).unwrap_or(0);
                    next_sample_index = decoded.first_sample_index.checked_add(sample_count as u64);
                    stats.received_batches = stats.received_batches.saturating_add(1);
                    stats.received_samples =
                        stats.received_samples.saturating_add(sample_count as u64);
                    let worker = acquisition.as_mut().ok_or_else(|| {
                        SessionError::Acquisition(
                            "received samples before acquisition configuration completed"
                                .to_owned(),
                        )
                    })?;
                    let was_armed = worker.trigger.is_armed();
                    let (captures, snapshot) = worker.process(decoded)?;
                    for capture in captures {
                        send_control(
                            control_tx,
                            SessionEvent::TriggerCapture(capture, worker.trigger.config().clone()),
                        )?;
                    }
                    if worker.trigger.is_armed() != was_armed {
                        send_control(
                            control_tx,
                            SessionEvent::TriggerArmed(worker.trigger.is_armed()),
                        )?;
                    }
                    let publish_stats = snapshot.is_some();
                    if let Some(snapshot) = snapshot {
                        match data_tx.try_send(SessionEvent::DisplaySnapshot(snapshot)) {
                            Ok(()) | Err(TrySendError::Full(_)) => {}
                            Err(TrySendError::Disconnected(_)) => return Ok(()),
                        }
                    }
                    if publish_stats {
                        let _ = data_tx.try_send(SessionEvent::Stats(stats));
                    }
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

fn stop_recording_after_write_error(
    recording: &mut Option<RecordingIngress>,
    error: RecordingError,
) -> SessionEvent {
    *recording = None;
    SessionEvent::RecordingError(error.to_string())
}

fn worker_loop_v2(
    config: TransportConfig,
    command_rx: &Receiver<SessionCommand>,
    control_tx: &Sender<SessionEvent>,
    data_tx: &Sender<SessionEvent>,
    disconnect_requested: &AtomicBool,
) -> Result<(), SessionError> {
    let mut transport = config.connect()?;
    send_control(
        control_tx,
        SessionEvent::State(ConnectionState::Handshaking),
    )?;
    let mut out_sequence = 1_u32;
    write_common_v2(
        &mut transport,
        &mut out_sequence,
        0,
        Message::Hello(Hello {
            client_capabilities: 0b111 | CAPABILITY_V2_STREAMS,
            max_payload: MAX_PAYLOAD_LEN as u32,
            client_name: "ScopeAnalyzer V2".to_owned(),
        }),
    )?;

    let mut decoder = FrameDecoder::default();
    let mut read_buffer = [0_u8; 16 * 1024];
    let mut session_id = 0_u32;
    let mut hello_ack: Option<HelloAck> = None;
    let mut channels: Option<ChannelTable> = None;
    let mut streams: Option<StreamTable> = None;
    let mut pending = HashMap::new();
    let mut validator = SnapshotValidator::default();
    let mut capture: Option<CaptureAssembler> = None;
    let mut stats = SessionStats::default();
    let mut streaming = false;
    let mut last_received = Instant::now();
    let mut last_ping = Instant::now();
    let mut ping_nonce = 1_u64;
    let mut outstanding_ping: Option<u64> = None;
    let mut stream_timing: HashMap<u16, (u32, u64)> = HashMap::new();

    loop {
        while let Ok(command) = command_rx.try_recv() {
            match command {
                SessionCommand::ConfigureStream(configure) => {
                    let sequence = write_v2_message(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        MessageV2::ConfigureStream(configure.clone()),
                    )?;
                    pending.insert(sequence, PendingCommand::ConfigureStream(configure));
                    send_control(
                        control_tx,
                        SessionEvent::State(ConnectionState::Configuring),
                    )?;
                }
                SessionCommand::Start => {
                    let sequence = write_common_v2(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        Message::Start,
                    )?;
                    pending.insert(sequence, PendingCommand::Start);
                }
                SessionCommand::Stop => {
                    let sequence = write_common_v2(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        Message::Stop,
                    )?;
                    pending.insert(sequence, PendingCommand::Stop);
                }
                SessionCommand::Ping(nonce) => {
                    write_common_v2(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        Message::Ping(nonce),
                    )?;
                }
                SessionCommand::ArmCapture(value) => {
                    write_v2_message(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        MessageV2::ArmCapture(value),
                    )?;
                }
                SessionCommand::ManualTrigger(value) => {
                    write_v2_message(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        MessageV2::ManualTrigger(value),
                    )?;
                }
                SessionCommand::CancelCapture(value) => {
                    write_v2_message(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        MessageV2::CancelCapture(value),
                    )?;
                }
                SessionCommand::Configure(_, _)
                | SessionCommand::SetTriggerConfig(_)
                | SessionCommand::ArmTrigger
                | SessionCommand::SetRecording(_, _) => {
                    return Err(SessionError::Acquisition(
                        "SCP1 V1 command sent to a V2 session".to_owned(),
                    ));
                }
                SessionCommand::Disconnect => {
                    if streaming {
                        let _ = write_common_v2(
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
        if disconnect_requested.load(Ordering::Acquire) {
            if streaming {
                let _ =
                    write_common_v2(&mut transport, &mut out_sequence, session_id, Message::Stop);
            }
            return Ok(());
        }
        if session_id != 0 && last_received.elapsed() >= Duration::from_secs(3) {
            stats.heartbeat_timeout_count = stats.heartbeat_timeout_count.saturating_add(1);
            let _ = data_tx.try_send(SessionEvent::Stats(stats));
            return Err(SessionError::Acquisition(
                "no valid SCP1 V2 frame received for 3 seconds".to_owned(),
            ));
        }
        if session_id != 0 && last_ping.elapsed() >= Duration::from_secs(1) {
            write_common_v2(
                &mut transport,
                &mut out_sequence,
                session_id,
                Message::Ping(ping_nonce),
            )?;
            outstanding_ping = Some(ping_nonce);
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
        stats.crc_errors = decoder_stats.crc_errors;
        stats.malformed_headers = decoder_stats.malformed_headers;
        stats.discarded_bytes = decoder_stats.discarded_bytes;

        for frame in decoder.drain_frames() {
            if frame.version != PROTOCOL_VERSION_V2 {
                stats.protocol_errors = stats.protocol_errors.saturating_add(1);
                send_control(
                    control_tx,
                    SessionEvent::Error("SCP1 V2 session received a non-V2 frame".to_owned()),
                )?;
                continue;
            }
            if session_id != 0 && frame.session_id != session_id {
                stats.protocol_errors = stats.protocol_errors.saturating_add(1);
                continue;
            }
            last_received = Instant::now();
            stats.received_frames = stats.received_frames.saturating_add(1);

            match frame.message_type {
                super::protocol::MSG_HELLO_ACK => {
                    let Message::HelloAck(hello) =
                        Message::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    if frame.session_id == 0
                        || hello.device_capabilities & CAPABILITY_V2_STREAMS == 0
                    {
                        return Err(SessionError::Acquisition(
                            "peer did not negotiate SCP1 V2 streams".to_owned(),
                        ));
                    }
                    session_id = frame.session_id;
                    hello_ack = Some(hello.clone());
                    send_control(control_tx, SessionEvent::HelloAck(hello))?;
                }
                super::protocol::MSG_CHANNEL_TABLE => {
                    let Message::ChannelTable(table) =
                        Message::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    let hello = hello_ack.as_ref().ok_or_else(|| {
                        SessionError::Acquisition(
                            "CHANNEL_TABLE arrived before HELLO_ACK".to_owned(),
                        )
                    })?;
                    if usize::from(hello.channel_count) != table.channels.len() {
                        return Err(SessionError::Acquisition(
                            "V2 CHANNEL_TABLE count disagrees with HELLO_ACK".to_owned(),
                        ));
                    }
                    channels = Some(table.clone());
                    send_control(control_tx, SessionEvent::ChannelTable(table))?;
                }
                super::protocol::MSG_COMMAND_RESULT => {
                    let Message::CommandResult(result) =
                        Message::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    if result.result_code == ResultCode::Ok {
                        if let Some(command) = pending.remove(&result.request_sequence) {
                            match command {
                                PendingCommand::ConfigureStream(configure) => {
                                    send_control(
                                        control_tx,
                                        SessionEvent::ConfiguredV2(configure),
                                    )?;
                                    send_control(
                                        control_tx,
                                        SessionEvent::State(ConnectionState::Ready),
                                    )?;
                                }
                                PendingCommand::Start => {
                                    streaming = true;
                                    send_control(
                                        control_tx,
                                        SessionEvent::State(ConnectionState::Streaming),
                                    )?;
                                }
                                PendingCommand::Stop => {
                                    streaming = false;
                                    send_control(
                                        control_tx,
                                        SessionEvent::State(ConnectionState::Ready),
                                    )?;
                                }
                                PendingCommand::Configure(_, _) => {}
                            }
                        }
                    }
                    send_control(control_tx, SessionEvent::CommandResult(result))?;
                }
                MSG_STREAM_SAMPLE_BATCH => {
                    let table = channels.as_ref().ok_or_else(|| {
                        SessionError::Acquisition(
                            "V2 sample arrived before CHANNEL_TABLE".to_owned(),
                        )
                    })?;
                    let stream_table = streams.as_ref().ok_or_else(|| {
                        SessionError::Acquisition(
                            "V2 sample arrived before STREAM_TABLE".to_owned(),
                        )
                    })?;
                    let decoded = decode_stream_sample_frame(&frame, table, stream_table)?;
                    let descriptor = stream_table.stream(decoded.stream_id).ok_or_else(|| {
                        SessionError::Acquisition(
                            "decoded stream disappeared from STREAM_TABLE".to_owned(),
                        )
                    })?;
                    let hello = hello_ack.as_ref().ok_or_else(|| {
                        SessionError::Acquisition("V2 sample arrived before HELLO_ACK".to_owned())
                    })?;
                    super::protocol_v2::validate_stream_sample_period(
                        hello.tick_hz,
                        descriptor,
                        decoded.sample_period_ticks,
                    )?;
                    validate_v2_stream_timing(&mut stream_timing, &decoded)?;
                    for row in &decoded.row_metadata {
                        validator.observe_with_table(stream_table, descriptor, *row);
                    }
                    let diagnostics = validator.diagnostics().clone();
                    stats.row_sequence_gaps = diagnostics.row_sequence_gaps;
                    stats.row_sequence_reorders = diagnostics.row_sequence_reorders;
                    stats.source_sequence_faults = diagnostics.source_sequence_faults;
                    stats.applied_sequence_faults = diagnostics.applied_sequence_faults;
                    stats.invalid_snapshot_rows = diagnostics.invalid_snapshot_rows;
                    stats.missing_causal_source = diagnostics.missing_causal_source;
                    stats.causal_source_mismatch = diagnostics.causal_source_mismatch;
                    stats.causal_application_mismatch = diagnostics.causal_application_mismatch;
                    stats.causal_sequence_reorder = diagnostics.causal_sequence_reorder;
                    stats.causal_group_mismatch = diagnostics.causal_group_mismatch;
                    stats.received_batches = stats.received_batches.saturating_add(1);
                    stats.received_samples = stats
                        .received_samples
                        .saturating_add(u64::from(decoded.row_metadata.len() as u32));
                    let dropped_stream_id = decoded.stream_id;
                    let dropped_first = decoded.first_row_sequence;
                    let dropped_last = decoded
                        .row_metadata
                        .last()
                        .map(|row| row.row_sequence)
                        .unwrap_or(dropped_first);
                    let dropped_rows = decoded.row_metadata.len() as u64;
                    let snapshot_dropped =
                        match data_tx.try_send(SessionEvent::SnapshotV2(decoded, diagnostics)) {
                            Ok(()) => false,
                            Err(TrySendError::Full(_)) => {
                                stats.host_dropped_v2_batches =
                                    stats.host_dropped_v2_batches.saturating_add(1);
                                stats.host_dropped_v2_rows =
                                    stats.host_dropped_v2_rows.saturating_add(dropped_rows);
                                stats.v2_snapshot_queue_overruns =
                                    stats.v2_snapshot_queue_overruns.saturating_add(1);
                                stats.last_dropped_v2_stream_id = Some(dropped_stream_id);
                                stats.last_dropped_v2_first_row = Some(dropped_first);
                                stats.last_dropped_v2_last_row = Some(dropped_last);
                                true
                            }
                            Err(TrySendError::Disconnected(_)) => return Ok(()),
                        };
                    if snapshot_dropped {
                        // A host-side drop must remain observable even while the data queue is
                        // full. Publish it on the bounded control path so sustained consumer
                        // backpressure fails explicitly instead of silently hiding loss.
                        send_control(control_tx, SessionEvent::Stats(stats))?;
                    }
                }
                super::protocol::MSG_PING => {
                    let Message::Ping(nonce) = Message::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    write_common_v2(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        Message::Pong(nonce),
                    )?;
                }
                MSG_CAPTURE_BEGIN => {
                    let MessageV2::CaptureBegin(begin) =
                        MessageV2::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    let stream_table = streams.as_ref().ok_or_else(|| {
                        SessionError::Acquisition(
                            "CAPTURE_BEGIN arrived before STREAM_TABLE".to_owned(),
                        )
                    })?;
                    let descriptor = stream_table.stream(begin.stream_id).ok_or_else(|| {
                        SessionError::Acquisition(
                            "CAPTURE_BEGIN references unknown stream".to_owned(),
                        )
                    })?;
                    let mut assembler = CaptureAssembler::default();
                    assembler
                        .begin_with_descriptor(begin, descriptor, stream_table.revision)
                        .map_err(SessionError::Acquisition)?;
                    capture = Some(assembler);
                }
                MSG_CAPTURE_DATA => {
                    let MessageV2::CaptureData(data) =
                        MessageV2::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    let table = channels.as_ref().ok_or_else(|| {
                        SessionError::Acquisition(
                            "CAPTURE_DATA arrived before CHANNEL_TABLE".to_owned(),
                        )
                    })?;
                    let stream_table = streams.as_ref().ok_or_else(|| {
                        SessionError::Acquisition(
                            "CAPTURE_DATA arrived before STREAM_TABLE".to_owned(),
                        )
                    })?;
                    let nested = MessageV2::StreamSampleBatch(data.batch.clone()).into_frame(
                        0,
                        frame.sequence,
                        session_id,
                        frame.timestamp_ticks,
                    )?;
                    let _ = decode_stream_sample_frame(&nested, table, stream_table)?;
                    let descriptor =
                        stream_table.stream(data.batch.stream_id).ok_or_else(|| {
                            SessionError::Acquisition(
                                "CAPTURE_DATA references unknown stream".to_owned(),
                            )
                        })?;
                    let hello = hello_ack.as_ref().ok_or_else(|| {
                        SessionError::Acquisition(
                            "CAPTURE_DATA arrived before HELLO_ACK".to_owned(),
                        )
                    })?;
                    super::protocol_v2::validate_stream_sample_period(
                        hello.tick_hz,
                        descriptor,
                        data.batch.sample_period_ticks,
                    )?;
                    capture
                        .as_mut()
                        .ok_or_else(|| {
                            SessionError::Acquisition(
                                "CAPTURE_DATA arrived before CAPTURE_BEGIN".to_owned(),
                            )
                        })?
                        .push(data)
                        .map_err(SessionError::Acquisition)?;
                }
                MSG_CAPTURE_END => {
                    let MessageV2::CaptureEnd(end) =
                        MessageV2::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    let mut assembler = capture.take().ok_or_else(|| {
                        SessionError::Acquisition(
                            "CAPTURE_END arrived before CAPTURE_BEGIN".to_owned(),
                        )
                    })?;
                    match assembler.finish(end) {
                        Ok(assembled) => {
                            send_control(control_tx, SessionEvent::CaptureComplete(assembled))?
                        }
                        Err(error) => {
                            stats.capture_processing_overruns =
                                stats.capture_processing_overruns.saturating_add(1);
                            let _ = data_tx.try_send(SessionEvent::Stats(stats));
                            send_control(control_tx, SessionEvent::CaptureFailure(error.clone()))?;
                            return Err(SessionError::Acquisition(error));
                        }
                    }
                }
                super::protocol_v2::MSG_CAPTURE_STATUS => {
                    let MessageV2::CaptureStatus(status) =
                        MessageV2::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    send_control(control_tx, SessionEvent::CaptureStatus(status))?;
                }
                super::protocol_v2::MSG_STREAM_TABLE => {
                    let MessageV2::StreamTable(table) =
                        MessageV2::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    let channel_table = channels.as_ref().ok_or_else(|| {
                        SessionError::Acquisition(
                            "STREAM_TABLE arrived before CHANNEL_TABLE".to_owned(),
                        )
                    })?;
                    table.validate_against_channels(channel_table)?;
                    streams = Some(table.clone());
                    send_control(control_tx, SessionEvent::StreamTable(table))?;
                    send_control(control_tx, SessionEvent::State(ConnectionState::Ready))?;
                }
                super::protocol::MSG_STATUS => {
                    let Message::Status(status) =
                        Message::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    stats.device_dropped_samples = status.dropped_samples;
                    stats.device_tx_overruns = u64::from(status.tx_overruns);
                    stats.device_state = Some(status.state);
                    let _ = data_tx.try_send(SessionEvent::Stats(stats));
                }
                super::protocol::MSG_PONG => {
                    let Message::Pong(nonce) = Message::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    stats.last_pong_nonce = Some(nonce);
                    if outstanding_ping == Some(nonce) {
                        stats.heartbeat_round_trip_count =
                            stats.heartbeat_round_trip_count.saturating_add(1);
                        outstanding_ping = None;
                    } else {
                        stats.heartbeat_timeout_count =
                            stats.heartbeat_timeout_count.saturating_add(1);
                        stats.protocol_errors = stats.protocol_errors.saturating_add(1);
                        send_control(
                            control_tx,
                            SessionEvent::Error(format!("unexpected SCP1 V2 PONG nonce {nonce}")),
                        )?;
                    }
                    let _ = data_tx.try_send(SessionEvent::Stats(stats));
                }
                super::protocol::MSG_ERROR => {
                    let Message::Error(error) =
                        Message::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    send_control(control_tx, SessionEvent::Error(error.detail))?;
                }
                _ => {
                    stats.unknown_messages = stats.unknown_messages.saturating_add(1);
                }
            }
        }
    }
}

fn validate_v2_stream_timing(
    timing: &mut HashMap<u16, (u32, u64)>,
    batch: &DecodedStreamSampleBatch,
) -> Result<(), SessionError> {
    let row_count = u64::try_from(batch.row_metadata.len()).map_err(|_| {
        SessionError::Acquisition("V2 stream row count does not fit u64".to_owned())
    })?;
    let last_offset = u64::from(batch.sample_period_ticks)
        .checked_mul(row_count.saturating_sub(1))
        .ok_or_else(|| SessionError::Acquisition("V2 sample timestamp overflow".to_owned()))?;
    let last_timestamp = batch
        .timestamp_ticks
        .checked_add(last_offset)
        .ok_or_else(|| SessionError::Acquisition("V2 sample timestamp overflow".to_owned()))?;
    if let Some((period, previous_last_timestamp)) = timing.get(&batch.stream_id) {
        if *period != batch.sample_period_ticks {
            return Err(SessionError::Acquisition(format!(
                "V2 stream {} changed sample_period_ticks during the session",
                batch.stream_id
            )));
        }
        if batch.timestamp_ticks <= *previous_last_timestamp {
            return Err(SessionError::Acquisition(format!(
                "V2 stream {} timestamp is not monotonic with row sequence",
                batch.stream_id
            )));
        }
    }
    timing.insert(batch.stream_id, (batch.sample_period_ticks, last_timestamp));
    Ok(())
}

fn write_common_v2(
    transport: &mut TransportStream,
    next_sequence: &mut u32,
    session_id: u32,
    message: Message,
) -> Result<u32, SessionError> {
    let sequence = *next_sequence;
    let frame = Frame::new_v2(
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

fn write_v2_message(
    transport: &mut TransportStream,
    next_sequence: &mut u32,
    session_id: u32,
    message: MessageV2,
) -> Result<u32, SessionError> {
    let sequence = *next_sequence;
    let frame = message.into_frame(0, sequence, session_id, 0)?;
    transport.write_all(&frame.encode()?)?;
    transport.flush()?;
    *next_sequence = next_sequence.wrapping_add(1);
    Ok(sequence)
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
        protocol_v2::{
            ArmCapture, CaptureEdge, CapturePhase, CaptureState, CaptureTrigger,
            CaptureTriggerKind, ConfigureStream, ManualTrigger, SampleDomain,
        },
        simulator::{SimulatorConfig, SimulatorHandle, SimulatorProtocol, V2Preset},
        snapshot::SnapshotMeta,
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

    fn v2_timing_batch(
        stream_id: u16,
        sample_period_ticks: u32,
        timestamp_ticks: u64,
        rows: u64,
    ) -> DecodedStreamSampleBatch {
        DecodedStreamSampleBatch {
            stream_id,
            revision: 1,
            domain: SampleDomain::Fast32k,
            capture_phase: CapturePhase::AfterClaComplete,
            consistency_group: 1,
            first_row_sequence: 1,
            sample_period_ticks,
            timestamp_ticks,
            channel_ids: vec![0],
            channels: vec![vec![0.0; rows as usize]],
            row_metadata: (0..rows)
                .map(|row_sequence| SnapshotMeta {
                    row_sequence,
                    ..SnapshotMeta::default()
                })
                .collect(),
            raw_frame: Vec::new(),
        }
    }

    #[test]
    fn v2_stream_timing_requires_fixed_period_monotonicity_and_no_overflow() {
        let mut timing = HashMap::new();
        validate_v2_stream_timing(&mut timing, &v2_timing_batch(1, 1_000, 10_000, 4)).unwrap();
        validate_v2_stream_timing(&mut timing, &v2_timing_batch(1, 1_000, 14_000, 2)).unwrap();

        let changed_period =
            validate_v2_stream_timing(&mut timing, &v2_timing_batch(1, 4_000, 16_000, 1))
                .unwrap_err();
        assert!(changed_period
            .to_string()
            .contains("changed sample_period_ticks"));

        let non_monotonic =
            validate_v2_stream_timing(&mut timing, &v2_timing_batch(1, 1_000, 14_000, 1))
                .unwrap_err();
        assert!(non_monotonic.to_string().contains("not monotonic"));

        let overflow = validate_v2_stream_timing(
            &mut HashMap::new(),
            &v2_timing_batch(2, 1_000, u64::MAX - 500, 2),
        )
        .unwrap_err();
        assert!(overflow.to_string().contains("timestamp overflow"));
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
        let snapshot = wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::DisplaySnapshot(_))
        });
        let SessionEvent::DisplaySnapshot(snapshot) = snapshot else {
            unreachable!();
        };
        assert_eq!(snapshot.channel_ids, vec![0, 1, 2, 3]);
        assert!(snapshot
            .segments
            .iter()
            .all(|segment| segment.channels.len() == 4));
        assert!(
            snapshot
                .segments
                .iter()
                .map(|segment| segment.times.len())
                .sum::<usize>()
                >= 10
        );

        session.stop().unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Ready))
        });
        session.disconnect().unwrap();
    }

    #[test]
    fn v2_simulator_handshake_validates_a_frozen_fast_stream() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
            protocol: SimulatorProtocol::V2,
            preset: Some(V2Preset::Normal30k),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect_v2(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::StreamTable(_))
        });
        session
            .configure_stream(ConfigureStream {
                stream_id: 1,
                batch_samples: 4,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::ConfiguredV2(_))
        });
        session.start().unwrap();
        let snapshot = wait_for(
            &session,
            Duration::from_secs(2),
            |event| matches!(event, SessionEvent::SnapshotV2(_, diagnostics) if diagnostics.invalid_snapshot_rows == 0),
        );
        assert!(
            matches!(snapshot, SessionEvent::SnapshotV2(batch, diagnostics)
            if batch.domain == crate::live::protocol_v2::SampleDomain::Fast32k
                && batch.row_metadata.len() == 4
                && diagnostics.applied_sequence_faults == 0)
        );
        session.disconnect().unwrap();
    }

    #[test]
    fn v2_manual_capture_is_assembled_in_memory() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
            protocol: SimulatorProtocol::V2,
            preset: Some(V2Preset::CaptureManual30k),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect_v2(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::StreamTable(_))
        });
        session
            .configure_stream(ConfigureStream {
                stream_id: 1,
                batch_samples: 2,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::ConfiguredV2(_))
        });
        session
            .arm_capture(ArmCapture {
                capture_id: 77,
                stream_id: 1,
                pretrigger_rows: 1,
                posttrigger_rows: 1,
                timeout_samples: 100,
                trigger: CaptureTrigger {
                    kind: CaptureTriggerKind::Manual,
                    channel_id: 0,
                    level: 0.0,
                    edge: CaptureEdge::Rising,
                    hysteresis: 0.0,
                    flag_mask: 0,
                    flag_value: 0,
                },
            })
            .unwrap();
        session
            .manual_trigger(ManualTrigger { capture_id: 77 })
            .unwrap();
        let capture = wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::CaptureComplete(_))
        });
        assert!(
            matches!(capture, SessionEvent::CaptureComplete(capture) if capture.diagnostics.capture_complete)
        );
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

        let snapshot = wait_for(
            &session,
            Duration::from_secs(2),
            |event| matches!(event, SessionEvent::DisplaySnapshot(snapshot) if snapshot.segments.len() >= 2),
        );
        let SessionEvent::DisplaySnapshot(snapshot) = snapshot else {
            unreachable!();
        };
        assert!(snapshot.segments.len() >= 2);
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

    #[test]
    fn recording_queue_full_is_reported_as_a_fatal_recording_error() {
        let mut recording = None;

        let event = stop_recording_after_write_error(&mut recording, RecordingError::QueueFull);

        assert!(recording.is_none());
        assert!(matches!(
            event,
            SessionEvent::RecordingError(message) if message.contains("queue is full")
        ));
    }

    #[test]
    fn v2_exchanges_heartbeats_and_applies_status_statistics() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            protocol: SimulatorProtocol::V2,
            preset: Some(V2Preset::Normal30k),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect_v2(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::StreamTable(_))
        });
        let status = wait_for(
            &session,
            Duration::from_secs(2),
            |event| matches!(event, SessionEvent::Stats(stats) if stats.device_state == Some(crate::live::protocol::DeviceState::Idle)),
        );
        assert!(
            matches!(status, SessionEvent::Stats(stats) if stats.device_dropped_samples == 0 && stats.device_tx_overruns == 0)
        );

        let heartbeat = wait_for(
            &session,
            Duration::from_secs(3),
            |event| matches!(event, SessionEvent::Stats(stats) if stats.heartbeat_round_trip_count > 0),
        );
        assert!(matches!(heartbeat, SessionEvent::Stats(stats) if stats.last_pong_nonce.is_some()));
        assert!(simulator.stats().pings_sent > 0);
        assert!(simulator.stats().pongs_received > 0);
        session.disconnect().unwrap();
    }

    #[test]
    fn v2_snapshot_backpressure_is_accounted_without_protocol_gap_claims() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
            protocol: SimulatorProtocol::V2,
            preset: Some(V2Preset::Normal30k),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect_v2(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::StreamTable(_))
        });
        session
            .configure_stream(ConfigureStream {
                stream_id: 1,
                batch_samples: 1,
                channel_mask: 0b1111,
            })
            .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::ConfiguredV2(_))
        });
        session.start().unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::State(ConnectionState::Streaming))
        });
        std::thread::sleep(Duration::from_millis(150));
        let stats = wait_for(
            &session,
            Duration::from_secs(2),
            |event| matches!(event, SessionEvent::Stats(stats) if stats.host_dropped_v2_batches > 0),
        );
        assert!(matches!(
            stats,
            SessionEvent::Stats(stats)
                if stats.host_dropped_v2_rows > 0
                    && stats.v2_snapshot_queue_overruns > 0
                    && stats.last_dropped_v2_stream_id == Some(1)
                    && stats.row_sequence_gaps == 0
        ));
        session.disconnect().unwrap();
    }

    #[test]
    fn all_30k_presets_complete_the_v2_session_acceptance_matrix() {
        #[derive(Clone, Copy)]
        enum Expected {
            CleanSnapshot,
            DiagnosticSnapshot,
            RejectedStream,
            CaptureComplete,
            CaptureFailure,
            CaptureStatus(CaptureState),
        }
        let matrix = [
            (V2Preset::Normal30k, Expected::CleanSnapshot),
            (V2Preset::CausalDelay30k, Expected::CleanSnapshot),
            (V2Preset::ClaStale30k, Expected::DiagnosticSnapshot),
            (V2Preset::RowGap30k, Expected::DiagnosticSnapshot),
            (V2Preset::RowReorder30k, Expected::DiagnosticSnapshot),
            (V2Preset::PhaseMismatch30k, Expected::RejectedStream),
            (V2Preset::GroupMismatch30k, Expected::RejectedStream),
            (V2Preset::UnfrozenRow30k, Expected::DiagnosticSnapshot),
            (V2Preset::CaptureManual30k, Expected::CaptureComplete),
            (V2Preset::CaptureEdge30k, Expected::CaptureComplete),
            (V2Preset::CaptureFault30k, Expected::CaptureComplete),
            (
                V2Preset::CaptureTimeout30k,
                Expected::CaptureStatus(CaptureState::Timeout),
            ),
            (V2Preset::CaptureChunkLoss30k, Expected::CaptureFailure),
            (V2Preset::CaptureChunkReorder30k, Expected::CaptureComplete),
            (
                V2Preset::DeviceReset30k,
                Expected::CaptureStatus(CaptureState::DeviceReset),
            ),
        ];

        for (preset, expected) in matrix {
            let simulator = SimulatorHandle::spawn(SimulatorConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                accelerated: true,
                protocol: SimulatorProtocol::V2,
                preset: Some(preset),
                ..SimulatorConfig::default()
            })
            .unwrap();
            let session = LiveSession::connect_v2(TransportConfig::Tcp {
                address: simulator.address().to_string(),
            })
            .unwrap();
            wait_for(&session, Duration::from_secs(2), |event| {
                matches!(event, SessionEvent::StreamTable(_))
            });
            session
                .configure_stream(ConfigureStream {
                    stream_id: 1,
                    batch_samples: 2,
                    channel_mask: 0b1111,
                })
                .unwrap();
            wait_for(&session, Duration::from_secs(2), |event| {
                matches!(event, SessionEvent::ConfiguredV2(_))
            });

            match expected {
                Expected::CleanSnapshot
                | Expected::DiagnosticSnapshot
                | Expected::RejectedStream => {
                    session.start().unwrap();
                    let event =
                        wait_for(&session, Duration::from_secs(2), |event| match expected {
                            Expected::CleanSnapshot => matches!(
                                event,
                                SessionEvent::SnapshotV2(_, diagnostics)
                                    if diagnostics.invalid_snapshot_rows == 0
                            ),
                            Expected::DiagnosticSnapshot => matches!(
                                event,
                                SessionEvent::SnapshotV2(_, diagnostics)
                                    if diagnostics.invalid_snapshot_rows > 0
                                        || diagnostics.row_sequence_gaps > 0
                                        || diagnostics.row_sequence_reorders > 0
                            ),
                            Expected::RejectedStream => matches!(event, SessionEvent::Error(_)),
                            _ => false,
                        });
                    match expected {
                        Expected::CleanSnapshot => assert!(matches!(
                            event,
                            SessionEvent::SnapshotV2(_, diagnostics)
                                if diagnostics.invalid_snapshot_rows == 0
                        )),
                        Expected::DiagnosticSnapshot => assert!(matches!(
                            event,
                            SessionEvent::SnapshotV2(_, diagnostics)
                                if diagnostics.invalid_snapshot_rows > 0
                                    || diagnostics.row_sequence_gaps > 0
                                    || diagnostics.row_sequence_reorders > 0
                        )),
                        Expected::RejectedStream => {
                            assert!(matches!(event, SessionEvent::Error(_)))
                        }
                        _ => unreachable!(),
                    }
                }
                Expected::CaptureComplete
                | Expected::CaptureFailure
                | Expected::CaptureStatus(_) => {
                    let capture_id = 700 + preset as u32;
                    session
                        .arm_capture(ArmCapture {
                            capture_id,
                            stream_id: 1,
                            pretrigger_rows: 1,
                            posttrigger_rows: 1,
                            timeout_samples: 100,
                            trigger: CaptureTrigger {
                                kind: CaptureTriggerKind::Manual,
                                channel_id: 0,
                                level: 0.0,
                                edge: CaptureEdge::Rising,
                                hysteresis: 0.0,
                                flag_mask: 0,
                                flag_value: 0,
                            },
                        })
                        .unwrap();
                    if matches!(
                        preset,
                        V2Preset::CaptureManual30k
                            | V2Preset::CaptureChunkLoss30k
                            | V2Preset::CaptureChunkReorder30k
                    ) {
                        session
                            .manual_trigger(ManualTrigger { capture_id })
                            .unwrap();
                    }
                    let event = wait_for(&session, Duration::from_secs(2), |event| {
                        matches!(
                            event,
                            SessionEvent::CaptureComplete(_)
                                | SessionEvent::CaptureFailure(_)
                                | SessionEvent::CaptureStatus(_)
                        )
                    });
                    match expected {
                        Expected::CaptureComplete => {
                            // The first status is normally Armed; keep waiting for completion.
                            let event = if matches!(event, SessionEvent::CaptureComplete(_)) {
                                event
                            } else {
                                wait_for(&session, Duration::from_secs(2), |event| {
                                    matches!(event, SessionEvent::CaptureComplete(_))
                                })
                            };
                            assert!(matches!(event, SessionEvent::CaptureComplete(_)));
                        }
                        Expected::CaptureFailure => {
                            let event = if matches!(event, SessionEvent::CaptureFailure(_)) {
                                event
                            } else {
                                wait_for(&session, Duration::from_secs(2), |event| {
                                    matches!(event, SessionEvent::CaptureFailure(_))
                                })
                            };
                            assert!(matches!(event, SessionEvent::CaptureFailure(_)));
                        }
                        Expected::CaptureStatus(expected_state) => {
                            let event = if matches!(event, SessionEvent::CaptureStatus(status) if status.state == expected_state)
                            {
                                event
                            } else {
                                wait_for(
                                    &session,
                                    Duration::from_secs(2),
                                    |event| matches!(event, SessionEvent::CaptureStatus(status) if status.state == expected_state),
                                )
                            };
                            assert!(
                                matches!(event, SessionEvent::CaptureStatus(status) if status.state == expected_state)
                            );
                        }
                        _ => unreachable!(),
                    }
                }
            }
            let _ = session.disconnect();
        }
    }
}
