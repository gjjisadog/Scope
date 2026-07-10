use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    buffer::{GapReason, LiveGap},
    protocol::{crc32c, decode_sample_frame, ChannelTable, Frame, ProtocolError},
    trigger::TriggerCapture,
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

    pub fn write_trigger(&mut self, capture: &TriggerCapture) -> Result<(), RecordingError> {
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
        let mut payload = Vec::with_capacity(9);
        payload.extend_from_slice(&trigger_index.to_le_bytes());
        payload.push(u8::from(capture.auto_timeout));
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

impl Drop for ScopeWriter {
    fn drop(&mut self) {
        if !self.finished {
            if let Some(file) = &mut self.file {
                let _ = file.flush();
            }
        }
    }
}

pub struct ScopeRecording {
    path: PathBuf,
    metadata: RecordingMetadata,
    sample_records: Vec<SampleRecordIndex>,
    gaps: Vec<LiveGap>,
    clean_end: bool,
    recovered_tail: bool,
}

impl ScopeRecording {
    pub fn open(path: &Path) -> Result<Self, RecordingError> {
        let mut file = File::open(path)?;
        let metadata = read_file_header(&mut file)?;
        let mut sample_records = Vec::new();
        let mut gaps = Vec::new();
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
                RECORD_TRIGGER => validate_trigger_record(&payload)?,
                RECORD_INDEX => validate_index(&payload)?,
                RECORD_SESSION_END => {
                    if !payload.is_empty() || timestamp_ticks != 0 {
                        return format_error("invalid SessionEnd record");
                    }
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

fn validate_index(payload: &[u8]) -> Result<(), RecordingError> {
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

fn validate_trigger_record(payload: &[u8]) -> Result<(), RecordingError> {
    if payload.len() != 9 || payload[8] > 1 {
        return format_error("invalid trigger record");
    }
    Ok(())
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
}
