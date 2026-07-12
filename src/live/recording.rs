use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::presentation::ChannelPresentation;

use super::{
    buffer::{GapReason, LiveGap},
    protocol::{
        crc32c, decode_sample_frame, ChannelDescriptor, ChannelTable, Frame, Message,
        ProtocolError, SampleBatch, WireFormat, MAX_BATCH_SAMPLES, MAX_PAYLOAD_LEN,
        MSG_SAMPLE_BATCH,
    },
    trigger::{TriggerCapture, TriggerConfig, TriggerEdge, TriggerEngine, TriggerMode},
};

const FILE_MAGIC: [u8; 8] = *b"SCOPEV1\0";
const FILE_VERSION: u16 = 1;
const FILE_HEADER_LEN: u16 = 32;
const RECORD_MAGIC: [u8; 4] = *b"REC1";
const RECORD_HEADER_LEN: u64 = 20;
const MAX_METADATA_LEN: usize = 1024 * 1024;
const MAX_RECORD_PAYLOAD: usize = 16 * 1024 * 1024;

const RECORD_SAMPLE_FRAME: u8 = 1;
const RECORD_GAP: u8 = 2;
const RECORD_TRIGGER: u8 = 3;
const RECORD_SESSION_END: u8 = 4;
const RECORD_INDEX: u8 = 5;
const TRIGGER_RECORD_VERSION: u8 = 1;
const TRIGGER_RECORD_LEN: usize = 48;
const RECORDING_QUEUE_CAPACITY: usize = 128;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordingMetadata {
    pub device_id: String,
    pub firmware_name: String,
    pub tick_hz: u64,
    pub channel_table: ChannelTable,
    pub sample_rate_hz: u32,
    pub batch_samples: u16,
    pub channel_mask: u64,
    pub client_version: String,
    #[serde(default)]
    pub channel_presentations: BTreeMap<u16, ChannelPresentation>,
}

impl RecordingMetadata {
    pub fn validate(&self) -> Result<(), RecordingError> {
        if self.tick_hz == 0 {
            return format_error("recording tick_hz must be greater than zero");
        }
        if self.sample_rate_hz == 0 || self.batch_samples == 0 || self.channel_mask == 0 {
            return format_error("recording acquisition parameters must be non-zero");
        }
        self.channel_table.validate()?;
        if self.channel_presentations.keys().any(|channel_id| {
            self.channel_table.channel(*channel_id).is_none()
                || self.channel_mask & (1_u64 << channel_id) == 0
        }) {
            return format_error(
                "recording channel presentations must reference selected table channels",
            );
        }
        if !self
            .channel_table
            .channels
            .iter()
            .any(|channel| self.channel_mask & (1_u64 << channel.channel_id) != 0)
        {
            return format_error("recording channel mask does not select a channel in the table");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleRecordIndex {
    pub payload_offset: u64,
    pub payload_len: u32,
    pub first_sample_index: u64,
    pub timestamp_ticks: u64,
    pub sample_period_ticks: u32,
    pub sample_count: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TriggerRecord {
    pub timestamp_ticks: u64,
    pub trigger_sample_index: u64,
    pub config: TriggerConfig,
    pub auto_timeout: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StoredIndexEntry {
    first_sample_index: u64,
    timestamp_ticks: u64,
    payload_offset: u64,
}

impl SampleRecordIndex {
    pub fn last_timestamp_ticks(&self) -> Result<u64, RecordingError> {
        let offset = u64::from(self.sample_period_ticks)
            .checked_mul(u64::from(self.sample_count.saturating_sub(1)))
            .ok_or_else(|| format_error_value("record timestamp offset overflow"))?;
        self.timestamp_ticks
            .checked_add(offset)
            .ok_or_else(|| format_error_value("record timestamp overflow"))
    }
}

#[derive(Debug, Error)]
pub enum RecordingError {
    #[error("scope recording I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("scope recording metadata error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("scope recording protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("invalid scope recording: {0}")]
    InvalidFormat(String),
    #[error("scope recording queue is full; recording stopped")]
    QueueFull,
    #[error("scope recording worker stopped unexpectedly")]
    WorkerStopped,
    #[error("scope recording worker failed: {0}")]
    WorkerFailed(String),
    #[error("scope recording worker panicked")]
    WorkerPanicked,
    #[error("scope recording write cancelled")]
    Cancelled,
}

pub struct ScopeWriter {
    file: Option<File>,
    metadata: RecordingMetadata,
    index: Vec<SampleRecordIndex>,
    finished: bool,
}

impl ScopeWriter {
    pub fn create(path: &Path, metadata: RecordingMetadata) -> Result<Self, RecordingError> {
        metadata.validate()?;
        let metadata_bytes = serde_json::to_vec(&metadata)?;
        if metadata_bytes.len() > MAX_METADATA_LEN {
            return format_error(format!(
                "metadata length {} exceeds {MAX_METADATA_LEN}",
                metadata_bytes.len()
            ));
        }
        let metadata_len = u32::try_from(metadata_bytes.len())
            .map_err(|_| format_error_value("metadata length does not fit u32"))?;
        let created_unix_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let created_unix_ns = u64::try_from(created_unix_ns).unwrap_or(u64::MAX);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)?;
        file.write_all(&FILE_MAGIC)?;
        file.write_all(&FILE_VERSION.to_le_bytes())?;
        file.write_all(&FILE_HEADER_LEN.to_le_bytes())?;
        file.write_all(&metadata_len.to_le_bytes())?;
        file.write_all(&created_unix_ns.to_le_bytes())?;
        file.write_all(&0_u32.to_le_bytes())?;
        file.write_all(&0_u32.to_le_bytes())?;
        file.write_all(&metadata_bytes)?;
        Ok(Self {
            file: Some(file),
            metadata,
            index: Vec::new(),
            finished: false,
        })
    }

    pub fn write_sample_frame(&mut self, frame: &Frame) -> Result<(), RecordingError> {
        let decoded = decode_sample_frame(frame, &self.metadata.channel_table)?;
        let encoded = frame.encode()?;
        let record_offset =
            self.write_record(RECORD_SAMPLE_FRAME, 0, frame.timestamp_ticks, &encoded)?;
        self.index.push(SampleRecordIndex {
            payload_offset: record_offset + RECORD_HEADER_LEN,
            payload_len: u32::try_from(encoded.len())
                .map_err(|_| format_error_value("sample frame length does not fit u32"))?,
            first_sample_index: decoded.first_sample_index,
            timestamp_ticks: decoded.timestamp_ticks,
            sample_period_ticks: decoded.sample_period_ticks,
            sample_count: u16::try_from(decoded.channels.first().map(Vec::len).unwrap_or(0))
                .map_err(|_| format_error_value("sample count does not fit u16"))?,
        });
        Ok(())
    }

    pub fn write_gap(&mut self, gap: LiveGap, timestamp_ticks: u64) -> Result<(), RecordingError> {
        let mut payload = Vec::with_capacity(17);
        payload.extend_from_slice(&gap.start_sample_index.to_le_bytes());
        payload.extend_from_slice(&gap.missing_samples.to_le_bytes());
        payload.push(gap_reason_code(gap.reason));
        self.write_record(RECORD_GAP, 0, timestamp_ticks, &payload)?;
        Ok(())
    }

    pub fn write_trigger(
        &mut self,
        capture: &TriggerCapture,
        config: &TriggerConfig,
    ) -> Result<(), RecordingError> {
        let trigger_index = capture
            .sample_indices
            .get(capture.trigger_position)
            .copied()
            .ok_or_else(|| format_error_value("trigger capture position is out of range"))?;
        let timestamp = capture
            .timestamps
            .get(capture.trigger_position)
            .copied()
            .ok_or_else(|| format_error_value("trigger timestamp is out of range"))?;
        if !capture.channel_ids.contains(&config.source_channel) {
            return format_error("trigger source channel is absent from capture");
        }
        if capture.auto_timeout && config.mode != TriggerMode::Auto {
            return format_error("non-Auto trigger capture is marked as an auto timeout");
        }
        TriggerEngine::new(config.clone())
            .map_err(|error| format_error_value(error.to_string()))?;
        let mut payload = Vec::with_capacity(TRIGGER_RECORD_LEN);
        payload.push(TRIGGER_RECORD_VERSION);
        payload.push(trigger_mode_code(config.mode));
        payload.push(trigger_edge_code(config.edge));
        payload.push(u8::from(capture.auto_timeout));
        payload.extend_from_slice(&config.source_channel.to_le_bytes());
        payload.extend_from_slice(&0_u16.to_le_bytes());
        payload.extend_from_slice(&trigger_index.to_le_bytes());
        payload.extend_from_slice(&config.level.to_le_bytes());
        payload.extend_from_slice(&config.hysteresis.to_le_bytes());
        payload.extend_from_slice(
            &u64::try_from(config.pre_samples)
                .map_err(|_| format_error_value("trigger pre_samples does not fit u64"))?
                .to_le_bytes(),
        );
        payload.extend_from_slice(
            &u64::try_from(config.post_samples)
                .map_err(|_| format_error_value("trigger post_samples does not fit u64"))?
                .to_le_bytes(),
        );
        payload.extend_from_slice(
            &u64::try_from(config.auto_timeout_samples)
                .map_err(|_| format_error_value("trigger auto timeout does not fit u64"))?
                .to_le_bytes(),
        );
        self.write_record(RECORD_TRIGGER, 0, timestamp, &payload)?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), RecordingError> {
        let index_payload = encode_index(&self.index)?;
        self.write_record(RECORD_INDEX, 0, 0, &index_payload)?;
        self.write_record(RECORD_SESSION_END, 0, 0, &[])?;
        if let Some(file) = &mut self.file {
            file.flush()?;
            file.sync_all()?;
        }
        self.finished = true;
        Ok(())
    }

    fn write_record(
        &mut self,
        record_type: u8,
        flags: u8,
        timestamp_ticks: u64,
        payload: &[u8],
    ) -> Result<u64, RecordingError> {
        if payload.len() > MAX_RECORD_PAYLOAD {
            return format_error(format!(
                "record payload length {} exceeds {MAX_RECORD_PAYLOAD}",
                payload.len()
            ));
        }
        let payload_len = u32::try_from(payload.len())
            .map_err(|_| format_error_value("record payload length does not fit u32"))?;
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| format_error_value("recording file is closed"))?;
        let offset = file.stream_position()?;
        let mut body = Vec::with_capacity(16 + payload.len());
        body.push(record_type);
        body.push(flags);
        body.extend_from_slice(&0_u16.to_le_bytes());
        body.extend_from_slice(&payload_len.to_le_bytes());
        body.extend_from_slice(&timestamp_ticks.to_le_bytes());
        body.extend_from_slice(payload);
        let checksum = crc32c(&body);
        file.write_all(&RECORD_MAGIC)?;
        file.write_all(&body)?;
        file.write_all(&checksum.to_le_bytes())?;
        Ok(offset)
    }
}

pub struct CaptureScopeContext<'a> {
    pub source_table: &'a ChannelTable,
    pub channel_presentations: &'a BTreeMap<u16, ChannelPresentation>,
    pub tick_hz: u64,
    pub sample_rate_hz: u32,
    pub client_version: &'a str,
}

pub fn write_capture_scope_file(
    path: &Path,
    capture: &TriggerCapture,
    config: &TriggerConfig,
    context: CaptureScopeContext<'_>,
) -> Result<(), RecordingError> {
    write_capture_scope_file_with_cancel(path, capture, config, context, || false)
}

pub fn write_capture_scope_file_with_cancel<F>(
    path: &Path,
    capture: &TriggerCapture,
    config: &TriggerConfig,
    context: CaptureScopeContext<'_>,
    mut is_cancelled: F,
) -> Result<(), RecordingError>
where
    F: FnMut() -> bool,
{
    let CaptureScopeContext {
        source_table,
        channel_presentations,
        tick_hz,
        sample_rate_hz,
        client_version,
    } = context;
    if is_cancelled() {
        return Err(RecordingError::Cancelled);
    }
    if path.exists() {
        return Ok(());
    }
    let sample_count = capture.sample_indices.len();
    if sample_count == 0
        || capture.timestamps.len() != sample_count
        || capture.channels.len() != capture.channel_ids.len()
        || capture
            .channels
            .iter()
            .any(|channel| channel.len() != sample_count)
    {
        return format_error("capture columns are empty or unaligned");
    }
    let channels = capture
        .channel_ids
        .iter()
        .map(|channel_id| {
            let source = source_table.channel(*channel_id).ok_or_else(|| {
                RecordingError::InvalidFormat(format!(
                    "capture channel {channel_id} is absent from the source table"
                ))
            })?;
            Ok(ChannelDescriptor {
                channel_id: *channel_id,
                kind: source.kind,
                wire_format: WireFormat::F32,
                scale: 1.0,
                offset: 0.0,
                unit: source.unit.clone(),
                name: source.name.clone(),
            })
        })
        .collect::<Result<Vec<_>, RecordingError>>()?;
    let channel_table = ChannelTable {
        revision: source_table.revision.max(1),
        channels,
    };
    let channel_mask = capture
        .channel_ids
        .iter()
        .fold(0_u64, |mask, channel_id| mask | (1_u64 << channel_id));
    let bytes_per_sample = capture
        .channel_ids
        .len()
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(|| RecordingError::InvalidFormat("capture size overflow".to_owned()))?;
    let fixed_payload = 20_usize + capture.channel_ids.len() * 2;
    let payload_batch_limit = MAX_PAYLOAD_LEN
        .saturating_sub(fixed_payload)
        .checked_div(bytes_per_sample)
        .unwrap_or(0);
    let batch_samples = MAX_BATCH_SAMPLES
        .min(payload_batch_limit)
        .min(usize::from(u16::MAX));
    if batch_samples == 0 {
        return format_error("capture channel set cannot fit in an SCP1 SampleBatch");
    }
    let metadata = RecordingMetadata {
        device_id: "scope-capture-asset".to_owned(),
        firmware_name: "Scope Analyzer".to_owned(),
        tick_hz,
        channel_table,
        sample_rate_hz,
        batch_samples: u16::try_from(batch_samples)
            .map_err(|_| RecordingError::InvalidFormat("batch size overflow".to_owned()))?,
        channel_mask,
        client_version: client_version.to_owned(),
        channel_presentations: channel_presentations
            .iter()
            .filter(|(channel_id, _)| capture.channel_ids.contains(channel_id))
            .map(|(channel_id, presentation)| (*channel_id, presentation.clone()))
            .collect(),
    };
    let temporary = path.with_extension("scope.tmp");
    let result = (|| {
        let mut writer = ScopeWriter::create(&temporary, metadata)?;
        let sample_period_ticks = capture
            .timestamps
            .windows(2)
            .next()
            .and_then(|pair| u32::try_from(pair[1].saturating_sub(pair[0])).ok())
            .filter(|period| *period > 0)
            .unwrap_or_else(|| {
                u32::try_from((tick_hz / u64::from(sample_rate_hz.max(1))).max(1))
                    .unwrap_or(u32::MAX)
            });
        for (sequence, start) in (0..sample_count).step_by(batch_samples).enumerate() {
            if is_cancelled() {
                return Err(RecordingError::Cancelled);
            }
            let end = (start + batch_samples).min(sample_count);
            let mut sample_data = Vec::with_capacity((end - start) * bytes_per_sample);
            for sample in start..end {
                for channel in &capture.channels {
                    sample_data.extend_from_slice(&channel[sample].to_le_bytes());
                }
            }
            let message = Message::SampleBatch(SampleBatch {
                channel_table_revision: source_table.revision.max(1),
                first_sample_index: capture.sample_indices[start],
                sample_period_ticks,
                sample_count: u16::try_from(end - start).map_err(|_| {
                    RecordingError::InvalidFormat("capture chunk is too large".to_owned())
                })?,
                channel_ids: capture.channel_ids.clone(),
                sample_data,
            });
            let frame = Frame::new(
                MSG_SAMPLE_BATCH,
                0,
                u32::try_from(sequence + 1).unwrap_or(u32::MAX),
                1,
                capture.timestamps[start],
                message.encode_payload()?,
            );
            writer.write_sample_frame(&frame)?;
        }
        if is_cancelled() {
            return Err(RecordingError::Cancelled);
        }
        writer.write_trigger(capture, config)?;
        writer.finish()
    })();
    match result {
        Ok(()) => {
            std::fs::rename(&temporary, path)?;
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(error)
        }
    }
}

pub struct LoadedCaptureAsset {
    pub capture: TriggerCapture,
    pub config: TriggerConfig,
    pub metadata: RecordingMetadata,
}

pub fn read_capture_scope_file(path: &Path) -> Result<LoadedCaptureAsset, RecordingError> {
    let recording = ScopeRecording::open(path)?;
    let trigger =
        recording.triggers().first().cloned().ok_or_else(|| {
            RecordingError::InvalidFormat("capture asset has no trigger".to_owned())
        })?;
    let mut channel_ids = Vec::new();
    let mut sample_indices = Vec::new();
    let mut timestamps = Vec::new();
    let mut channels: Vec<Vec<f32>> = Vec::new();
    for record in recording.sample_records() {
        let frame = recording.read_sample_frame(record)?;
        let decoded = decode_sample_frame(&frame, &recording.metadata().channel_table)?;
        if channel_ids.is_empty() {
            channel_ids = decoded.channel_ids.clone();
            channels = decoded.channels.iter().map(|_| Vec::new()).collect();
        } else if channel_ids != decoded.channel_ids {
            return format_error("capture asset changes channel order between batches");
        }
        for offset in 0..decoded.channels.first().map(Vec::len).unwrap_or(0) {
            sample_indices.push(
                decoded
                    .first_sample_index
                    .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX)),
            );
            timestamps.push(
                decoded.timestamp_ticks.saturating_add(
                    u64::from(decoded.sample_period_ticks)
                        .saturating_mul(u64::try_from(offset).unwrap_or(u64::MAX)),
                ),
            );
        }
        for (target, values) in channels.iter_mut().zip(decoded.channels) {
            target.extend(values);
        }
    }
    let trigger_position = sample_indices
        .iter()
        .position(|sample| *sample == trigger.trigger_sample_index)
        .ok_or_else(|| {
            RecordingError::InvalidFormat(
                "capture asset trigger sample is absent from sample data".to_owned(),
            )
        })?;
    Ok(LoadedCaptureAsset {
        capture: TriggerCapture {
            channel_ids,
            sample_indices,
            timestamps,
            channels,
            trigger_position,
            auto_timeout: trigger.auto_timeout,
        },
        config: trigger.config,
        metadata: recording.metadata().clone(),
    })
}

impl Drop for ScopeWriter {
    fn drop(&mut self) {
        if !self.finished {
            if let Some(file) = &mut self.file {
                let _ = file.flush();
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecordingStats {
    pub written_records: u64,
    pub sample_frames: u64,
    pub gap_records: u64,
    pub trigger_records: u64,
}

enum RecordingCommand {
    SampleFrame(Frame),
    Gap(LiveGap, u64),
    Trigger(TriggerCapture, TriggerConfig),
    Finish(Sender<Result<(), String>>),
    #[cfg(test)]
    Pause(Sender<()>, Receiver<()>),
    #[cfg(test)]
    FailForTest,
}

pub struct AsyncScopeRecorder {
    command_tx: Option<Sender<RecordingCommand>>,
    error_rx: Receiver<String>,
    stats: Arc<Mutex<RecordingStats>>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub struct RecordingIngress {
    command_tx: Sender<RecordingCommand>,
}

impl RecordingIngress {
    pub fn try_write_sample_frame(&self, frame: Frame) -> Result<(), RecordingError> {
        self.try_send(RecordingCommand::SampleFrame(frame))
    }

    pub fn try_write_gap(&self, gap: LiveGap, timestamp_ticks: u64) -> Result<(), RecordingError> {
        self.try_send(RecordingCommand::Gap(gap, timestamp_ticks))
    }

    fn try_send(&self, command: RecordingCommand) -> Result<(), RecordingError> {
        match self.command_tx.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(RecordingError::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(RecordingError::WorkerStopped),
        }
    }
}

impl AsyncScopeRecorder {
    pub fn create(path: &Path, metadata: RecordingMetadata) -> Result<Self, RecordingError> {
        Self::create_with_capacity(path, metadata, RECORDING_QUEUE_CAPACITY)
    }

    fn create_with_capacity(
        path: &Path,
        metadata: RecordingMetadata,
        capacity: usize,
    ) -> Result<Self, RecordingError> {
        if capacity == 0 {
            return format_error("recording queue capacity must be greater than zero");
        }
        let writer = ScopeWriter::create(path, metadata)?;
        let (command_tx, command_rx) = bounded(capacity);
        let (error_tx, error_rx) = bounded(1);
        let stats = Arc::new(Mutex::new(RecordingStats::default()));
        let worker_stats = Arc::clone(&stats);
        let worker = thread::Builder::new()
            .name("scope-recording-writer".to_owned())
            .spawn(move || recording_worker(writer, command_rx, error_tx, worker_stats))?;
        Ok(Self {
            command_tx: Some(command_tx),
            error_rx,
            stats,
            worker: Some(worker),
        })
    }

    pub fn try_write_sample_frame(&mut self, frame: Frame) -> Result<(), RecordingError> {
        self.try_send(RecordingCommand::SampleFrame(frame))
    }

    pub fn ingress(&self) -> Result<RecordingIngress, RecordingError> {
        Ok(RecordingIngress {
            command_tx: self
                .command_tx
                .as_ref()
                .ok_or(RecordingError::WorkerStopped)?
                .clone(),
        })
    }

    pub fn try_write_gap(
        &mut self,
        gap: LiveGap,
        timestamp_ticks: u64,
    ) -> Result<(), RecordingError> {
        self.try_send(RecordingCommand::Gap(gap, timestamp_ticks))
    }

    pub fn try_write_trigger(
        &mut self,
        capture: TriggerCapture,
        config: TriggerConfig,
    ) -> Result<(), RecordingError> {
        self.try_send(RecordingCommand::Trigger(capture, config))
    }

    pub fn poll_error(&mut self) -> Option<RecordingError> {
        match self.error_rx.try_recv() {
            Ok(error) => {
                self.command_tx.take();
                Some(RecordingError::WorkerFailed(error))
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) if self.worker_finished() => {
                self.command_tx.take();
                Some(RecordingError::WorkerStopped)
            }
            Err(_) => None,
        }
    }

    pub fn is_accepting(&self) -> bool {
        self.command_tx.is_some() && !self.worker_finished()
    }

    pub fn pending_records(&self) -> usize {
        self.command_tx.as_ref().map(Sender::len).unwrap_or(0)
    }

    pub fn stats(&self) -> RecordingStats {
        self.stats.lock().map(|stats| *stats).unwrap_or_default()
    }

    pub fn finish(mut self) -> Result<RecordingStats, RecordingError> {
        let command_tx = self
            .command_tx
            .take()
            .ok_or(RecordingError::WorkerStopped)?;
        let (result_tx, result_rx) = bounded(1);
        command_tx
            .send(RecordingCommand::Finish(result_tx))
            .map_err(|_| self.worker_error_or_stopped())?;
        drop(command_tx);
        result_rx
            .recv()
            .map_err(|_| self.worker_error_or_stopped())?
            .map_err(RecordingError::WorkerFailed)?;
        self.join_worker()?;
        Ok(self.stats())
    }

    pub fn abort(mut self) -> Result<(), RecordingError> {
        self.command_tx.take();
        self.join_worker()
    }

    fn try_send(&mut self, command: RecordingCommand) -> Result<(), RecordingError> {
        if let Some(error) = self.poll_error() {
            return Err(error);
        }
        let sender = self
            .command_tx
            .as_ref()
            .ok_or(RecordingError::WorkerStopped)?;
        match sender.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.command_tx.take();
                Err(RecordingError::QueueFull)
            }
            Err(TrySendError::Disconnected(_)) => {
                self.command_tx.take();
                Err(self.worker_error_or_stopped())
            }
        }
    }

    fn worker_finished(&self) -> bool {
        self.worker.as_ref().is_some_and(JoinHandle::is_finished)
    }

    fn worker_error_or_stopped(&self) -> RecordingError {
        self.error_rx
            .try_recv()
            .map(RecordingError::WorkerFailed)
            .unwrap_or(RecordingError::WorkerStopped)
    }

    fn join_worker(&mut self) -> Result<(), RecordingError> {
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| RecordingError::WorkerPanicked)?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn pause_worker_for_test(&self) -> Sender<()> {
        let (ready_tx, ready_rx) = bounded(0);
        let (release_tx, release_rx) = bounded(0);
        self.command_tx
            .as_ref()
            .expect("recorder accepts commands")
            .send(RecordingCommand::Pause(ready_tx, release_rx))
            .unwrap();
        ready_rx.recv().unwrap();
        release_tx
    }

    #[cfg(test)]
    fn fail_worker_for_test(&self) {
        self.command_tx
            .as_ref()
            .expect("recorder accepts commands")
            .send(RecordingCommand::FailForTest)
            .unwrap();
    }
}

impl Drop for AsyncScopeRecorder {
    fn drop(&mut self) {
        self.command_tx.take();
        // Dropping a JoinHandle detaches the worker. This keeps application shutdown bounded;
        // the worker owns the file and flushes its recoverable prefix when it observes disconnect.
        self.worker.take();
    }
}

fn recording_worker(
    mut writer: ScopeWriter,
    command_rx: Receiver<RecordingCommand>,
    error_tx: Sender<String>,
    stats: Arc<Mutex<RecordingStats>>,
) {
    while let Ok(command) = command_rx.recv() {
        let result = match command {
            RecordingCommand::SampleFrame(frame) => writer.write_sample_frame(&frame).map(|()| 1),
            RecordingCommand::Gap(gap, timestamp_ticks) => {
                writer.write_gap(gap, timestamp_ticks).map(|()| 2)
            }
            RecordingCommand::Trigger(capture, config) => {
                writer.write_trigger(&capture, &config).map(|()| 3)
            }
            RecordingCommand::Finish(result_tx) => {
                let result = writer.finish().map_err(|error| error.to_string());
                let _ = result_tx.send(result);
                return;
            }
            #[cfg(test)]
            RecordingCommand::Pause(ready_tx, release_rx) => {
                let _ = ready_tx.send(());
                let _ = release_rx.recv();
                continue;
            }
            #[cfg(test)]
            RecordingCommand::FailForTest => {
                let _ = error_tx.send("injected writer failure".to_owned());
                return;
            }
        };
        match result {
            Ok(record_type) => {
                if let Ok(mut stats) = stats.lock() {
                    stats.written_records = stats.written_records.saturating_add(1);
                    match record_type {
                        1 => stats.sample_frames = stats.sample_frames.saturating_add(1),
                        2 => stats.gap_records = stats.gap_records.saturating_add(1),
                        3 => stats.trigger_records = stats.trigger_records.saturating_add(1),
                        _ => {}
                    }
                }
            }
            Err(error) => {
                let _ = error_tx.send(error.to_string());
                return;
            }
        }
    }
}

pub struct ScopeRecording {
    path: PathBuf,
    metadata: RecordingMetadata,
    sample_records: Vec<SampleRecordIndex>,
    gaps: Vec<LiveGap>,
    triggers: Vec<TriggerRecord>,
    clean_end: bool,
    recovered_tail: bool,
}

impl ScopeRecording {
    pub fn open(path: &Path) -> Result<Self, RecordingError> {
        let mut file = File::open(path)?;
        let metadata = read_file_header(&mut file)?;
        let mut sample_records = Vec::new();
        let mut gaps = Vec::new();
        let mut triggers = Vec::new();
        let mut stored_index: Option<Vec<StoredIndexEntry>> = None;
        let mut clean_end = false;
        let mut recovered_tail = false;
        loop {
            let record_offset = file.stream_position()?;
            let mut magic = [0_u8; 4];
            match read_partial(&mut file, &mut magic)? {
                0 => {
                    recovered_tail = !clean_end;
                    break;
                }
                4 => {}
                _ => {
                    recovered_tail = true;
                    break;
                }
            }
            if magic != RECORD_MAGIC {
                return format_error(format!("invalid record magic at offset {record_offset}"));
            }
            let mut header = [0_u8; 16];
            if read_partial(&mut file, &mut header)? != header.len() {
                recovered_tail = true;
                break;
            }
            let record_type = header[0];
            let flags = header[1];
            let reserved = u16::from_le_bytes(header[2..4].try_into().expect("fixed slice"));
            let payload_len =
                u32::from_le_bytes(header[4..8].try_into().expect("fixed slice")) as usize;
            let timestamp_ticks =
                u64::from_le_bytes(header[8..16].try_into().expect("fixed slice"));
            if flags != 0 || reserved != 0 {
                return format_error(format!(
                    "unsupported record flags at offset {record_offset}"
                ));
            }
            if payload_len > MAX_RECORD_PAYLOAD {
                return format_error(format!(
                    "record payload at offset {record_offset} exceeds {MAX_RECORD_PAYLOAD}"
                ));
            }
            let mut payload = vec![0_u8; payload_len];
            if read_partial(&mut file, &mut payload)? != payload_len {
                recovered_tail = true;
                break;
            }
            let mut checksum = [0_u8; 4];
            if read_partial(&mut file, &mut checksum)? != checksum.len() {
                recovered_tail = true;
                break;
            }
            let mut body = Vec::with_capacity(header.len() + payload.len());
            body.extend_from_slice(&header);
            body.extend_from_slice(&payload);
            let expected_crc = u32::from_le_bytes(checksum);
            let actual_crc = crc32c(&body);
            if expected_crc != actual_crc {
                return format_error(format!("record CRC mismatch at offset {record_offset}"));
            }
            match record_type {
                RECORD_SAMPLE_FRAME => {
                    let frame = Frame::decode(&payload)?;
                    let decoded = decode_sample_frame(&frame, &metadata.channel_table)?;
                    sample_records.push(SampleRecordIndex {
                        payload_offset: record_offset + RECORD_HEADER_LEN,
                        payload_len: u32::try_from(payload_len).map_err(|_| {
                            format_error_value("sample record payload length does not fit u32")
                        })?,
                        first_sample_index: decoded.first_sample_index,
                        timestamp_ticks: decoded.timestamp_ticks,
                        sample_period_ticks: decoded.sample_period_ticks,
                        sample_count: u16::try_from(
                            decoded.channels.first().map(Vec::len).unwrap_or(0),
                        )
                        .map_err(|_| format_error_value("sample count does not fit u16"))?,
                    });
                }
                RECORD_GAP => gaps.push(decode_gap(&payload)?),
                RECORD_TRIGGER => triggers.push(decode_trigger_record(timestamp_ticks, &payload)?),
                RECORD_INDEX => {
                    if stored_index.is_some() {
                        return format_error("duplicate index record");
                    }
                    stored_index = Some(decode_index(&payload)?);
                }
                RECORD_SESSION_END => {
                    if !payload.is_empty() || timestamp_ticks != 0 {
                        return format_error("invalid SessionEnd record");
                    }
                    let index = stored_index
                        .as_deref()
                        .ok_or_else(|| format_error_value("SessionEnd is missing index record"))?;
                    validate_index_matches_records(index, &sample_records)?;
                    clean_end = true;
                    break;
                }
                _ => {}
            }
        }
        Ok(Self {
            path: path.to_path_buf(),
            metadata,
            sample_records,
            gaps,
            triggers,
            clean_end,
            recovered_tail,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn metadata(&self) -> &RecordingMetadata {
        &self.metadata
    }

    pub fn sample_records(&self) -> &[SampleRecordIndex] {
        &self.sample_records
    }

    pub fn gaps(&self) -> &[LiveGap] {
        &self.gaps
    }

    pub fn triggers(&self) -> &[TriggerRecord] {
        &self.triggers
    }

    pub fn clean_end(&self) -> bool {
        self.clean_end
    }

    pub fn recovered_tail(&self) -> bool {
        self.recovered_tail
    }

    pub fn read_sample_frame(&self, record: &SampleRecordIndex) -> Result<Frame, RecordingError> {
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(record.payload_offset))?;
        let mut payload = vec![0_u8; record.payload_len as usize];
        file.read_exact(&mut payload)?;
        Ok(Frame::decode(&payload)?)
    }
}

fn read_file_header(file: &mut File) -> Result<RecordingMetadata, RecordingError> {
    let mut header = [0_u8; FILE_HEADER_LEN as usize];
    file.read_exact(&mut header)?;
    if header[..8] != FILE_MAGIC {
        return format_error("invalid .scope file magic");
    }
    let version = u16::from_le_bytes(header[8..10].try_into().expect("fixed slice"));
    let header_len = u16::from_le_bytes(header[10..12].try_into().expect("fixed slice"));
    let metadata_len = u32::from_le_bytes(header[12..16].try_into().expect("fixed slice")) as usize;
    let flags = u32::from_le_bytes(header[24..28].try_into().expect("fixed slice"));
    let reserved = u32::from_le_bytes(header[28..32].try_into().expect("fixed slice"));
    if version != FILE_VERSION || header_len != FILE_HEADER_LEN || flags != 0 || reserved != 0 {
        return format_error("unsupported .scope file header");
    }
    if metadata_len > MAX_METADATA_LEN {
        return format_error(format!(
            "metadata length {metadata_len} exceeds {MAX_METADATA_LEN}"
        ));
    }
    let mut metadata_bytes = vec![0_u8; metadata_len];
    file.read_exact(&mut metadata_bytes)?;
    let metadata: RecordingMetadata = serde_json::from_slice(&metadata_bytes)?;
    metadata.validate()?;
    Ok(metadata)
}

fn read_partial(reader: &mut impl Read, bytes: &mut [u8]) -> Result<usize, std::io::Error> {
    let mut read = 0;
    while read < bytes.len() {
        match reader.read(&mut bytes[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(read)
}

fn encode_index(index: &[SampleRecordIndex]) -> Result<Vec<u8>, RecordingError> {
    let capacity = index
        .len()
        .checked_mul(24)
        .and_then(|len| len.checked_add(4))
        .ok_or_else(|| format_error_value("index length overflow"))?;
    let mut payload = Vec::with_capacity(capacity);
    payload.extend_from_slice(
        &u32::try_from(index.len())
            .map_err(|_| format_error_value("index entry count does not fit u32"))?
            .to_le_bytes(),
    );
    for entry in index {
        payload.extend_from_slice(&entry.first_sample_index.to_le_bytes());
        payload.extend_from_slice(&entry.timestamp_ticks.to_le_bytes());
        payload.extend_from_slice(&entry.payload_offset.to_le_bytes());
    }
    Ok(payload)
}

fn decode_index(payload: &[u8]) -> Result<Vec<StoredIndexEntry>, RecordingError> {
    if payload.len() < 4 {
        return format_error("truncated index record");
    }
    let count = u32::from_le_bytes(payload[..4].try_into().expect("fixed slice")) as usize;
    let expected = count
        .checked_mul(24)
        .and_then(|len| len.checked_add(4))
        .ok_or_else(|| format_error_value("index length overflow"))?;
    if payload.len() != expected {
        return format_error("index record length mismatch");
    }
    let mut entries = Vec::with_capacity(count);
    for bytes in payload[4..].chunks_exact(24) {
        entries.push(StoredIndexEntry {
            first_sample_index: u64::from_le_bytes(bytes[0..8].try_into().expect("fixed slice")),
            timestamp_ticks: u64::from_le_bytes(bytes[8..16].try_into().expect("fixed slice")),
            payload_offset: u64::from_le_bytes(bytes[16..24].try_into().expect("fixed slice")),
        });
    }
    Ok(entries)
}

fn validate_index_matches_records(
    index: &[StoredIndexEntry],
    records: &[SampleRecordIndex],
) -> Result<(), RecordingError> {
    if index.len() != records.len() {
        return format_error("index entry count does not match sample records");
    }
    for (entry, record) in index.iter().zip(records) {
        if entry.first_sample_index != record.first_sample_index
            || entry.timestamp_ticks != record.timestamp_ticks
            || entry.payload_offset != record.payload_offset
        {
            return format_error("index entry does not match sample record");
        }
    }
    Ok(())
}

fn decode_gap(payload: &[u8]) -> Result<LiveGap, RecordingError> {
    if payload.len() != 17 {
        return format_error("gap record length mismatch");
    }
    let start_sample_index = u64::from_le_bytes(payload[..8].try_into().expect("fixed slice"));
    let missing_samples = u64::from_le_bytes(payload[8..16].try_into().expect("fixed slice"));
    if missing_samples == 0 {
        return format_error("gap record has zero missing samples");
    }
    let reason = match payload[16] {
        1 => GapReason::SequenceLoss,
        2 => GapReason::SampleIndexLoss,
        3 => GapReason::HostBackpressure,
        4 => GapReason::DeviceReported,
        value => return format_error(format!("unknown gap reason {value}")),
    };
    Ok(LiveGap {
        start_sample_index,
        missing_samples,
        reason,
    })
}

fn decode_trigger_record(
    timestamp_ticks: u64,
    payload: &[u8],
) -> Result<TriggerRecord, RecordingError> {
    if payload.len() != TRIGGER_RECORD_LEN
        || payload[0] != TRIGGER_RECORD_VERSION
        || payload[3] > 1
        || payload[6..8] != [0, 0]
    {
        return format_error("invalid trigger record");
    }
    let config = TriggerConfig {
        mode: decode_trigger_mode(payload[1])?,
        edge: decode_trigger_edge(payload[2])?,
        source_channel: u16::from_le_bytes(payload[4..6].try_into().expect("fixed slice")),
        level: f32::from_le_bytes(payload[16..20].try_into().expect("fixed slice")),
        hysteresis: f32::from_le_bytes(payload[20..24].try_into().expect("fixed slice")),
        pre_samples: usize::try_from(u64::from_le_bytes(
            payload[24..32].try_into().expect("fixed slice"),
        ))
        .map_err(|_| format_error_value("trigger pre_samples does not fit usize"))?,
        post_samples: usize::try_from(u64::from_le_bytes(
            payload[32..40].try_into().expect("fixed slice"),
        ))
        .map_err(|_| format_error_value("trigger post_samples does not fit usize"))?,
        auto_timeout_samples: usize::try_from(u64::from_le_bytes(
            payload[40..48].try_into().expect("fixed slice"),
        ))
        .map_err(|_| format_error_value("trigger auto timeout does not fit usize"))?,
    };
    TriggerEngine::new(config.clone()).map_err(|error| format_error_value(error.to_string()))?;
    let auto_timeout = payload[3] != 0;
    if auto_timeout && config.mode != TriggerMode::Auto {
        return format_error("non-Auto trigger record is marked as an auto timeout");
    }
    Ok(TriggerRecord {
        timestamp_ticks,
        trigger_sample_index: u64::from_le_bytes(payload[8..16].try_into().expect("fixed slice")),
        config,
        auto_timeout,
    })
}

fn trigger_mode_code(mode: TriggerMode) -> u8 {
    match mode {
        TriggerMode::Auto => 0,
        TriggerMode::Normal => 1,
        TriggerMode::Single => 2,
    }
}

fn decode_trigger_mode(value: u8) -> Result<TriggerMode, RecordingError> {
    match value {
        0 => Ok(TriggerMode::Auto),
        1 => Ok(TriggerMode::Normal),
        2 => Ok(TriggerMode::Single),
        _ => format_error(format!("unknown trigger mode {value}")),
    }
}

fn trigger_edge_code(edge: TriggerEdge) -> u8 {
    match edge {
        TriggerEdge::Rising => 0,
        TriggerEdge::Falling => 1,
        TriggerEdge::Either => 2,
    }
}

fn decode_trigger_edge(value: u8) -> Result<TriggerEdge, RecordingError> {
    match value {
        0 => Ok(TriggerEdge::Rising),
        1 => Ok(TriggerEdge::Falling),
        2 => Ok(TriggerEdge::Either),
        _ => format_error(format!("unknown trigger edge {value}")),
    }
}

fn gap_reason_code(reason: GapReason) -> u8 {
    match reason {
        GapReason::SequenceLoss => 1,
        GapReason::SampleIndexLoss => 2,
        GapReason::HostBackpressure => 3,
        GapReason::DeviceReported => 4,
    }
}

fn format_error<T>(message: impl Into<String>) -> Result<T, RecordingError> {
    Err(format_error_value(message))
}

fn format_error_value(message: impl Into<String>) -> RecordingError {
    RecordingError::InvalidFormat(message.into())
}

#[cfg(test)]
mod tests {
    use std::{
        fs::OpenOptions,
        io::{Read, Seek, SeekFrom, Write},
        path::PathBuf,
    };

    use super::*;
    use crate::{
        data::{DataCancelToken, DataSource},
        live::{
            protocol::{
                ChannelDescriptor, ChannelKind, ChannelTable, Frame, Message, SampleBatch,
                WireFormat, MSG_SAMPLE_BATCH,
            },
            scope_source::ScopeRecordingDataSource,
            trigger::{TriggerCapture, TriggerConfig, TriggerEdge, TriggerMode},
        },
    };

    fn unique_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "scope_live_{name}_{}_{}.scope",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn metadata() -> RecordingMetadata {
        let mut channel_presentations = BTreeMap::new();
        channel_presentations.insert(
            0,
            ChannelPresentation {
                display_name: "Phase A".to_owned(),
                color: [12, 34, 56, 255],
                visible: true,
                scale: 2.5,
                pane: 1,
            },
        );
        RecordingMetadata {
            device_id: "sim-1".to_owned(),
            firmware_name: "scope-sim".to_owned(),
            tick_hz: 1_000,
            channel_table: ChannelTable {
                revision: 1,
                channels: vec![ChannelDescriptor {
                    channel_id: 0,
                    kind: ChannelKind::Analog,
                    wire_format: WireFormat::I16,
                    scale: 1.0,
                    offset: 0.0,
                    unit: "V".to_owned(),
                    name: "Ua".to_owned(),
                }],
            },
            sample_rate_hz: 10,
            batch_samples: 2,
            channel_mask: 1,
            client_version: "0.8.0-test".to_owned(),
            channel_presentations,
        }
    }

    fn sample_frame(sequence: u32, first: u64, timestamp: u64, values: &[i16]) -> Frame {
        let mut sample_data = Vec::new();
        for value in values {
            sample_data.extend_from_slice(&value.to_le_bytes());
        }
        let message = Message::SampleBatch(SampleBatch {
            channel_table_revision: 1,
            first_sample_index: first,
            sample_period_ticks: 100,
            sample_count: values.len() as u16,
            channel_ids: vec![0],
            sample_data,
        });
        Frame::new(
            MSG_SAMPLE_BATCH,
            0,
            sequence,
            7,
            timestamp,
            message.encode_payload().unwrap(),
        )
    }

    fn append_raw_record(path: &Path, record_type: u8, timestamp_ticks: u64, payload: &[u8]) {
        let mut header = Vec::with_capacity(16);
        header.push(record_type);
        header.push(0);
        header.extend_from_slice(&0_u16.to_le_bytes());
        header.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        header.extend_from_slice(&timestamp_ticks.to_le_bytes());
        let mut body = header.clone();
        body.extend_from_slice(payload);
        let checksum = crc32c(&body);
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(&RECORD_MAGIC).unwrap();
        file.write_all(&body).unwrap();
        file.write_all(&checksum.to_le_bytes()).unwrap();
        file.flush().unwrap();
    }

    fn trigger_capture() -> TriggerCapture {
        TriggerCapture {
            channel_ids: vec![0],
            sample_indices: vec![10, 11, 12],
            timestamps: vec![1_000, 1_100, 1_200],
            channels: vec![vec![-1.0, 0.5, 1.0]],
            trigger_position: 1,
            auto_timeout: false,
        }
    }

    fn trigger_config() -> TriggerConfig {
        TriggerConfig {
            mode: TriggerMode::Single,
            edge: TriggerEdge::Rising,
            source_channel: 0,
            level: 0.25,
            hysteresis: 0.1,
            pre_samples: 1,
            post_samples: 1,
            auto_timeout_samples: 500,
        }
    }

    #[test]
    fn recording_round_trip_and_data_source_read() {
        let path = unique_path("round_trip");
        let mut writer = ScopeWriter::create(&path, metadata()).unwrap();
        writer
            .write_sample_frame(&sample_frame(1, 0, 0, &[1, 2]))
            .unwrap();
        writer
            .write_sample_frame(&sample_frame(2, 2, 200, &[3, 4]))
            .unwrap();
        writer.finish().unwrap();

        let recording = ScopeRecording::open(&path).unwrap();
        assert!(recording.clean_end());
        assert!(!recording.recovered_tail());
        assert_eq!(recording.sample_records().len(), 2);

        let source = ScopeRecordingDataSource::open(&path).unwrap();
        assert_eq!(source.metadata().sample_count, 4);
        assert_eq!(source.metadata().channels[0].name, "Ua");
        assert_eq!(
            source.channel_presentation(0),
            Some(ChannelPresentation {
                display_name: "Phase A".to_owned(),
                color: [12, 34, 56, 255],
                visible: true,
                scale: 2.5,
                pane: 1,
            })
        );
        let block = source.read_range(0.0, 0.3, &[0], 10).unwrap();
        assert_eq!(block.channels[0], vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(block.times, vec![0.0, 0.1, 0.2, 0.3]);
        let decimated = source.read_range(0.0, 0.3, &[0], 2).unwrap();
        assert_eq!(decimated.channels[0], vec![1.0, 4.0]);
        let summary = source.summarize_range(0.0, 0.3, &[0], 2).unwrap();
        assert_eq!(summary.min[0], vec![1.0, 3.0]);
        assert_eq!(summary.max[0], vec![2.0, 4.0]);
        let cancel = DataCancelToken::new();
        cancel.cancel();
        assert!(source
            .read_range_cancellable(0.0, 0.3, &[0], 10, &cancel)
            .is_err());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn companion_capture_writer_creates_replayable_scope_v1() {
        let path = unique_path("capture_asset");
        let metadata = metadata();
        write_capture_scope_file(
            &path,
            &trigger_capture(),
            &trigger_config(),
            CaptureScopeContext {
                source_table: &metadata.channel_table,
                channel_presentations: &metadata.channel_presentations,
                tick_hz: metadata.tick_hz,
                sample_rate_hz: metadata.sample_rate_hz,
                client_version: "0.11.0-test",
            },
        )
        .unwrap();

        let recording = ScopeRecording::open(&path).unwrap();
        assert!(recording.clean_end());
        assert_eq!(recording.triggers().len(), 1);
        assert_eq!(recording.sample_records().len(), 1);
        let loaded = read_capture_scope_file(&path).unwrap();
        assert_eq!(loaded.capture, trigger_capture());
        assert_eq!(loaded.config, trigger_config());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cancelled_companion_capture_write_removes_partial_asset() {
        let path = unique_path("cancelled_capture_asset");
        let metadata = metadata();
        let sample_count = 10_000;
        let capture = TriggerCapture {
            channel_ids: vec![0],
            sample_indices: (0..sample_count as u64).collect(),
            timestamps: (0..sample_count as u64)
                .map(|sample| sample * 100)
                .collect(),
            channels: vec![vec![1.0; sample_count]],
            trigger_position: sample_count / 2,
            auto_timeout: false,
        };
        let mut cancellation_checks = 0;
        let error = write_capture_scope_file_with_cancel(
            &path,
            &capture,
            &trigger_config(),
            CaptureScopeContext {
                source_table: &metadata.channel_table,
                channel_presentations: &metadata.channel_presentations,
                tick_hz: metadata.tick_hz,
                sample_rate_hz: metadata.sample_rate_hz,
                client_version: "0.11.0-test",
            },
            || {
                cancellation_checks += 1;
                cancellation_checks >= 3
            },
        )
        .unwrap_err();

        assert!(matches!(error, RecordingError::Cancelled));
        assert!(!path.exists());
        assert!(!path.with_extension("scope.tmp").exists());
    }

    #[test]
    fn scope_data_source_keeps_sample_index_gaps_segmented_for_plot_and_export() {
        let path = unique_path("segmented_gap");
        let mut writer = ScopeWriter::create(&path, metadata()).unwrap();
        writer
            .write_sample_frame(&sample_frame(1, 0, 0, &[1, 2]))
            .unwrap();
        writer
            .write_sample_frame(&sample_frame(2, 4, 400, &[5, 6]))
            .unwrap();
        writer.finish().unwrap();

        let source = ScopeRecordingDataSource::open(&path).unwrap();
        let segments = source.read_range_segments(0.0, 0.5, &[0], 10).unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].times, vec![0.0, 0.1]);
        assert_eq!(segments[1].times, vec![0.4, 0.5]);
        assert_eq!(segments[0].channels[0], vec![1.0, 2.0]);
        assert_eq!(segments[1].channels[0], vec![5.0, 6.0]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn legacy_recording_metadata_without_presentations_remains_readable() {
        let mut value = serde_json::to_value(metadata()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("channel_presentations");
        let decoded: RecordingMetadata = serde_json::from_value(value).unwrap();
        assert!(decoded.channel_presentations.is_empty());
        decoded.validate().unwrap();
    }

    #[test]
    fn scope_data_source_preserves_requested_order_for_reordered_wire_channels() {
        let path = unique_path("channel_order");
        let mut recording_metadata = metadata();
        recording_metadata
            .channel_table
            .channels
            .push(ChannelDescriptor {
                channel_id: 2,
                kind: ChannelKind::Analog,
                wire_format: WireFormat::I16,
                scale: 0.1,
                offset: 0.0,
                unit: "A".to_owned(),
                name: "Ib".to_owned(),
            });
        recording_metadata.channel_mask = 0b101;
        recording_metadata.batch_samples = 3;
        let mut sample_data = Vec::new();
        for (id_2, id_0) in [(20_i16, 1_i16), (40, 3), (60, 5)] {
            sample_data.extend_from_slice(&id_2.to_le_bytes());
            sample_data.extend_from_slice(&id_0.to_le_bytes());
        }
        let message = Message::SampleBatch(SampleBatch {
            channel_table_revision: 1,
            first_sample_index: 0,
            sample_period_ticks: 100,
            sample_count: 3,
            channel_ids: vec![2, 0],
            sample_data,
        });
        let frame = Frame::new(
            MSG_SAMPLE_BATCH,
            0,
            1,
            7,
            0,
            message.encode_payload().unwrap(),
        );
        let mut writer = ScopeWriter::create(&path, recording_metadata).unwrap();
        writer.write_sample_frame(&frame).unwrap();
        writer.finish().unwrap();

        let source = ScopeRecordingDataSource::open(&path).unwrap();
        assert_eq!(
            source
                .metadata()
                .channels
                .iter()
                .map(|channel| channel.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Ua", "Ib"]
        );
        let block = source.read_range(0.0, 0.2, &[1, 0], 10).unwrap();
        assert_eq!(block.channels[0], vec![2.0, 4.0, 6.0]);
        assert_eq!(block.channels[1], vec![1.0, 3.0, 5.0]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn recording_recovers_a_truncated_final_record() {
        let path = unique_path("truncated");
        let mut writer = ScopeWriter::create(&path, metadata()).unwrap();
        writer
            .write_sample_frame(&sample_frame(1, 0, 0, &[1, 2]))
            .unwrap();
        writer.finish().unwrap();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let len = file.seek(SeekFrom::End(0)).unwrap();
        file.set_len(len - 5).unwrap();

        let recording = ScopeRecording::open(&path).unwrap();

        assert!(!recording.clean_end());
        assert!(recording.recovered_tail());
        assert_eq!(recording.sample_records().len(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn recording_rejects_middle_record_crc_corruption() {
        let path = unique_path("corrupt");
        let mut writer = ScopeWriter::create(&path, metadata()).unwrap();
        writer
            .write_sample_frame(&sample_frame(1, 0, 0, &[1, 2]))
            .unwrap();
        writer
            .write_sample_frame(&sample_frame(2, 2, 200, &[3, 4]))
            .unwrap();
        writer.finish().unwrap();
        let recording = ScopeRecording::open(&path).unwrap();
        let corrupt_offset = recording.sample_records()[0].payload_offset + 28;
        drop(recording);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(corrupt_offset)).unwrap();
        let mut byte = [0_u8; 1];
        file.read_exact(&mut byte).unwrap();
        byte[0] ^= 0x55;
        file.seek(SeekFrom::Start(corrupt_offset)).unwrap();
        file.write_all(&byte).unwrap();
        file.flush().unwrap();

        assert!(matches!(
            ScopeRecording::open(&path),
            Err(RecordingError::InvalidFormat(message)) if message.contains("CRC")
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn recording_rejects_oversized_metadata_before_allocating_it() {
        let path = unique_path("oversized_metadata");
        let mut file = File::create(&path).unwrap();
        file.write_all(&FILE_MAGIC).unwrap();
        file.write_all(&FILE_VERSION.to_le_bytes()).unwrap();
        file.write_all(&FILE_HEADER_LEN.to_le_bytes()).unwrap();
        file.write_all(&((MAX_METADATA_LEN + 1) as u32).to_le_bytes())
            .unwrap();
        file.write_all(&0_u64.to_le_bytes()).unwrap();
        file.write_all(&0_u32.to_le_bytes()).unwrap();
        file.write_all(&0_u32.to_le_bytes()).unwrap();
        drop(file);

        assert!(matches!(
            ScopeRecording::open(&path),
            Err(RecordingError::InvalidFormat(message)) if message.contains("metadata length")
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn recording_rejects_invalid_embedded_sample_batch_with_valid_crcs() {
        let path = unique_path("invalid_sample_batch");
        drop(ScopeWriter::create(&path, metadata()).unwrap());
        let mut frame = sample_frame(1, 0, 0, &[1, 2]);
        frame.payload[16..18].copy_from_slice(&3_u16.to_le_bytes());
        let encoded = frame.encode().unwrap();
        append_raw_record(&path, RECORD_SAMPLE_FRAME, 0, &encoded);

        assert!(matches!(
            ScopeRecording::open(&path),
            Err(RecordingError::Protocol(ProtocolError::InvalidPayload(message)))
                if message.contains("sample data length mismatch")
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn recording_persists_complete_trigger_configuration() {
        let path = unique_path("trigger");
        let mut writer = ScopeWriter::create(&path, metadata()).unwrap();
        writer
            .write_trigger(&trigger_capture(), &trigger_config())
            .unwrap();
        writer.finish().unwrap();

        let recording = ScopeRecording::open(&path).unwrap();

        assert_eq!(
            recording.triggers(),
            &[TriggerRecord {
                timestamp_ticks: 1_100,
                trigger_sample_index: 11,
                config: trigger_config(),
                auto_timeout: false,
            }]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn recording_rejects_a_valid_crc_index_that_disagrees_with_sample_records() {
        let path = unique_path("bad_index");
        let mut writer = ScopeWriter::create(&path, metadata()).unwrap();
        writer
            .write_sample_frame(&sample_frame(1, 0, 0, &[1, 2]))
            .unwrap();
        writer.finish().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let index_offset = bytes
            .windows(RECORD_MAGIC.len())
            .rposition(|window| window == RECORD_MAGIC)
            .unwrap();
        assert_eq!(bytes[index_offset + 4], RECORD_SESSION_END);
        let index_offset = bytes[..index_offset]
            .windows(RECORD_MAGIC.len())
            .rposition(|window| window == RECORD_MAGIC)
            .unwrap();
        assert_eq!(bytes[index_offset + 4], RECORD_INDEX);
        let payload_len = u32::from_le_bytes(
            bytes[index_offset + 8..index_offset + 12]
                .try_into()
                .unwrap(),
        ) as usize;
        let payload_start = index_offset + RECORD_HEADER_LEN as usize;
        bytes[payload_start + 4] ^= 0x01;
        let crc_start = payload_start + payload_len;
        let checksum = crc32c(&bytes[index_offset + 4..crc_start]);
        bytes[crc_start..crc_start + 4].copy_from_slice(&checksum.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();

        assert!(matches!(
            ScopeRecording::open(&path),
            Err(RecordingError::InvalidFormat(message)) if message.contains("index")
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn async_recorder_writes_validated_records_and_finishes_cleanly() {
        let path = unique_path("async");
        let mut recorder = AsyncScopeRecorder::create(&path, metadata()).unwrap();

        recorder
            .try_write_sample_frame(sample_frame(1, 0, 0, &[1, 2]))
            .unwrap();
        recorder
            .try_write_gap(
                LiveGap {
                    start_sample_index: 2,
                    missing_samples: 2,
                    reason: GapReason::SampleIndexLoss,
                },
                200,
            )
            .unwrap();
        recorder
            .try_write_trigger(trigger_capture(), trigger_config())
            .unwrap();
        recorder.finish().unwrap();

        let recording = ScopeRecording::open(&path).unwrap();
        assert!(recording.clean_end());
        assert_eq!(recording.sample_records().len(), 1);
        assert_eq!(recording.gaps().len(), 1);
        assert_eq!(recording.triggers().len(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn async_recorder_queue_overflow_stops_without_writing_a_clean_end() {
        let path = unique_path("queue_full");
        let mut recorder = AsyncScopeRecorder::create_with_capacity(&path, metadata(), 1).unwrap();
        let release = recorder.pause_worker_for_test();
        recorder
            .try_write_sample_frame(sample_frame(1, 0, 0, &[1, 2]))
            .unwrap();

        assert!(matches!(
            recorder.try_write_sample_frame(sample_frame(2, 2, 200, &[3, 4])),
            Err(RecordingError::QueueFull)
        ));
        drop(release);
        recorder.abort().unwrap();

        let recording = ScopeRecording::open(&path).unwrap();
        assert!(!recording.clean_end());
        assert!(recording.recovered_tail());
        assert_eq!(recording.sample_records().len(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn async_recorder_reports_worker_failure_to_the_owner() {
        let path = unique_path("worker_failure");
        let mut recorder = AsyncScopeRecorder::create(&path, metadata()).unwrap();
        recorder.fail_worker_for_test();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let error = loop {
            if let Some(error) = recorder.poll_error() {
                break error;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        };

        assert!(matches!(error, RecordingError::WorkerFailed(_)));
        assert!(!recorder.is_accepting());
        recorder.abort().unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn recording_ingress_can_enqueue_from_the_acquisition_worker() {
        let path = unique_path("ingress");
        let recorder = AsyncScopeRecorder::create(&path, metadata()).unwrap();
        let ingress = recorder.ingress().unwrap();

        ingress
            .try_write_sample_frame(sample_frame(1, 0, 0, &[1, 2]))
            .unwrap();
        drop(ingress);
        recorder.finish().unwrap();

        let recording = ScopeRecording::open(&path).unwrap();
        assert_eq!(recording.sample_records().len(), 1);
        let _ = std::fs::remove_file(path);
    }
}
