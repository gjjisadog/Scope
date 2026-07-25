use std::{
    collections::{HashMap, HashSet, VecDeque},
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
    hardware_capture::{
        AssembledCapture, AssembledCaptureR2, CaptureAssembler, CaptureAssemblerR2,
    },
    protocol::{
        decode_configure_result_detail, decode_sample_frame, validate_configure_for_device,
        ChannelTable, CommandResult, Configure, DecodedSampleBatch, Frame, FrameDecoder, Hello,
        HelloAck, Message, ProtocolError, ResultCode, MAX_PAYLOAD_LEN, PROTOCOL_VERSION_V2,
    },
    protocol_v2::{
        decode_stream_sample_frame, ArmCapture, CancelCapture, CaptureState, CaptureStatus,
        ConfigureStream, DecodedStreamSampleBatch, ManualTrigger, MessageV2, StreamTable,
        CAPABILITY_V2_STREAMS_R1, MSG_CAPTURE_BEGIN, MSG_CAPTURE_DATA_R1, MSG_CAPTURE_END,
        MSG_SAMPLE_BATCH_V2_R1, MSG_STREAM_TABLE_R1,
    },
    protocol_v2_r2::{
        decode_stream_sample_frame_r2, ConfigureStreamsR2, DecodedStreamSampleBatchR2, MessageV2R2,
        StreamTableR2, CAPABILITY_V2_COMPRESSED_METADATA, CAPABILITY_V2_HARDWARE_CAPTURE_R2,
        CAPABILITY_V2_MULTI_STREAM, CAPABILITY_V2_STREAMS_R2, MSG_CAPTURE_DATA_R2,
        MSG_SAMPLE_BATCH_V2_R2, MSG_STREAM_TABLE_R2,
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
const HEARTBEAT_NONCE_WINDOW: usize = 8;
const HEARTBEAT_REPLY_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
struct HeartbeatWindow {
    sent_at: HashMap<u64, Instant>,
    acknowledged: HashSet<u64>,
    acknowledged_order: VecDeque<u64>,
    capacity: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeartbeatAck {
    Matched(Duration),
    Duplicate,
    Unknown,
}

impl HeartbeatWindow {
    fn new(capacity: usize) -> Self {
        Self {
            sent_at: HashMap::new(),
            acknowledged: HashSet::new(),
            acknowledged_order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    fn record(&mut self, nonce: u64, now: Instant) -> bool {
        self.acknowledged.remove(&nonce);
        self.acknowledged_order.retain(|value| *value != nonce);
        self.sent_at.insert(nonce, now);
        if self.sent_at.len() <= self.capacity {
            return false;
        }
        let oldest = self
            .sent_at
            .iter()
            .min_by_key(|(_, sent_at)| *sent_at)
            .map(|(nonce, _)| *nonce);
        oldest.is_some_and(|nonce| self.sent_at.remove(&nonce).is_some())
    }

    fn acknowledge(&mut self, nonce: u64, now: Instant) -> HeartbeatAck {
        if let Some(sent_at) = self.sent_at.remove(&nonce) {
            self.acknowledged.insert(nonce);
            self.acknowledged_order.push_back(nonce);
            while self.acknowledged_order.len() > self.capacity {
                if let Some(oldest) = self.acknowledged_order.pop_front() {
                    self.acknowledged.remove(&oldest);
                }
            }
            HeartbeatAck::Matched(now.saturating_duration_since(sent_at))
        } else if self.acknowledged.contains(&nonce) {
            HeartbeatAck::Duplicate
        } else {
            HeartbeatAck::Unknown
        }
    }

    fn expire(&mut self, now: Instant) -> u64 {
        let expired = self
            .sent_at
            .iter()
            .filter_map(|(nonce, sent_at)| {
                (now.saturating_duration_since(*sent_at) >= HEARTBEAT_REPLY_TIMEOUT)
                    .then_some(*nonce)
            })
            .collect::<Vec<_>>();
        for nonce in &expired {
            self.sent_at.remove(nonce);
        }
        expired.len() as u64
    }

    fn pending_count(&self) -> usize {
        self.sent_at.len()
    }

    fn clear(&mut self) {
        self.sent_at.clear();
        self.acknowledged.clear();
        self.acknowledged_order.clear();
    }
}

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
    DeviceResetHandshake,
    Configuring,
    Ready,
    Streaming,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LiveProtocol {
    #[default]
    V1,
    V2R1,
    V2R2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamSessionStats {
    pub stream_id: u16,
    pub received_batches: u64,
    pub received_rows: u64,
    pub host_dropped_batches: u64,
    pub host_dropped_rows: u64,
    pub row_sequence_gaps: u64,
    pub row_sequence_reorders: u64,
    pub logical_cycle_faults: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionStats {
    pub session_id: u32,
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
    pub causal_cached_rows: usize,
    pub causal_pending_matches: usize,
    pub causal_match_timeouts: u64,
    pub causal_cache_evictions: u64,
    pub causal_window_overflows: u64,
    pub causal_duplicate_cycles: u64,
    pub host_dropped_v2_batches: u64,
    pub host_dropped_v2_rows: u64,
    pub v2_snapshot_queue_overruns: u64,
    pub capture_processing_overruns: u64,
    pub last_dropped_v2_stream_id: Option<u16>,
    pub last_dropped_v2_first_row: Option<u64>,
    pub last_dropped_v2_last_row: Option<u64>,
    pub last_pong_nonce: Option<u64>,
    pub heartbeat_pending_count: usize,
    pub heartbeat_round_trip_count: u64,
    pub heartbeat_timeout_count: u64,
    pub heartbeat_unexpected_pong_count: u64,
    pub heartbeat_duplicate_pong_count: u64,
    pub heartbeat_window_overflow_count: u64,
    pub heartbeat_last_rtt_ms: u64,
    pub heartbeat_max_rtt_ms: u64,
    pub device_reset_count: u64,
    pub device_state: Option<super::protocol::DeviceState>,
    pub stream_stats: [StreamSessionStats; 8],
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // Preserve the frozen source-compatible event payload API.
pub enum SessionEvent {
    State(ConnectionState),
    HelloAck(HelloAck),
    ChannelTable(ChannelTable),
    StreamTable(StreamTable),
    StreamTableR2(StreamTableR2),
    Configured(Configure),
    ConfiguredV2(ConfigureStream),
    ConfiguredV2R2(ConfigureStreamsR2),
    CommandResult(CommandResult),
    /// Retained for source compatibility; acquisition workers no longer emit
    /// a batch to the UI event queue.
    Batch(DecodedSampleBatch),
    SnapshotV2(DecodedStreamSampleBatch, SnapshotDiagnostics),
    SnapshotV2R2(DecodedStreamSampleBatchR2, SnapshotDiagnostics),
    CaptureStatus(CaptureStatus),
    CaptureComplete(AssembledCapture),
    CaptureCompleteR2(AssembledCaptureR2),
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
    ConfigureStreamsR2(ConfigureStreamsR2),
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
    ConfigureStreamsR2(ConfigureStreamsR2),
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

    /// Compatibility alias for the frozen R2 entry point. The desktop GUI
    /// keeps calling `connect()` and therefore remains on SCP1 V1 by default.
    pub fn connect_v2(config: TransportConfig) -> Result<Self, SessionError> {
        Self::connect_v2_r2(config)
    }

    pub fn connect_v2_r1(config: TransportConfig) -> Result<Self, SessionError> {
        Self::connect_with_protocol(config, LiveProtocol::V2R1)
    }

    pub fn connect_v2_r2(config: TransportConfig) -> Result<Self, SessionError> {
        Self::connect_with_protocol(config, LiveProtocol::V2R2)
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

    pub fn configure_streams_r2(&self, configure: ConfigureStreamsR2) -> Result<(), SessionError> {
        self.send(SessionCommand::ConfigureStreamsR2(configure))
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
        LiveProtocol::V2R1 => worker_loop_v2_r1(
            config,
            &command_rx,
            &control_tx,
            &data_tx,
            &disconnect_requested,
        ),
        LiveProtocol::V2R2 => worker_loop_v2_r2(
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
                | SessionCommand::ConfigureStreamsR2(_)
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
                                PendingCommand::ConfigureStream(_)
                                | PendingCommand::ConfigureStreamsR2(_) => ConnectionState::Ready,
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

fn worker_loop_v2_r1(
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
            client_capabilities: 0b111 | CAPABILITY_V2_STREAMS_R1,
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
    let mut heartbeat_window = HeartbeatWindow::new(HEARTBEAT_NONCE_WINDOW);
    let mut stream_timing: HashMap<u16, StreamTiming> = HashMap::new();

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
                SessionCommand::ConfigureStreamsR2(_) => {
                    return Err(SessionError::Acquisition(
                        "SCP1 V2 R2 command sent to an R1 session".to_owned(),
                    ));
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
                    if heartbeat_window.record(nonce, Instant::now()) {
                        stats.heartbeat_window_overflow_count =
                            stats.heartbeat_window_overflow_count.saturating_add(1);
                    }
                    stats.heartbeat_pending_count = heartbeat_window.pending_count();
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
        let expired_heartbeats = heartbeat_window.expire(Instant::now());
        if expired_heartbeats != 0 {
            stats.heartbeat_timeout_count = stats
                .heartbeat_timeout_count
                .saturating_add(expired_heartbeats);
            stats.heartbeat_pending_count = heartbeat_window.pending_count();
            let _ = data_tx.try_send(SessionEvent::Stats(stats));
        }
        if session_id != 0 && last_ping.elapsed() >= Duration::from_secs(1) {
            write_common_v2(
                &mut transport,
                &mut out_sequence,
                session_id,
                Message::Ping(ping_nonce),
            )?;
            if heartbeat_window.record(ping_nonce, Instant::now()) {
                stats.heartbeat_window_overflow_count =
                    stats.heartbeat_window_overflow_count.saturating_add(1);
            }
            stats.heartbeat_pending_count = heartbeat_window.pending_count();
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
                        || hello.device_capabilities & CAPABILITY_V2_STREAMS_R1 == 0
                    {
                        return Err(SessionError::Acquisition(
                            "peer did not negotiate SCP1 V2 streams".to_owned(),
                        ));
                    }
                    session_id = frame.session_id;
                    stats.session_id = session_id;
                    hello_ack = Some(hello.clone());
                    send_control(control_tx, SessionEvent::HelloAck(hello))?;
                    send_control(control_tx, SessionEvent::Stats(stats))?;
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
                                PendingCommand::Configure(_, _)
                                | PendingCommand::ConfigureStreamsR2(_) => {}
                            }
                        }
                    }
                    send_control(control_tx, SessionEvent::CommandResult(result))?;
                }
                MSG_SAMPLE_BATCH_V2_R1 => {
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
                        validator.observe_r1(descriptor, *row);
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
                    stats.causal_cached_rows = diagnostics.causal_cached_rows;
                    stats.causal_pending_matches = diagnostics.causal_pending_matches;
                    stats.causal_match_timeouts = diagnostics.causal_match_timeouts;
                    stats.causal_cache_evictions = diagnostics.causal_cache_evictions;
                    stats.causal_window_overflows = diagnostics.causal_window_overflows;
                    stats.causal_duplicate_cycles = diagnostics.causal_duplicate_cycles;
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
                    let result: Result<CaptureAssembler, SessionError> = (|| {
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
                        Ok(assembler)
                    })();
                    match result {
                        Ok(assembler) => capture = Some(assembler),
                        Err(error) => {
                            capture = None;
                            report_capture_failure(
                                control_tx,
                                data_tx,
                                &mut stats,
                                error.to_string(),
                            )?;
                        }
                    }
                }
                MSG_CAPTURE_DATA_R1 => {
                    let result: Result<(), SessionError> =
                        (|| {
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
                            let nested = MessageV2::StreamSampleBatch(data.batch.clone())
                                .into_frame(0, frame.sequence, session_id, frame.timestamp_ticks)?;
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
                                .map_err(SessionError::Acquisition)
                        })();
                    if let Err(error) = result {
                        capture = None;
                        report_capture_failure(control_tx, data_tx, &mut stats, error.to_string())?;
                    }
                }
                MSG_CAPTURE_END => {
                    let result: Result<AssembledCapture, SessionError> = (|| {
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
                        assembler.finish(end).map_err(SessionError::Acquisition)
                    })();
                    capture = None;
                    match result {
                        Ok(assembled) => {
                            send_control(control_tx, SessionEvent::CaptureComplete(assembled))?
                        }
                        Err(error) => {
                            report_capture_failure(
                                control_tx,
                                data_tx,
                                &mut stats,
                                error.to_string(),
                            )?;
                        }
                    }
                }
                super::protocol_v2::MSG_CAPTURE_STATUS => {
                    let MessageV2::CaptureStatus(status) =
                        MessageV2::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    if matches!(
                        status.state,
                        CaptureState::Idle
                            | CaptureState::Cancelled
                            | CaptureState::Timeout
                            | CaptureState::BufferOverrun
                            | CaptureState::InvalidConfig
                            | CaptureState::DeviceReset
                    ) {
                        capture = None;
                    }
                    send_control(control_tx, SessionEvent::CaptureStatus(status))?;
                }
                MSG_STREAM_TABLE_R1 => {
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
                    match heartbeat_window.acknowledge(nonce, Instant::now()) {
                        HeartbeatAck::Matched(rtt) => {
                            let rtt_ms = u64::try_from(rtt.as_millis()).unwrap_or(u64::MAX);
                            stats.heartbeat_round_trip_count =
                                stats.heartbeat_round_trip_count.saturating_add(1);
                            stats.heartbeat_last_rtt_ms = rtt_ms;
                            stats.heartbeat_max_rtt_ms = stats.heartbeat_max_rtt_ms.max(rtt_ms);
                        }
                        HeartbeatAck::Duplicate => {
                            stats.heartbeat_duplicate_pong_count =
                                stats.heartbeat_duplicate_pong_count.saturating_add(1);
                        }
                        HeartbeatAck::Unknown => {
                            stats.heartbeat_unexpected_pong_count =
                                stats.heartbeat_unexpected_pong_count.saturating_add(1);
                        }
                    }
                    stats.heartbeat_pending_count = heartbeat_window.pending_count();
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

fn worker_loop_v2_r2(
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
            client_capabilities: 0b111
                | CAPABILITY_V2_STREAMS_R2
                | CAPABILITY_V2_MULTI_STREAM
                | CAPABILITY_V2_COMPRESSED_METADATA
                | CAPABILITY_V2_HARDWARE_CAPTURE_R2,
            max_payload: MAX_PAYLOAD_LEN as u32,
            client_name: "ScopeAnalyzer V2 R2".to_owned(),
        }),
    )?;

    let mut decoder = FrameDecoder::default();
    let mut read_buffer = [0_u8; 16 * 1024];
    let mut session_id = 0_u32;
    let mut rejected_session_ids = HashSet::new();
    let mut hello_ack: Option<HelloAck> = None;
    let mut channels: Option<ChannelTable> = None;
    let mut streams: Option<StreamTableR2> = None;
    let mut subscriptions: Option<ConfigureStreamsR2> = None;
    let mut pending = HashMap::new();
    let mut validator = SnapshotValidator::default();
    let mut capture: Option<CaptureAssemblerR2> = None;
    let mut stats = SessionStats::default();
    let mut streaming = false;
    let mut resetting = false;
    let mut last_received = Instant::now();
    let mut last_ping = Instant::now();
    let mut ping_nonce = 1_u64;
    let mut heartbeat_window = HeartbeatWindow::new(HEARTBEAT_NONCE_WINDOW);
    let mut stream_timing: HashMap<u16, StreamTiming> = HashMap::new();

    loop {
        while let Ok(command) = command_rx.try_recv() {
            match command {
                SessionCommand::ConfigureStreamsR2(configure) => {
                    if resetting || session_id == 0 {
                        return Err(SessionError::Acquisition(
                            "R2 subscriptions cannot be configured before handshake".to_owned(),
                        ));
                    }
                    let sequence = write_v2_r2_message(
                        &mut transport,
                        &mut out_sequence,
                        session_id,
                        MessageV2R2::ConfigureStreams(configure.clone()),
                    )?;
                    pending.insert(sequence, PendingCommand::ConfigureStreamsR2(configure));
                    send_control(
                        control_tx,
                        SessionEvent::State(ConnectionState::Configuring),
                    )?;
                }
                SessionCommand::ConfigureStream(_) => {
                    return Err(SessionError::Acquisition(
                        "SCP1 V2 R1 CONFIGURE_STREAM sent to an R2 session".to_owned(),
                    ));
                }
                SessionCommand::Start => {
                    if subscriptions.is_none() {
                        return Err(SessionError::Acquisition(
                            "R2 START requires an accepted atomic subscription set".to_owned(),
                        ));
                    }
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
                    record_heartbeat(&mut heartbeat_window, &mut stats, nonce);
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
                        "SCP1 V1 command sent to a V2 R2 session".to_owned(),
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
            return Err(SessionError::Acquisition(
                "no valid SCP1 V2 R2 frame received for 3 seconds".to_owned(),
            ));
        }
        let expired = heartbeat_window.expire(Instant::now());
        if expired != 0 {
            stats.heartbeat_timeout_count = stats.heartbeat_timeout_count.saturating_add(expired);
            stats.heartbeat_pending_count = heartbeat_window.pending_count();
            let _ = data_tx.try_send(SessionEvent::Stats(stats));
        }
        if session_id != 0 && !resetting && last_ping.elapsed() >= Duration::from_secs(1) {
            write_common_v2(
                &mut transport,
                &mut out_sequence,
                session_id,
                Message::Ping(ping_nonce),
            )?;
            record_heartbeat(&mut heartbeat_window, &mut stats, ping_nonce);
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
                continue;
            }
            if rejected_session_ids.contains(&frame.session_id) {
                stats.protocol_errors = stats.protocol_errors.saturating_add(1);
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
                    let required = CAPABILITY_V2_STREAMS_R2
                        | CAPABILITY_V2_MULTI_STREAM
                        | CAPABILITY_V2_COMPRESSED_METADATA;
                    if frame.session_id == 0
                        || hello.device_capabilities & required != required
                        || rejected_session_ids.contains(&frame.session_id)
                    {
                        return Err(SessionError::Acquisition(
                            "peer did not negotiate the frozen SCP1 V2 R2 capabilities".to_owned(),
                        ));
                    }
                    if resetting && frame.session_id == session_id {
                        return Err(SessionError::Acquisition(
                            "DeviceReset HELLO_ACK did not allocate a new session id".to_owned(),
                        ));
                    }
                    session_id = frame.session_id;
                    stats.session_id = session_id;
                    hello_ack = Some(hello.clone());
                    resetting = false;
                    send_control(control_tx, SessionEvent::HelloAck(hello))?;
                    send_control(control_tx, SessionEvent::Stats(stats))?;
                }
                super::protocol::MSG_CHANNEL_TABLE => {
                    let Message::ChannelTable(table) =
                        Message::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    let hello = hello_ack.as_ref().ok_or_else(|| {
                        SessionError::Acquisition(
                            "R2 CHANNEL_TABLE arrived before HELLO_ACK".to_owned(),
                        )
                    })?;
                    if usize::from(hello.channel_count) != table.channels.len() {
                        return Err(SessionError::Acquisition(
                            "R2 CHANNEL_TABLE count disagrees with HELLO_ACK".to_owned(),
                        ));
                    }
                    channels = Some(table.clone());
                    send_control(control_tx, SessionEvent::ChannelTable(table))?;
                }
                MSG_STREAM_TABLE_R2 => {
                    let MessageV2R2::StreamTable(table) =
                        MessageV2R2::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    let channel_table = channels.as_ref().ok_or_else(|| {
                        SessionError::Acquisition(
                            "STREAM_TABLE_R2 arrived before CHANNEL_TABLE".to_owned(),
                        )
                    })?;
                    table.validate_against_channels(channel_table)?;
                    if streams
                        .as_ref()
                        .is_some_and(|previous| previous.revision != table.revision)
                    {
                        validator.reset();
                        stream_timing.clear();
                    }
                    streams = Some(table.clone());
                    send_control(control_tx, SessionEvent::StreamTableR2(table))?;
                    send_control(control_tx, SessionEvent::State(ConnectionState::Ready))?;
                }
                super::protocol::MSG_COMMAND_RESULT => {
                    let Message::CommandResult(result) =
                        Message::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    if let Some(command) = pending.remove(&result.request_sequence) {
                        if result.result_code == ResultCode::Ok {
                            match command {
                                PendingCommand::ConfigureStreamsR2(configure) => {
                                    subscriptions = Some(configure.clone());
                                    send_control(
                                        control_tx,
                                        SessionEvent::ConfiguredV2R2(configure),
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
                                PendingCommand::Configure(_, _)
                                | PendingCommand::ConfigureStream(_) => {}
                            }
                        } else if matches!(command, PendingCommand::ConfigureStreamsR2(_)) {
                            send_control(control_tx, SessionEvent::State(ConnectionState::Ready))?;
                        }
                    }
                    send_control(control_tx, SessionEvent::CommandResult(result))?;
                }
                MSG_SAMPLE_BATCH_V2_R2 => {
                    let channel_table = channels.as_ref().ok_or_else(|| {
                        SessionError::Acquisition(
                            "R2 sample arrived before CHANNEL_TABLE".to_owned(),
                        )
                    })?;
                    let stream_table = streams.as_ref().ok_or_else(|| {
                        SessionError::Acquisition(
                            "R2 sample arrived before STREAM_TABLE_R2".to_owned(),
                        )
                    })?;
                    let decoded =
                        decode_stream_sample_frame_r2(&frame, channel_table, stream_table)?;
                    let descriptor = stream_table.stream(decoded.stream_id).ok_or_else(|| {
                        SessionError::Acquisition(
                            "decoded R2 stream disappeared from STREAM_TABLE_R2".to_owned(),
                        )
                    })?;
                    let hello = hello_ack.as_ref().ok_or_else(|| {
                        SessionError::Acquisition("R2 sample arrived before HELLO_ACK".to_owned())
                    })?;
                    validate_stream_sample_period_r2(
                        hello.tick_hz,
                        descriptor,
                        decoded.sample_period_ticks,
                    )?;
                    validate_v2_r2_stream_timing(&mut stream_timing, &decoded)?;
                    for row in &decoded.row_metadata {
                        validator
                            .observe_with_table(stream_table, descriptor, *row)
                            .map_err(SessionError::Acquisition)?;
                    }
                    let diagnostics = validator.diagnostics().clone();
                    apply_snapshot_diagnostics(&mut stats, &diagnostics);
                    stats.received_batches = stats.received_batches.saturating_add(1);
                    stats.received_samples = stats
                        .received_samples
                        .saturating_add(decoded.row_metadata.len() as u64);
                    let dropped_stream_id = decoded.stream_id;
                    let dropped_first = decoded.first_row_sequence;
                    let dropped_last = decoded
                        .row_metadata
                        .last()
                        .map_or(dropped_first, |row| row.row_sequence);
                    let dropped_rows = decoded.row_metadata.len() as u64;
                    let snapshot_dropped =
                        match data_tx.try_send(SessionEvent::SnapshotV2R2(decoded, diagnostics)) {
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
                    update_stream_stats(
                        &mut stats,
                        dropped_stream_id,
                        dropped_rows,
                        snapshot_dropped,
                        validator.diagnostics(),
                    );
                    if snapshot_dropped {
                        send_control(control_tx, SessionEvent::Stats(stats))?;
                    }
                }
                MSG_CAPTURE_BEGIN => {
                    let result: Result<CaptureAssemblerR2, SessionError> = (|| {
                        let MessageV2::CaptureBegin(begin) =
                            MessageV2::decode(frame.message_type, &frame.payload)?
                        else {
                            unreachable!()
                        };
                        let stream_table = streams.as_ref().ok_or_else(|| {
                            SessionError::Acquisition(
                                "R2 CAPTURE_BEGIN arrived before STREAM_TABLE_R2".to_owned(),
                            )
                        })?;
                        let descriptor = stream_table.stream(begin.stream_id).ok_or_else(|| {
                            SessionError::Acquisition(
                                "R2 CAPTURE_BEGIN references unknown stream".to_owned(),
                            )
                        })?;
                        let mut assembler = CaptureAssemblerR2::default();
                        assembler
                            .begin_with_descriptor(begin, descriptor, stream_table.revision)
                            .map_err(SessionError::Acquisition)?;
                        Ok(assembler)
                    })();
                    match result {
                        Ok(assembler) => capture = Some(assembler),
                        Err(error) => {
                            capture = None;
                            report_capture_failure(
                                control_tx,
                                data_tx,
                                &mut stats,
                                error.to_string(),
                            )?;
                        }
                    }
                }
                MSG_CAPTURE_DATA_R2 => {
                    let result: Result<(), SessionError> = (|| {
                        let MessageV2R2::CaptureData(data) =
                            MessageV2R2::decode(frame.message_type, &frame.payload)?
                        else {
                            unreachable!()
                        };
                        let channel_table = channels.as_ref().ok_or_else(|| {
                            SessionError::Acquisition(
                                "R2 CAPTURE_DATA arrived before CHANNEL_TABLE".to_owned(),
                            )
                        })?;
                        let stream_table = streams.as_ref().ok_or_else(|| {
                            SessionError::Acquisition(
                                "R2 CAPTURE_DATA arrived before STREAM_TABLE_R2".to_owned(),
                            )
                        })?;
                        let nested = MessageV2R2::StreamSampleBatch(data.batch.clone())
                            .into_frame(0, frame.sequence, session_id, frame.timestamp_ticks)?;
                        let _ =
                            decode_stream_sample_frame_r2(&nested, channel_table, stream_table)?;
                        let encoded_payload: Arc<[u8]> = frame.payload.into();
                        capture
                            .as_mut()
                            .ok_or_else(|| {
                                SessionError::Acquisition(
                                    "R2 CAPTURE_DATA arrived before CAPTURE_BEGIN".to_owned(),
                                )
                            })?
                            .push_encoded_payload(data, encoded_payload)
                            .map_err(SessionError::Acquisition)
                    })();
                    if let Err(error) = result {
                        capture = None;
                        report_capture_failure(control_tx, data_tx, &mut stats, error.to_string())?;
                    }
                }
                MSG_CAPTURE_END => {
                    let result: Result<AssembledCaptureR2, SessionError> = (|| {
                        let MessageV2::CaptureEnd(end) =
                            MessageV2::decode(frame.message_type, &frame.payload)?
                        else {
                            unreachable!()
                        };
                        let mut assembler = capture.take().ok_or_else(|| {
                            SessionError::Acquisition(
                                "R2 CAPTURE_END arrived before CAPTURE_BEGIN".to_owned(),
                            )
                        })?;
                        assembler.finish(end).map_err(SessionError::Acquisition)
                    })();
                    capture = None;
                    match result {
                        Ok(assembled) => {
                            send_control(control_tx, SessionEvent::CaptureCompleteR2(assembled))?
                        }
                        Err(error) => {
                            report_capture_failure(
                                control_tx,
                                data_tx,
                                &mut stats,
                                error.to_string(),
                            )?;
                        }
                    }
                }
                super::protocol_v2::MSG_CAPTURE_STATUS => {
                    let MessageV2::CaptureStatus(status) =
                        MessageV2::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    if status.state == CaptureState::DeviceReset {
                        let old_session_id = session_id;
                        rejected_session_ids.insert(old_session_id);
                        capture = None;
                        validator.reset();
                        stream_timing.clear();
                        heartbeat_window.clear();
                        pending.clear();
                        subscriptions = None;
                        channels = None;
                        streams = None;
                        hello_ack = None;
                        streaming = false;
                        resetting = true;
                        session_id = 0;
                        ping_nonce = 1;
                        let reset_count = stats.device_reset_count.saturating_add(1);
                        stats = SessionStats::default();
                        stats.device_reset_count = reset_count;
                        send_control(
                            control_tx,
                            SessionEvent::State(ConnectionState::DeviceResetHandshake),
                        )?;
                        send_control(
                            control_tx,
                            SessionEvent::Error("设备已复位，等待重新握手".to_owned()),
                        )?;
                    } else if matches!(
                        status.state,
                        CaptureState::Idle
                            | CaptureState::Cancelled
                            | CaptureState::Timeout
                            | CaptureState::BufferOverrun
                            | CaptureState::InvalidConfig
                    ) {
                        capture = None;
                    }
                    send_control(control_tx, SessionEvent::CaptureStatus(status))?;
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
                super::protocol::MSG_PONG => {
                    let Message::Pong(nonce) = Message::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    apply_heartbeat_ack(&mut heartbeat_window, &mut stats, nonce, Instant::now());
                    let _ = data_tx.try_send(SessionEvent::Stats(stats));
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
                super::protocol::MSG_ERROR => {
                    let Message::Error(error) =
                        Message::decode(frame.message_type, &frame.payload)?
                    else {
                        unreachable!()
                    };
                    send_control(control_tx, SessionEvent::Error(error.detail))?;
                }
                MSG_SAMPLE_BATCH_V2_R1
                | MSG_STREAM_TABLE_R1
                | super::protocol_v2::MSG_CONFIGURE_STREAM_R1
                | MSG_CAPTURE_DATA_R1 => {
                    stats.protocol_errors = stats.protocol_errors.saturating_add(1);
                    send_control(
                        control_tx,
                        SessionEvent::Error(
                            "SCP1 V2 R1 message received in an R2 session".to_owned(),
                        ),
                    )?;
                }
                _ => {
                    stats.unknown_messages = stats.unknown_messages.saturating_add(1);
                }
            }
        }
    }
}

fn record_heartbeat(window: &mut HeartbeatWindow, stats: &mut SessionStats, nonce: u64) {
    if window.record(nonce, Instant::now()) {
        stats.heartbeat_window_overflow_count =
            stats.heartbeat_window_overflow_count.saturating_add(1);
    }
    stats.heartbeat_pending_count = window.pending_count();
}

fn apply_heartbeat_ack(
    window: &mut HeartbeatWindow,
    stats: &mut SessionStats,
    nonce: u64,
    now: Instant,
) {
    stats.last_pong_nonce = Some(nonce);
    match window.acknowledge(nonce, now) {
        HeartbeatAck::Matched(rtt) => {
            let rtt_ms = u64::try_from(rtt.as_millis()).unwrap_or(u64::MAX);
            stats.heartbeat_round_trip_count = stats.heartbeat_round_trip_count.saturating_add(1);
            stats.heartbeat_last_rtt_ms = rtt_ms;
            stats.heartbeat_max_rtt_ms = stats.heartbeat_max_rtt_ms.max(rtt_ms);
        }
        HeartbeatAck::Duplicate => {
            stats.heartbeat_duplicate_pong_count =
                stats.heartbeat_duplicate_pong_count.saturating_add(1);
        }
        HeartbeatAck::Unknown => {
            stats.heartbeat_unexpected_pong_count =
                stats.heartbeat_unexpected_pong_count.saturating_add(1);
        }
    }
    stats.heartbeat_pending_count = window.pending_count();
}

fn apply_snapshot_diagnostics(stats: &mut SessionStats, diagnostics: &SnapshotDiagnostics) {
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
    stats.causal_cached_rows = diagnostics.causal_cached_rows;
    stats.causal_pending_matches = diagnostics.causal_pending_matches;
    stats.causal_match_timeouts = diagnostics.causal_match_timeouts;
    stats.causal_cache_evictions = diagnostics.causal_cache_evictions;
    stats.causal_window_overflows = diagnostics.causal_window_overflows;
    stats.causal_duplicate_cycles = diagnostics.causal_duplicate_cycles;
}

fn update_stream_stats(
    stats: &mut SessionStats,
    stream_id: u16,
    rows: u64,
    dropped: bool,
    diagnostics: &SnapshotDiagnostics,
) {
    let slot = stats
        .stream_stats
        .iter()
        .position(|stream| stream.stream_id == stream_id)
        .or_else(|| {
            stats
                .stream_stats
                .iter()
                .position(|stream| stream.stream_id == 0)
        });
    let Some(slot) = slot else {
        return;
    };
    let stream = &mut stats.stream_stats[slot];
    stream.stream_id = stream_id;
    stream.received_batches = stream.received_batches.saturating_add(1);
    stream.received_rows = stream.received_rows.saturating_add(rows);
    if dropped {
        stream.host_dropped_batches = stream.host_dropped_batches.saturating_add(1);
        stream.host_dropped_rows = stream.host_dropped_rows.saturating_add(rows);
    }
    stream.row_sequence_gaps = diagnostics.row_sequence_gaps;
    stream.row_sequence_reorders = diagnostics.row_sequence_reorders;
    stream.logical_cycle_faults = diagnostics.logical_cycle_faults;
}

fn validate_stream_sample_period_r2(
    tick_hz: u64,
    descriptor: &super::protocol_v2_r2::StreamDescriptorR2,
    sample_period_ticks: u32,
) -> Result<(), SessionError> {
    let rate = u64::from(descriptor.sample_rate_hz);
    if tick_hz == 0 || !tick_hz.is_multiple_of(rate) {
        return Err(SessionError::Acquisition(
            "R2 tick_hz is not exactly divisible by the stream rate".to_owned(),
        ));
    }
    let expected = tick_hz / rate;
    if expected == 0 || expected > u64::from(u32::MAX) || u64::from(sample_period_ticks) != expected
    {
        return Err(SessionError::Acquisition(
            "R2 sample period does not match the frozen stream rate".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct StreamTiming {
    sample_period_ticks: u32,
    last_row_sequence: u64,
    last_timestamp_ticks: u64,
}

fn validate_v2_stream_timing(
    timing: &mut HashMap<u16, StreamTiming>,
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
    let last_row_sequence = batch
        .row_metadata
        .last()
        .map(|row| row.row_sequence)
        .ok_or_else(|| SessionError::Acquisition("V2 stream batch has no rows".to_owned()))?;
    if let Some(previous) = timing.get(&batch.stream_id) {
        if previous.sample_period_ticks != batch.sample_period_ticks {
            return Err(SessionError::Acquisition(format!(
                "V2 stream {} changed sample_period_ticks during the session",
                batch.stream_id
            )));
        }
        let row_delta = batch
            .first_row_sequence
            .checked_sub(previous.last_row_sequence)
            .filter(|delta| *delta != 0)
            .ok_or_else(|| {
                SessionError::Acquisition(format!(
                    "V2 stream {} row_sequence is not strictly increasing",
                    batch.stream_id
                ))
            })?;
        let timestamp_delta = u64::from(batch.sample_period_ticks)
            .checked_mul(row_delta)
            .ok_or_else(|| SessionError::Acquisition("V2 sample timestamp overflow".to_owned()))?;
        let expected_timestamp = previous
            .last_timestamp_ticks
            .checked_add(timestamp_delta)
            .ok_or_else(|| SessionError::Acquisition("V2 sample timestamp overflow".to_owned()))?;
        if batch.timestamp_ticks != expected_timestamp {
            return Err(SessionError::Acquisition(format!(
                "V2 stream {} timestamp does not correspond to row_sequence",
                batch.stream_id
            )));
        }
    }
    timing.insert(
        batch.stream_id,
        StreamTiming {
            sample_period_ticks: batch.sample_period_ticks,
            last_row_sequence,
            last_timestamp_ticks: last_timestamp,
        },
    );
    Ok(())
}

fn validate_v2_r2_stream_timing(
    timing: &mut HashMap<u16, StreamTiming>,
    batch: &DecodedStreamSampleBatchR2,
) -> Result<(), SessionError> {
    let row_count = u64::try_from(batch.row_metadata.len()).map_err(|_| {
        SessionError::Acquisition("R2 stream row count does not fit u64".to_owned())
    })?;
    let last_offset = u64::from(batch.sample_period_ticks)
        .checked_mul(row_count.saturating_sub(1))
        .ok_or_else(|| SessionError::Acquisition("R2 sample timestamp overflow".to_owned()))?;
    let last_timestamp = batch
        .timestamp_ticks
        .checked_add(last_offset)
        .ok_or_else(|| SessionError::Acquisition("R2 sample timestamp overflow".to_owned()))?;
    let last_row_sequence = batch
        .row_metadata
        .last()
        .map(|row| row.row_sequence)
        .ok_or_else(|| SessionError::Acquisition("R2 stream batch has no rows".to_owned()))?;
    if let Some(previous) = timing.get(&batch.stream_id) {
        if previous.sample_period_ticks != batch.sample_period_ticks {
            return Err(SessionError::Acquisition(format!(
                "R2 stream {} changed sample_period_ticks",
                batch.stream_id
            )));
        }
        let row_delta = batch
            .first_row_sequence
            .checked_sub(previous.last_row_sequence)
            .filter(|delta| *delta != 0)
            .ok_or_else(|| {
                SessionError::Acquisition(format!(
                    "R2 stream {} row_sequence is not strictly increasing",
                    batch.stream_id
                ))
            })?;
        let expected_timestamp = u64::from(batch.sample_period_ticks)
            .checked_mul(row_delta)
            .and_then(|delta| previous.last_timestamp_ticks.checked_add(delta))
            .ok_or_else(|| SessionError::Acquisition("R2 sample timestamp overflow".to_owned()))?;
        if batch.timestamp_ticks != expected_timestamp {
            return Err(SessionError::Acquisition(format!(
                "R2 stream {} timestamp does not correspond to row_sequence",
                batch.stream_id
            )));
        }
    }
    timing.insert(
        batch.stream_id,
        StreamTiming {
            sample_period_ticks: batch.sample_period_ticks,
            last_row_sequence,
            last_timestamp_ticks: last_timestamp,
        },
    );
    Ok(())
}

fn report_capture_failure(
    control_tx: &Sender<SessionEvent>,
    data_tx: &Sender<SessionEvent>,
    stats: &mut SessionStats,
    error: String,
) -> Result<(), SessionError> {
    stats.capture_processing_overruns = stats.capture_processing_overruns.saturating_add(1);
    let _ = data_tx.try_send(SessionEvent::Stats(*stats));
    send_control(control_tx, SessionEvent::CaptureFailure(error))
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

fn write_v2_r2_message(
    transport: &mut TransportStream,
    next_sequence: &mut u32,
    session_id: u32,
    message: MessageV2R2,
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
        protocol_v2_r2::StreamSubscriptionR2,
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
        first_row_sequence: u64,
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
            first_row_sequence,
            sample_period_ticks,
            timestamp_ticks,
            channel_ids: vec![0],
            channels: vec![vec![0.0; rows as usize]],
            row_metadata: (0..rows)
                .map(|offset| SnapshotMeta {
                    row_sequence: first_row_sequence + offset,
                    logical_cycle_sequence: first_row_sequence + offset,
                    ..SnapshotMeta::default()
                })
                .collect(),
            raw_frame: Vec::new(),
        }
    }

    #[test]
    fn v2_stream_timing_requires_fixed_period_monotonicity_and_no_overflow() {
        let mut timing = HashMap::new();
        validate_v2_stream_timing(&mut timing, &v2_timing_batch(1, 10, 1_000, 10_000, 4)).unwrap();
        validate_v2_stream_timing(&mut timing, &v2_timing_batch(1, 14, 1_000, 14_000, 2)).unwrap();

        let changed_period =
            validate_v2_stream_timing(&mut timing, &v2_timing_batch(1, 16, 4_000, 16_000, 1))
                .unwrap_err();
        assert!(changed_period
            .to_string()
            .contains("changed sample_period_ticks"));

        let wrong_row_timestamp =
            validate_v2_stream_timing(&mut timing, &v2_timing_batch(1, 17, 1_000, 17_500, 1))
                .unwrap_err();
        assert!(wrong_row_timestamp
            .to_string()
            .contains("does not correspond to row_sequence"));

        let reordered =
            validate_v2_stream_timing(&mut timing, &v2_timing_batch(1, 15, 1_000, 15_000, 1))
                .unwrap_err();
        assert!(reordered
            .to_string()
            .contains("row_sequence is not strictly increasing"));

        let overflow = validate_v2_stream_timing(
            &mut HashMap::new(),
            &v2_timing_batch(2, 1, 1_000, u64::MAX - 500, 2),
        )
        .unwrap_err();
        assert!(overflow.to_string().contains("timestamp overflow"));
    }

    #[test]
    fn heartbeat_window_accepts_multiple_out_of_order_pongs_and_is_bounded() {
        let now = Instant::now();
        let mut window = HeartbeatWindow::new(2);
        assert!(!window.record(10, now));
        assert!(!window.record(11, now + Duration::from_millis(1)));
        assert!(matches!(
            window.acknowledge(11, now + Duration::from_millis(2)),
            HeartbeatAck::Matched(_)
        ));
        assert!(matches!(
            window.acknowledge(10, now + Duration::from_millis(3)),
            HeartbeatAck::Matched(_)
        ));
        assert_eq!(
            window.acknowledge(10, now + Duration::from_millis(4)),
            HeartbeatAck::Duplicate
        );

        assert!(!window.record(20, now));
        assert!(!window.record(21, now + Duration::from_millis(1)));
        assert!(window.record(22, now + Duration::from_millis(2)));
        assert_eq!(
            window.acknowledge(20, now + Duration::from_millis(3)),
            HeartbeatAck::Unknown
        );
        assert!(matches!(
            window.acknowledge(21, now + Duration::from_millis(3)),
            HeartbeatAck::Matched(_)
        ));
        assert!(matches!(
            window.acknowledge(22, now + Duration::from_millis(3)),
            HeartbeatAck::Matched(_)
        ));
    }

    #[test]
    fn heartbeat_timeout_rtt_unknown_duplicate_and_reset_are_independent() {
        let now = Instant::now();
        let mut window = HeartbeatWindow::new(8);
        let mut stats = SessionStats::default();
        assert!(!window.record(1, now));
        assert_eq!(window.expire(now + Duration::from_millis(2_999)), 0);
        apply_heartbeat_ack(
            &mut window,
            &mut stats,
            1,
            now + Duration::from_millis(1_250),
        );
        assert_eq!(stats.heartbeat_round_trip_count, 1);
        assert_eq!(stats.heartbeat_last_rtt_ms, 1_250);
        assert_eq!(stats.heartbeat_max_rtt_ms, 1_250);
        apply_heartbeat_ack(
            &mut window,
            &mut stats,
            1,
            now + Duration::from_millis(1_500),
        );
        apply_heartbeat_ack(
            &mut window,
            &mut stats,
            99,
            now + Duration::from_millis(1_500),
        );
        assert_eq!(stats.heartbeat_duplicate_pong_count, 1);
        assert_eq!(stats.heartbeat_unexpected_pong_count, 1);

        assert!(!window.record(2, now));
        assert_eq!(window.expire(now + Duration::from_secs(3)), 1);
        assert_eq!(window.pending_count(), 0);
        window.record(3, now);
        window.clear();
        assert_eq!(window.pending_count(), 0);
        assert_eq!(window.acknowledge(3, now), HeartbeatAck::Unknown);
    }

    #[test]
    fn heartbeat_window_overflow_is_not_reported_as_timeout() {
        let mut window = HeartbeatWindow::new(1);
        let mut stats = SessionStats::default();
        record_heartbeat(&mut window, &mut stats, 10);
        record_heartbeat(&mut window, &mut stats, 11);
        assert_eq!(stats.heartbeat_window_overflow_count, 1);
        assert_eq!(stats.heartbeat_timeout_count, 0);
        assert_eq!(stats.protocol_errors, 0);
        assert_eq!(stats.heartbeat_pending_count, 1);
        apply_heartbeat_ack(&mut window, &mut stats, 11, Instant::now());
        assert_eq!(stats.heartbeat_round_trip_count, 1);
        assert_eq!(stats.heartbeat_pending_count, 0);
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
            protocol: SimulatorProtocol::V2R1,
            preset: Some(V2Preset::Normal30k),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect_v2_r1(TransportConfig::Tcp {
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
    fn v2_r2_simulator_atomically_runs_three_independent_streams() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: false,
            protocol: SimulatorProtocol::V2R2,
            preset: Some(V2Preset::CausalInOrder30k),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect_v2_r2(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        let table = wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::StreamTableR2(_))
        });
        let SessionEvent::StreamTableR2(table) = table else {
            unreachable!()
        };
        assert_eq!(
            table
                .streams
                .iter()
                .map(|stream| (stream.stream_id, stream.logical_cycle_step))
                .collect::<Vec<_>>(),
            vec![(1, 1), (2, 4), (3, 32)]
        );
        let subscriptions = table
            .streams
            .iter()
            .map(|stream| StreamSubscriptionR2 {
                stream_id: stream.stream_id,
                batch_samples: match stream.stream_id {
                    1 => 32,
                    2 => 8,
                    _ => 1,
                },
                channel_mask: stream
                    .channel_ids
                    .iter()
                    .fold(0_u64, |mask, id| mask | (1_u64 << id)),
            })
            .collect();
        session
            .configure_streams_r2(ConfigureStreamsR2 {
                transaction_id: 1,
                subscriptions,
            })
            .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::ConfiguredV2R2(_))
        });
        session
            .configure_streams_r2(ConfigureStreamsR2 {
                transaction_id: 2,
                subscriptions: vec![StreamSubscriptionR2 {
                    stream_id: 2,
                    batch_samples: 8,
                    channel_mask: 1,
                }],
            })
            .unwrap();
        let rejected = wait_for(&session, Duration::from_secs(2), |event| {
            matches!(
                event,
                SessionEvent::CommandResult(result)
                    if result.result_code == crate::live::protocol::ResultCode::InvalidArgument
            )
        });
        assert!(matches!(rejected, SessionEvent::CommandResult(_)));
        session.start().unwrap();
        let mut observed = HashSet::new();
        while observed.len() != 3 {
            let event = wait_for(&session, Duration::from_secs(2), |event| {
                matches!(event, SessionEvent::SnapshotV2R2(_, _))
            });
            let SessionEvent::SnapshotV2R2(batch, diagnostics) = event else {
                unreachable!()
            };
            let descriptor = table.stream(batch.stream_id).unwrap();
            assert_eq!(
                batch.row_metadata[0].logical_cycle_sequence,
                batch.row_metadata[0].row_sequence * u64::from(descriptor.logical_cycle_step)
            );
            assert_eq!(diagnostics.causal_match_timeouts, 0);
            assert_eq!(diagnostics.causal_window_overflows, 0);
            observed.insert(batch.stream_id);
        }
        session.stop().unwrap();
        session.disconnect().unwrap();
    }

    #[test]
    fn v2_r2_device_reset_allocates_a_new_idle_session() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
            protocol: SimulatorProtocol::V2R2,
            preset: Some(V2Preset::DeviceReset30k),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect_v2_r2(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::StreamTableR2(_))
        });
        session
            .arm_capture(ArmCapture {
                capture_id: 99,
                stream_id: 1,
                pretrigger_rows: 1,
                posttrigger_rows: 1,
                timeout_samples: 64,
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
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(
                event,
                SessionEvent::State(ConnectionState::DeviceResetHandshake)
            )
        });
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::StreamTableR2(_))
        });
        let stats = wait_for(
            &session,
            Duration::from_secs(2),
            |event| matches!(event, SessionEvent::Stats(stats) if stats.session_id == 2),
        );
        let SessionEvent::Stats(stats) = stats else {
            unreachable!()
        };
        assert_eq!(stats.device_reset_count, 1);
        assert!(
            stats.protocol_errors >= 1,
            "stale session frame was rejected"
        );
        assert_eq!(stats.heartbeat_pending_count, 0);
        assert_eq!(simulator.stats().configure_requests, 0);
        assert_eq!(simulator.stats().start_requests, 0);
        session.disconnect().unwrap();
    }

    #[test]
    fn v2_r2_capture_failure_keeps_session_and_second_arm_succeeds() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
            protocol: SimulatorProtocol::V2R2,
            preset: Some(V2Preset::CaptureChunkLoss30k),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect_v2_r2(TransportConfig::Tcp {
            address: simulator.address().to_string(),
        })
        .unwrap();
        wait_for(&session, Duration::from_secs(2), |event| {
            matches!(event, SessionEvent::StreamTableR2(_))
        });
        for capture_id in [1, 2] {
            session
                .arm_capture(ArmCapture {
                    capture_id,
                    stream_id: 1,
                    pretrigger_rows: 1,
                    posttrigger_rows: 1,
                    timeout_samples: 64,
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
            if capture_id == 1 {
                wait_for(&session, Duration::from_secs(2), |event| {
                    matches!(event, SessionEvent::CaptureFailure(_))
                });
            } else {
                let event = wait_for(&session, Duration::from_secs(2), |event| {
                    matches!(event, SessionEvent::CaptureCompleteR2(_))
                });
                let SessionEvent::CaptureCompleteR2(capture) = event else {
                    unreachable!()
                };
                assert!(capture.diagnostics.capture_complete);
                assert_eq!(capture.blocks.len(), 2);
            }
        }
        session.disconnect().unwrap();
    }

    #[test]
    fn v2_r2_causal_presets_have_deterministic_end_to_end_diagnostics() {
        let presets = [
            (V2Preset::CausalInOrder30k, false, false),
            (V2Preset::CausalResultFirst30k, false, false),
            (V2Preset::CausalApplicationFirst30k, false, false),
            (V2Preset::CausalNonzeroOffset30k, false, false),
            (V2Preset::CausalNegativeOffset30k, false, false),
            (V2Preset::CausalSourceTimeout30k, true, false),
            (V2Preset::CausalDuplicateCycle30k, false, true),
            (V2Preset::CausalWatermarkEviction30k, true, false),
        ];
        for (preset, expect_timeout, expect_duplicate) in presets {
            let simulator = SimulatorHandle::spawn(SimulatorConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                accelerated: false,
                protocol: SimulatorProtocol::V2R2,
                preset: Some(preset),
                ..SimulatorConfig::default()
            })
            .unwrap();
            let session = LiveSession::connect_v2_r2(TransportConfig::Tcp {
                address: simulator.address().to_string(),
            })
            .unwrap();
            let event = wait_for(&session, Duration::from_secs(2), |event| {
                matches!(event, SessionEvent::StreamTableR2(_))
            });
            let SessionEvent::StreamTableR2(table) = event else {
                unreachable!()
            };
            let subscriptions = table
                .streams
                .iter()
                .map(|stream| StreamSubscriptionR2 {
                    stream_id: stream.stream_id,
                    batch_samples: match stream.stream_id {
                        1 => 32,
                        2 => 8,
                        _ => 1,
                    },
                    channel_mask: stream
                        .channel_ids
                        .iter()
                        .fold(0_u64, |mask, id| mask | (1_u64 << id)),
                })
                .collect();
            session
                .configure_streams_r2(ConfigureStreamsR2 {
                    transaction_id: 7,
                    subscriptions,
                })
                .unwrap();
            wait_for(&session, Duration::from_secs(2), |event| {
                matches!(event, SessionEvent::ConfiguredV2R2(_))
            });
            session.start().unwrap();
            let mut latest = SnapshotDiagnostics::default();
            for _ in 0..36 {
                let event = wait_for(&session, Duration::from_secs(2), |event| {
                    matches!(event, SessionEvent::SnapshotV2R2(_, _))
                });
                let SessionEvent::SnapshotV2R2(_, diagnostics) = event else {
                    unreachable!()
                };
                latest = diagnostics;
                if (expect_timeout && latest.causal_match_timeouts > 0)
                    || (expect_duplicate && latest.causal_duplicate_cycles > 0)
                {
                    break;
                }
            }
            assert_eq!(
                latest.causal_source_mismatch,
                0,
                "unexpected source mismatch for {}",
                preset.name()
            );
            assert_eq!(
                latest.causal_application_mismatch,
                0,
                "unexpected application mismatch for {}",
                preset.name()
            );
            assert_eq!(
                latest.causal_match_timeouts > 0,
                expect_timeout,
                "timeout result for {}",
                preset.name()
            );
            assert_eq!(
                latest.causal_duplicate_cycles > 0,
                expect_duplicate,
                "duplicate result for {}",
                preset.name()
            );
            session.disconnect().unwrap();
        }
    }

    #[test]
    fn v2_manual_capture_is_assembled_in_memory() {
        let simulator = SimulatorHandle::spawn(SimulatorConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            accelerated: true,
            protocol: SimulatorProtocol::V2R1,
            preset: Some(V2Preset::CaptureManual30k),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect_v2_r1(TransportConfig::Tcp {
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
            protocol: SimulatorProtocol::V2R1,
            preset: Some(V2Preset::Normal30k),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect_v2_r1(TransportConfig::Tcp {
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
            protocol: SimulatorProtocol::V2R1,
            preset: Some(V2Preset::Normal30k),
            ..SimulatorConfig::default()
        })
        .unwrap();
        let session = LiveSession::connect_v2_r1(TransportConfig::Tcp {
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
        let presets = [
            V2Preset::Normal30k,
            V2Preset::CausalDelay30k,
            V2Preset::ClaStale30k,
            V2Preset::RowGap30k,
            V2Preset::RowReorder30k,
            V2Preset::PhaseMismatch30k,
            V2Preset::GroupMismatch30k,
            V2Preset::UnfrozenRow30k,
            V2Preset::CaptureManual30k,
            V2Preset::CaptureEdge30k,
            V2Preset::CaptureFault30k,
            V2Preset::CaptureTimeout30k,
            V2Preset::CaptureChunkLoss30k,
            V2Preset::CaptureChunkReorder30k,
            V2Preset::DeviceReset30k,
        ];

        for preset in presets {
            let simulator = SimulatorHandle::spawn(SimulatorConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                accelerated: true,
                protocol: SimulatorProtocol::V2R1,
                preset: Some(preset),
                ..SimulatorConfig::default()
            })
            .unwrap();
            let session = LiveSession::connect_v2_r1(TransportConfig::Tcp {
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

            match preset {
                V2Preset::Normal30k | V2Preset::CausalDelay30k => {
                    session.start().unwrap();
                    let event = wait_for(&session, Duration::from_secs(2), |event| {
                        matches!(
                            event,
                            SessionEvent::SnapshotV2(batch, diagnostics)
                                if diagnostics == &SnapshotDiagnostics::default()
                                    && batch.row_metadata.iter().all(|row| {
                                        row.source_sequence == row.logical_cycle_sequence
                                            && row.applied_sequence
                                                == row.logical_cycle_sequence.saturating_sub(1)
                                    })
                        )
                    });
                    assert!(matches!(
                        event,
                        SessionEvent::SnapshotV2(_, diagnostics)
                            if diagnostics == SnapshotDiagnostics::default()
                    ));
                }
                V2Preset::ClaStale30k | V2Preset::UnfrozenRow30k => {
                    session.start().unwrap();
                    let event = wait_for(&session, Duration::from_secs(2), |event| {
                        matches!(
                            event,
                            SessionEvent::SnapshotV2(_, diagnostics)
                                if diagnostics.invalid_snapshot_rows == 2
                                    && diagnostics.row_sequence_gaps == 0
                                    && diagnostics.row_sequence_reorders == 0
                        )
                    });
                    assert!(matches!(
                        event,
                        SessionEvent::SnapshotV2(_, diagnostics)
                            if diagnostics.invalid_snapshot_rows == 2
                    ));
                }
                V2Preset::RowGap30k => {
                    session.start().unwrap();
                    let event = wait_for(&session, Duration::from_secs(2), |event| {
                        matches!(
                            event,
                            SessionEvent::SnapshotV2(_, diagnostics)
                                if diagnostics.row_sequence_gaps == 1
                                    && diagnostics.row_sequence_reorders == 0
                                    && diagnostics.invalid_snapshot_rows == 0
                        )
                    });
                    assert!(matches!(
                        event,
                        SessionEvent::SnapshotV2(_, diagnostics)
                            if diagnostics.row_sequence_gaps == 1
                    ));
                }
                V2Preset::RowReorder30k => {
                    session.start().unwrap();
                    let event = wait_for(&session, Duration::from_secs(2), |event| {
                        matches!(
                            event,
                            SessionEvent::Error(error)
                                if error.contains("row_sequence is not strictly increasing")
                        )
                    });
                    assert!(matches!(event, SessionEvent::Error(_)));
                }
                V2Preset::PhaseMismatch30k => {
                    session.start().unwrap();
                    let event = wait_for(&session, Duration::from_secs(2), |event| {
                        matches!(
                            event,
                            SessionEvent::Error(error)
                                if error.contains("invalid domain/capture phase combination")
                        )
                    });
                    assert!(matches!(event, SessionEvent::Error(_)));
                }
                V2Preset::GroupMismatch30k => {
                    session.start().unwrap();
                    let event = wait_for(&session, Duration::from_secs(2), |event| {
                        matches!(
                            event,
                            SessionEvent::Error(error)
                                if error.contains("domain, phase, or consistency group")
                        )
                    });
                    assert!(matches!(event, SessionEvent::Error(_)));
                }
                V2Preset::CaptureManual30k
                | V2Preset::CaptureEdge30k
                | V2Preset::CaptureFault30k
                | V2Preset::CaptureTimeout30k
                | V2Preset::CaptureChunkLoss30k
                | V2Preset::CaptureChunkReorder30k
                | V2Preset::DeviceReset30k => {
                    let capture_id = 700 + preset as u32;
                    let trigger_kind = match preset {
                        V2Preset::CaptureEdge30k => CaptureTriggerKind::Edge,
                        V2Preset::CaptureFault30k => CaptureTriggerKind::FaultFlag,
                        _ => CaptureTriggerKind::Manual,
                    };
                    session
                        .arm_capture(ArmCapture {
                            capture_id,
                            stream_id: 1,
                            pretrigger_rows: 1,
                            posttrigger_rows: 1,
                            timeout_samples: 100,
                            trigger: CaptureTrigger {
                                kind: trigger_kind,
                                channel_id: 0,
                                level: 0.0,
                                edge: CaptureEdge::Rising,
                                hysteresis: 0.0,
                                flag_mask: u32::from(trigger_kind == CaptureTriggerKind::FaultFlag),
                                flag_value: u32::from(
                                    trigger_kind == CaptureTriggerKind::FaultFlag,
                                ),
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
                    match preset {
                        V2Preset::CaptureManual30k
                        | V2Preset::CaptureEdge30k
                        | V2Preset::CaptureFault30k
                        | V2Preset::CaptureChunkReorder30k => {
                            let event = wait_for(&session, Duration::from_secs(2), |event| {
                                matches!(event, SessionEvent::CaptureComplete(_))
                            });
                            let SessionEvent::CaptureComplete(capture) = event else {
                                unreachable!()
                            };
                            assert_eq!(capture.begin.capture_id, capture_id);
                            assert_eq!(capture.begin.row_count, 2);
                            assert_eq!(capture.blocks.len(), 2);
                            assert!(capture.diagnostics.capture_complete);
                            assert_eq!(
                                capture.diagnostics.capture_reordered_chunks,
                                2 * u32::from(preset == V2Preset::CaptureChunkReorder30k)
                            );
                            assert_eq!(capture.diagnostics.capture_missing_chunks, 0);
                        }
                        V2Preset::CaptureChunkLoss30k => {
                            let event = wait_for(&session, Duration::from_secs(2), |event| {
                                matches!(
                                    event,
                                    SessionEvent::CaptureFailure(error)
                                        if error.contains("uploaded or dropped row count")
                                )
                            });
                            assert!(matches!(event, SessionEvent::CaptureFailure(_)));
                            let nonce = 0xCAFE_0000 + u64::from(preset as u8);
                            session.ping(nonce).unwrap();
                            let pong = wait_for(&session, Duration::from_secs(2), |event| {
                                matches!(
                                    event,
                                    SessionEvent::Stats(stats)
                                        if stats.last_pong_nonce == Some(nonce)
                                )
                            });
                            assert!(matches!(
                                pong,
                                SessionEvent::Stats(stats)
                                    if stats.heartbeat_round_trip_count > 0
                            ));
                        }
                        V2Preset::CaptureTimeout30k | V2Preset::DeviceReset30k => {
                            let expected_state = if preset == V2Preset::CaptureTimeout30k {
                                CaptureState::Timeout
                            } else {
                                CaptureState::DeviceReset
                            };
                            let event = wait_for(&session, Duration::from_secs(2), |event| {
                                matches!(
                                    event,
                                    SessionEvent::CaptureStatus(status)
                                        if status.state == expected_state
                                            && status.captured_rows == 0
                                            && status.dropped_rows == 0
                                )
                            });
                            assert!(
                                matches!(event, SessionEvent::CaptureStatus(status) if status.state == expected_state)
                            );
                        }
                        _ => unreachable!(),
                    }
                }
                V2Preset::CausalInOrder30k
                | V2Preset::CausalResultFirst30k
                | V2Preset::CausalApplicationFirst30k
                | V2Preset::CausalSourceTimeout30k
                | V2Preset::CausalNonzeroOffset30k
                | V2Preset::CausalNegativeOffset30k
                | V2Preset::CausalDuplicateCycle30k
                | V2Preset::CausalWatermarkEviction30k => {
                    unreachable!("R2-only causal preset entered the R1 acceptance matrix")
                }
            }
            let _ = session.disconnect();
        }
    }
}
