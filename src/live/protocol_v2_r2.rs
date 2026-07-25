//! Frozen SCP1 V2 R2 stream, metadata, and multi-subscription wire protocol.
//!
//! R2 is selected only by its dedicated message identifiers. No decoder in
//! this module guesses a revision from payload length.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{
    protocol::{
        ChannelKind, ChannelTable, Crc32c, Frame, ProtocolError, WireFormat, FRAME_CRC_LEN,
        FRAME_HEADER_LEN, MAX_BATCH_SAMPLES, MAX_CHANNEL_COUNT, MAX_PAYLOAD_LEN,
        PROTOCOL_VERSION_V2,
    },
    protocol_v2::{
        CapturePhase, CausalRelation, SampleDomain, SignalOwner, SignalRole, StreamChannelBinding,
        MAX_CAPTURE_PAYLOAD_BYTES, MAX_CAUSAL_RELATION_COUNT, MAX_STREAM_COUNT,
    },
    snapshot::SnapshotMeta,
};

pub const CAPABILITY_V2_STREAMS_R2: u32 = 1 << 4;
pub const CAPABILITY_V2_MULTI_STREAM: u32 = 1 << 5;
pub const CAPABILITY_V2_COMPRESSED_METADATA: u32 = 1 << 6;
pub const CAPABILITY_V2_HARDWARE_CAPTURE_R2: u32 = 1 << 7;

pub const MSG_SAMPLE_BATCH_V2_R2: u8 = 0x33;
pub const MSG_CONFIGURE_STREAMS_R2: u8 = 0x34;
pub const MSG_STREAM_TABLE_R2: u8 = 0x35;
pub const MSG_CAPTURE_DATA_R2: u8 = 0x47;

pub const MAX_SUBSCRIPTIONS_R2: usize = 8;
pub const MAX_CAUSAL_GROUPS_R2: usize = 64;
pub const MAX_METADATA_OVERRIDES_R2: usize = MAX_BATCH_SAMPLES;
pub const R2_SAMPLE_FIXED_PAYLOAD_BYTES: usize = 36;
pub const R2_AFFINE_METADATA_BYTES: usize = 48;
pub const R2_EXPLICIT_METADATA_ROW_BYTES: usize = 36;
pub const R2_CAPTURE_DATA_PREFIX_BYTES: usize = 8;

const OVERRIDE_ROW_SEQUENCE: u16 = 1 << 0;
const OVERRIDE_LOGICAL_CYCLE: u16 = 1 << 1;
const OVERRIDE_SOURCE_SEQUENCE: u16 = 1 << 2;
const OVERRIDE_APPLIED_SEQUENCE: u16 = 1 << 3;
const OVERRIDE_VALID_FLAGS: u16 = 1 << 4;
const OVERRIDE_KNOWN_MASK: u16 = OVERRIDE_ROW_SEQUENCE
    | OVERRIDE_LOGICAL_CYCLE
    | OVERRIDE_SOURCE_SEQUENCE
    | OVERRIDE_APPLIED_SEQUENCE
    | OVERRIDE_VALID_FLAGS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MetadataEncodingR2 {
    AffineWithOverrides = 0,
    Explicit = 1,
}

impl TryFrom<u8> for MetadataEncodingR2 {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::AffineWithOverrides),
            1 => Ok(Self::Explicit),
            _ => invalid(format!("unknown R2 metadata encoding {value}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CausalGroupDescriptorR2 {
    pub consistency_group: u16,
    pub logical_cycle_rate_hz: u32,
    pub max_reorder_cycles: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamDescriptorR2 {
    pub stream_id: u16,
    pub domain: SampleDomain,
    pub capture_phase: CapturePhase,
    pub sample_rate_hz: u32,
    pub consistency_group: u16,
    pub logical_cycle_step: u32,
    pub channel_ids: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamTableR2 {
    pub revision: u32,
    pub causal_groups: Vec<CausalGroupDescriptorR2>,
    pub streams: Vec<StreamDescriptorR2>,
    pub bindings: Vec<StreamChannelBinding>,
    pub causal_relations: Vec<CausalRelation>,
}

impl StreamTableR2 {
    pub fn stream(&self, stream_id: u16) -> Option<&StreamDescriptorR2> {
        self.streams
            .iter()
            .find(|stream| stream.stream_id == stream_id)
    }

    pub fn group(&self, consistency_group: u16) -> Option<&CausalGroupDescriptorR2> {
        self.causal_groups
            .iter()
            .find(|group| group.consistency_group == consistency_group)
    }

    pub fn binding(&self, channel_id: u16) -> Option<&StreamChannelBinding> {
        self.bindings
            .iter()
            .find(|binding| binding.channel_id == channel_id)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_count(
            self.causal_groups.len(),
            1,
            MAX_CAUSAL_GROUPS_R2,
            "R2 causal group count",
        )?;
        validate_count(
            self.streams.len(),
            1,
            MAX_STREAM_COUNT,
            "R2 stream descriptor count",
        )?;
        validate_count(
            self.bindings.len(),
            1,
            MAX_CHANNEL_COUNT,
            "R2 stream binding count",
        )?;
        validate_count(
            self.causal_relations.len(),
            0,
            MAX_CAUSAL_RELATION_COUNT,
            "R2 causal relation count",
        )?;

        let mut group_ids = BTreeSet::new();
        for group in &self.causal_groups {
            if group.consistency_group == 0 || !group_ids.insert(group.consistency_group) {
                return invalid("R2 causal group ids must be non-zero and unique");
            }
            if group.logical_cycle_rate_hz == 0 {
                return invalid(format!(
                    "causal group {} logical cycle rate must be non-zero",
                    group.consistency_group
                ));
            }
        }

        let mut stream_ids = BTreeSet::new();
        let mut descriptor_channels = BTreeSet::new();
        for stream in &self.streams {
            if stream.stream_id == 0 || !stream_ids.insert(stream.stream_id) {
                return invalid("R2 stream ids must be non-zero and unique");
            }
            let group = self.group(stream.consistency_group).ok_or_else(|| {
                invalid_error(format!(
                    "stream {} references unknown causal group {}",
                    stream.stream_id, stream.consistency_group
                ))
            })?;
            if stream.sample_rate_hz == 0 || stream.logical_cycle_step == 0 {
                return invalid(format!(
                    "stream {} rate and logical cycle step must be non-zero",
                    stream.stream_id
                ));
            }
            if !group
                .logical_cycle_rate_hz
                .is_multiple_of(stream.sample_rate_hz)
            {
                return invalid(format!(
                    "causal group rate {} is not exactly divisible by stream {} rate {}",
                    group.logical_cycle_rate_hz, stream.stream_id, stream.sample_rate_hz
                ));
            }
            let expected_step = group.logical_cycle_rate_hz / stream.sample_rate_hz;
            if stream.logical_cycle_step != expected_step {
                return invalid(format!(
                    "stream {} logical cycle step {} does not match exact step {expected_step}",
                    stream.stream_id, stream.logical_cycle_step
                ));
            }
            if stream.sample_rate_hz != stream.domain.fixed_sample_rate_hz()
                || stream.capture_phase != stream.domain.fixed_capture_phase()
            {
                return invalid(format!(
                    "stream {} domain, rate, or capture phase is not frozen",
                    stream.stream_id
                ));
            }
            validate_count(
                stream.channel_ids.len(),
                1,
                MAX_CHANNEL_COUNT,
                "R2 stream channel count",
            )?;
            let mut local_channels = BTreeSet::new();
            for channel_id in &stream.channel_ids {
                if usize::from(*channel_id) >= MAX_CHANNEL_COUNT
                    || !local_channels.insert(*channel_id)
                    || !descriptor_channels.insert(*channel_id)
                {
                    return invalid(format!(
                        "R2 stream {} has an invalid or duplicate channel {channel_id}",
                        stream.stream_id
                    ));
                }
            }
        }

        let mut bound_channels = BTreeSet::new();
        for binding in &self.bindings {
            if !bound_channels.insert(binding.channel_id) {
                return invalid(format!(
                    "R2 channel {} is bound more than once",
                    binding.channel_id
                ));
            }
            let stream = self.stream(binding.stream_id).ok_or_else(|| {
                invalid_error(format!(
                    "R2 binding references unknown stream {}",
                    binding.stream_id
                ))
            })?;
            if !stream.channel_ids.contains(&binding.channel_id) {
                return invalid(format!(
                    "R2 binding channel {} is absent from stream {}",
                    binding.channel_id, binding.stream_id
                ));
            }
        }
        if descriptor_channels != bound_channels {
            return invalid("R2 stream descriptors and bindings contain different channel sets");
        }

        let mut relation_keys = BTreeSet::new();
        for relation in &self.causal_relations {
            let input = self.stream(relation.input_stream_id).ok_or_else(|| {
                invalid_error("R2 causal relation references an unknown input stream")
            })?;
            let result = self.stream(relation.result_stream_id).ok_or_else(|| {
                invalid_error("R2 causal relation references an unknown result stream")
            })?;
            let application = self.stream(relation.application_stream_id).ok_or_else(|| {
                invalid_error("R2 causal relation references an unknown application stream")
            })?;
            if input.consistency_group != result.consistency_group
                || result.consistency_group != application.consistency_group
            {
                return invalid("R2 causal relation crosses consistency groups");
            }
            if !relation_keys.insert((
                relation.input_stream_id,
                relation.result_stream_id,
                relation.application_stream_id,
                relation.result_input_offset,
                relation.application_result_offset,
            )) {
                return invalid("duplicate R2 causal relation");
            }
        }
        Ok(())
    }

    pub fn validate_against_channels(&self, channels: &ChannelTable) -> Result<(), ProtocolError> {
        self.validate()?;
        channels.validate()?;
        for binding in &self.bindings {
            if channels.channel(binding.channel_id).is_none() {
                return invalid(format!(
                    "R2 binding references unknown channel {}",
                    binding.channel_id
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamSubscriptionR2 {
    pub stream_id: u16,
    pub batch_samples: u16,
    pub channel_mask: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigureStreamsR2 {
    pub transaction_id: u32,
    pub subscriptions: Vec<StreamSubscriptionR2>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamSampleBatchR2 {
    pub stream_id: u16,
    pub stream_revision: u32,
    pub domain: SampleDomain,
    pub capture_phase: CapturePhase,
    pub consistency_group: u16,
    pub first_row_sequence: u64,
    pub row_sequence_step: u32,
    pub logical_cycle_step: u32,
    pub sample_period_ticks: u32,
    pub row_count: u16,
    pub channel_ids: Vec<u16>,
    pub sample_data: Vec<u8>,
    pub metadata_encoding: MetadataEncodingR2,
    pub row_metadata: Vec<SnapshotMeta>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedStreamSampleBatchR2 {
    pub stream_id: u16,
    pub revision: u32,
    pub domain: SampleDomain,
    pub capture_phase: CapturePhase,
    pub consistency_group: u16,
    pub first_row_sequence: u64,
    pub sample_period_ticks: u32,
    pub timestamp_ticks: u64,
    pub channel_ids: Vec<u16>,
    pub channels: Vec<Vec<f32>>,
    pub metadata_encoding: MetadataEncodingR2,
    pub row_metadata: Vec<SnapshotMeta>,
    pub raw_frame: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureDataR2 {
    pub capture_id: u32,
    pub block_index: u32,
    pub batch: StreamSampleBatchR2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageV2R2 {
    StreamTable(StreamTableR2),
    ConfigureStreams(ConfigureStreamsR2),
    StreamSampleBatch(StreamSampleBatchR2),
    CaptureData(CaptureDataR2),
}

impl MessageV2R2 {
    pub const fn message_type(&self) -> u8 {
        match self {
            Self::StreamTable(_) => MSG_STREAM_TABLE_R2,
            Self::ConfigureStreams(_) => MSG_CONFIGURE_STREAMS_R2,
            Self::StreamSampleBatch(_) => MSG_SAMPLE_BATCH_V2_R2,
            Self::CaptureData(_) => MSG_CAPTURE_DATA_R2,
        }
    }

    pub fn encode_payload(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut bytes = Vec::new();
        match self {
            Self::StreamTable(table) => encode_stream_table_r2(table, &mut bytes)?,
            Self::ConfigureStreams(configure) => {
                encode_configure_streams_r2(configure, &mut bytes)?
            }
            Self::StreamSampleBatch(batch) => encode_stream_sample_batch_r2(batch, &mut bytes)?,
            Self::CaptureData(data) => {
                put_u32(&mut bytes, data.capture_id);
                put_u32(&mut bytes, data.block_index);
                encode_stream_sample_batch_r2(&data.batch, &mut bytes)?;
            }
        }
        if bytes.len() > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge(bytes.len()));
        }
        Ok(bytes)
    }

    pub fn decode(message_type: u8, payload: &[u8]) -> Result<Self, ProtocolError> {
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge(payload.len()));
        }
        let mut reader = PayloadReader::new(payload);
        let message = match message_type {
            MSG_STREAM_TABLE_R2 => Self::StreamTable(decode_stream_table_r2(&mut reader)?),
            MSG_CONFIGURE_STREAMS_R2 => {
                Self::ConfigureStreams(decode_configure_streams_r2(&mut reader)?)
            }
            MSG_SAMPLE_BATCH_V2_R2 => {
                Self::StreamSampleBatch(decode_stream_sample_batch_r2(&mut reader)?)
            }
            MSG_CAPTURE_DATA_R2 => Self::CaptureData(CaptureDataR2 {
                capture_id: reader.u32()?,
                block_index: reader.u32()?,
                batch: decode_stream_sample_batch_r2(&mut reader)?,
            }),
            other => return Err(ProtocolError::UnknownMessageType(other)),
        };
        reader.finish()?;
        Ok(message)
    }

    pub fn into_frame(
        self,
        flags: u16,
        sequence: u32,
        session_id: u32,
        timestamp_ticks: u64,
    ) -> Result<Frame, ProtocolError> {
        Ok(Frame::new_v2(
            self.message_type(),
            flags,
            sequence,
            session_id,
            timestamp_ticks,
            self.encode_payload()?,
        ))
    }
}

pub fn validate_configure_streams_r2_for_device(
    configure: &ConfigureStreamsR2,
    streams: &StreamTableR2,
    channels: &ChannelTable,
    max_batch_samples: u16,
    max_payload: u32,
) -> Result<(), ProtocolError> {
    validate_configure_streams_shape(configure)?;
    streams.validate_against_channels(channels)?;
    if max_batch_samples == 0 || usize::from(max_batch_samples) > MAX_BATCH_SAMPLES {
        return invalid("device maximum batch samples is outside the protocol range");
    }
    if max_payload == 0 || max_payload as usize > MAX_PAYLOAD_LEN {
        return invalid("device maximum payload is outside the protocol range");
    }

    for subscription in &configure.subscriptions {
        if subscription.batch_samples > max_batch_samples {
            return invalid(format!(
                "stream {} batch samples exceed the device maximum",
                subscription.stream_id
            ));
        }
        let stream = streams.stream(subscription.stream_id).ok_or_else(|| {
            invalid_error(format!(
                "unknown R2 subscription stream {}",
                subscription.stream_id
            ))
        })?;
        let known_mask = channels
            .channels
            .iter()
            .fold(0_u64, |mask, channel| mask | (1_u64 << channel.channel_id));
        if subscription.channel_mask & !known_mask != 0 {
            return invalid(format!(
                "stream {} selects unknown channels",
                subscription.stream_id
            ));
        }
        let selected = channels
            .channels
            .iter()
            .filter(|channel| subscription.channel_mask & (1_u64 << channel.channel_id) != 0)
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return invalid(format!(
                "stream {} selects no channels",
                subscription.stream_id
            ));
        }
        for channel in &selected {
            let binding = streams.binding(channel.channel_id).ok_or_else(|| {
                invalid_error(format!(
                    "channel {} has no R2 stream binding",
                    channel.channel_id
                ))
            })?;
            if binding.stream_id != stream.stream_id {
                return invalid(format!(
                    "channel {} belongs to stream {}, not {}",
                    channel.channel_id, binding.stream_id, stream.stream_id
                ));
            }
        }
        let bytes_per_row = selected.iter().try_fold(0_usize, |total, channel| {
            total
                .checked_add(channel.wire_format.byte_width())
                .ok_or(ProtocolError::LengthOverflow)
        })?;
        let sample_bytes = bytes_per_row
            .checked_mul(usize::from(subscription.batch_samples))
            .ok_or(ProtocolError::LengthOverflow)?;
        let payload_len = R2_SAMPLE_FIXED_PAYLOAD_BYTES
            .checked_add(
                selected
                    .len()
                    .checked_mul(2)
                    .ok_or(ProtocolError::LengthOverflow)?,
            )
            .and_then(|value| value.checked_add(sample_bytes))
            .and_then(|value| value.checked_add(R2_AFFINE_METADATA_BYTES))
            .ok_or(ProtocolError::LengthOverflow)?;
        if payload_len > max_payload as usize {
            return invalid(format!(
                "stream {} R2 payload {payload_len} exceeds negotiated maximum {max_payload}",
                subscription.stream_id
            ));
        }
    }
    Ok(())
}

pub fn decode_stream_sample_frame_r2(
    frame: &Frame,
    channels: &ChannelTable,
    streams: &StreamTableR2,
) -> Result<DecodedStreamSampleBatchR2, ProtocolError> {
    if frame.version != PROTOCOL_VERSION_V2 {
        return Err(ProtocolError::UnsupportedVersion(frame.version));
    }
    if frame.message_type != MSG_SAMPLE_BATCH_V2_R2 {
        return Err(ProtocolError::UnexpectedMessageType {
            expected: MSG_SAMPLE_BATCH_V2_R2,
            actual: frame.message_type,
        });
    }
    streams.validate_against_channels(channels)?;
    let MessageV2R2::StreamSampleBatch(batch) =
        MessageV2R2::decode(frame.message_type, &frame.payload)?
    else {
        unreachable!();
    };
    let stream = streams.stream(batch.stream_id).ok_or_else(|| {
        invalid_error(format!(
            "R2 sample batch references unknown stream {}",
            batch.stream_id
        ))
    })?;
    if batch.stream_revision != streams.revision
        || batch.domain != stream.domain
        || batch.capture_phase != stream.capture_phase
        || batch.consistency_group != stream.consistency_group
        || batch.logical_cycle_step != stream.logical_cycle_step
    {
        return invalid("R2 sample batch disagrees with its frozen stream descriptor");
    }
    let descriptors = batch
        .channel_ids
        .iter()
        .map(|channel_id| {
            let descriptor = channels.channel(*channel_id).ok_or_else(|| {
                invalid_error(format!("R2 sample references unknown channel {channel_id}"))
            })?;
            let binding = streams.binding(*channel_id).ok_or_else(|| {
                invalid_error(format!("R2 channel {channel_id} has no stream binding"))
            })?;
            if binding.stream_id != stream.stream_id {
                return invalid(format!(
                    "R2 stream {} contains channel {channel_id} from stream {}",
                    stream.stream_id, binding.stream_id
                ));
            }
            Ok(descriptor)
        })
        .collect::<Result<Vec<_>, ProtocolError>>()?;
    let bytes_per_row = descriptors.iter().try_fold(0_usize, |total, descriptor| {
        total
            .checked_add(descriptor.wire_format.byte_width())
            .ok_or(ProtocolError::LengthOverflow)
    })?;
    let expected_sample_bytes = bytes_per_row
        .checked_mul(usize::from(batch.row_count))
        .ok_or(ProtocolError::LengthOverflow)?;
    if batch.sample_data.len() != expected_sample_bytes {
        return invalid(format!(
            "R2 sample data length mismatch: expected {expected_sample_bytes}, got {}",
            batch.sample_data.len()
        ));
    }
    let mut reader = PayloadReader::new(&batch.sample_data);
    let mut decoded_channels = descriptors
        .iter()
        .map(|_| Vec::with_capacity(usize::from(batch.row_count)))
        .collect::<Vec<_>>();
    for _ in 0..batch.row_count {
        for (channel_index, descriptor) in descriptors.iter().enumerate() {
            decoded_channels[channel_index]
                .push(decode_engineering_value(&mut reader, descriptor)?);
        }
    }
    reader.finish()?;
    Ok(DecodedStreamSampleBatchR2 {
        stream_id: batch.stream_id,
        revision: batch.stream_revision,
        domain: batch.domain,
        capture_phase: batch.capture_phase,
        consistency_group: batch.consistency_group,
        first_row_sequence: batch.first_row_sequence,
        sample_period_ticks: batch.sample_period_ticks,
        timestamp_ticks: frame.timestamp_ticks,
        channel_ids: batch.channel_ids,
        channels: decoded_channels,
        metadata_encoding: batch.metadata_encoding,
        row_metadata: batch.row_metadata,
        raw_frame: frame.encode()?,
    })
}

pub fn stream_sample_batch_r2_payload_len(
    batch: &StreamSampleBatchR2,
) -> Result<usize, ProtocolError> {
    validate_stream_sample_batch_r2_header(batch)?;
    let metadata_len = metadata_encoded_len(batch)?;
    R2_SAMPLE_FIXED_PAYLOAD_BYTES
        .checked_add(
            batch
                .channel_ids
                .len()
                .checked_mul(2)
                .ok_or(ProtocolError::LengthOverflow)?,
        )
        .and_then(|value| value.checked_add(batch.sample_data.len()))
        .and_then(|value| value.checked_add(metadata_len))
        .ok_or(ProtocolError::LengthOverflow)
}

pub fn capture_data_r2_payload_len(data: &CaptureDataR2) -> Result<usize, ProtocolError> {
    R2_CAPTURE_DATA_PREFIX_BYTES
        .checked_add(stream_sample_batch_r2_payload_len(&data.batch)?)
        .ok_or(ProtocolError::LengthOverflow)
}

pub fn encode_capture_data_r2_payload(data: &CaptureDataR2) -> Result<Vec<u8>, ProtocolError> {
    let mut bytes = Vec::with_capacity(capture_data_r2_payload_len(data)?);
    put_u32(&mut bytes, data.capture_id);
    put_u32(&mut bytes, data.block_index);
    encode_stream_sample_batch_r2(&data.batch, &mut bytes)?;
    Ok(bytes)
}

pub fn capture_integrity_summary_r2<'a>(
    capture_id: u32,
    blocks: impl IntoIterator<Item = &'a CaptureDataR2>,
) -> Result<u32, ProtocolError> {
    let mut sorted = blocks.into_iter().collect::<Vec<_>>();
    sorted.sort_by_key(|block| block.block_index);
    let mut total = 4_usize;
    let mut crc = Crc32c::new();
    crc.update(&capture_id.to_le_bytes());
    for block in sorted {
        let payload = MessageV2R2::CaptureData(block.clone()).encode_payload()?;
        total = total
            .checked_add(payload.len())
            .ok_or(ProtocolError::LengthOverflow)?;
        if total > MAX_CAPTURE_PAYLOAD_BYTES {
            return Err(ProtocolError::PayloadTooLarge(total));
        }
        crc.update(&payload);
    }
    Ok(crc.finalize())
}

pub fn calculate_r2_serial_utilization(
    batch: &StreamSampleBatchR2,
    sample_rate_hz: u32,
    baud: u32,
) -> Result<f64, ProtocolError> {
    if sample_rate_hz == 0 || baud == 0 || batch.row_count == 0 {
        return invalid("R2 link budget rate, baud, and row count must be non-zero");
    }
    let payload = stream_sample_batch_r2_payload_len(batch)?;
    let frame_bytes = FRAME_HEADER_LEN
        .checked_add(payload)
        .and_then(|value| value.checked_add(FRAME_CRC_LEN))
        .ok_or(ProtocolError::LengthOverflow)?;
    let frames_per_second = f64::from(sample_rate_hz) / f64::from(batch.row_count);
    Ok(frame_bytes as f64 * frames_per_second * 10.0 / f64::from(baud))
}

fn encode_stream_table_r2(table: &StreamTableR2, bytes: &mut Vec<u8>) -> Result<(), ProtocolError> {
    table.validate()?;
    put_u32(bytes, table.revision);
    put_u16(
        bytes,
        checked_u16(table.causal_groups.len(), "R2 causal groups")?,
    );
    put_u16(bytes, checked_u16(table.streams.len(), "R2 streams")?);
    put_u16(bytes, checked_u16(table.bindings.len(), "R2 bindings")?);
    put_u16(
        bytes,
        checked_u16(table.causal_relations.len(), "R2 causal relations")?,
    );
    put_u16(bytes, 0);
    for group in &table.causal_groups {
        put_u16(bytes, group.consistency_group);
        put_u16(bytes, group.max_reorder_cycles);
        put_u32(bytes, group.logical_cycle_rate_hz);
    }
    for stream in &table.streams {
        put_u16(bytes, stream.stream_id);
        bytes.push(stream.domain as u8);
        bytes.push(stream.capture_phase as u8);
        put_u32(bytes, stream.sample_rate_hz);
        put_u16(bytes, stream.consistency_group);
        put_u32(bytes, stream.logical_cycle_step);
        put_u16(
            bytes,
            checked_u16(stream.channel_ids.len(), "R2 stream channels")?,
        );
        for channel_id in &stream.channel_ids {
            put_u16(bytes, *channel_id);
        }
    }
    for binding in &table.bindings {
        put_u16(bytes, binding.channel_id);
        put_u16(bytes, binding.stream_id);
        bytes.push(binding.owner as u8);
        bytes.push(binding.role as u8);
    }
    for relation in &table.causal_relations {
        put_u16(bytes, relation.input_stream_id);
        put_u16(bytes, relation.result_stream_id);
        put_u16(bytes, relation.application_stream_id);
        put_i16(bytes, relation.result_input_offset);
        put_i16(bytes, relation.application_result_offset);
    }
    Ok(())
}

fn decode_stream_table_r2(reader: &mut PayloadReader<'_>) -> Result<StreamTableR2, ProtocolError> {
    let revision = reader.u32()?;
    let group_count = usize::from(reader.u16()?);
    let stream_count = usize::from(reader.u16()?);
    let binding_count = usize::from(reader.u16()?);
    let relation_count = usize::from(reader.u16()?);
    if reader.u16()? != 0 {
        return invalid("R2 STREAM_TABLE reserved field must be zero");
    }
    validate_count(group_count, 1, MAX_CAUSAL_GROUPS_R2, "R2 causal groups")?;
    validate_count(stream_count, 1, MAX_STREAM_COUNT, "R2 streams")?;
    validate_count(binding_count, 1, MAX_CHANNEL_COUNT, "R2 bindings")?;
    validate_count(
        relation_count,
        0,
        MAX_CAUSAL_RELATION_COUNT,
        "R2 causal relations",
    )?;
    let mut causal_groups = Vec::with_capacity(group_count);
    for _ in 0..group_count {
        causal_groups.push(CausalGroupDescriptorR2 {
            consistency_group: reader.u16()?,
            max_reorder_cycles: reader.u16()?,
            logical_cycle_rate_hz: reader.u32()?,
        });
    }
    let mut streams = Vec::with_capacity(stream_count);
    for _ in 0..stream_count {
        let stream_id = reader.u16()?;
        let domain = SampleDomain::try_from(reader.u8()?)?;
        let capture_phase = CapturePhase::try_from(reader.u8()?)?;
        let sample_rate_hz = reader.u32()?;
        let consistency_group = reader.u16()?;
        let logical_cycle_step = reader.u32()?;
        let channel_count = usize::from(reader.u16()?);
        validate_count(channel_count, 1, MAX_CHANNEL_COUNT, "R2 stream channels")?;
        let mut channel_ids = Vec::with_capacity(channel_count);
        for _ in 0..channel_count {
            channel_ids.push(reader.u16()?);
        }
        streams.push(StreamDescriptorR2 {
            stream_id,
            domain,
            capture_phase,
            sample_rate_hz,
            consistency_group,
            logical_cycle_step,
            channel_ids,
        });
    }
    let mut bindings = Vec::with_capacity(binding_count);
    for _ in 0..binding_count {
        bindings.push(StreamChannelBinding {
            channel_id: reader.u16()?,
            stream_id: reader.u16()?,
            owner: SignalOwner::try_from(reader.u8()?)?,
            role: SignalRole::try_from(reader.u8()?)?,
        });
    }
    let mut causal_relations = Vec::with_capacity(relation_count);
    for _ in 0..relation_count {
        causal_relations.push(CausalRelation {
            input_stream_id: reader.u16()?,
            result_stream_id: reader.u16()?,
            application_stream_id: reader.u16()?,
            result_input_offset: reader.i16()?,
            application_result_offset: reader.i16()?,
        });
    }
    let table = StreamTableR2 {
        revision,
        causal_groups,
        streams,
        bindings,
        causal_relations,
    };
    table.validate()?;
    Ok(table)
}

fn encode_configure_streams_r2(
    configure: &ConfigureStreamsR2,
    bytes: &mut Vec<u8>,
) -> Result<(), ProtocolError> {
    validate_configure_streams_shape(configure)?;
    put_u32(bytes, configure.transaction_id);
    put_u16(
        bytes,
        checked_u16(configure.subscriptions.len(), "R2 subscriptions")?,
    );
    put_u16(bytes, 0);
    for subscription in &configure.subscriptions {
        put_u16(bytes, subscription.stream_id);
        put_u16(bytes, subscription.batch_samples);
        put_u64(bytes, subscription.channel_mask);
    }
    Ok(())
}

fn decode_configure_streams_r2(
    reader: &mut PayloadReader<'_>,
) -> Result<ConfigureStreamsR2, ProtocolError> {
    let transaction_id = reader.u32()?;
    let count = usize::from(reader.u16()?);
    if reader.u16()? != 0 {
        return invalid("R2 CONFIGURE_STREAMS reserved field must be zero");
    }
    validate_count(count, 1, MAX_SUBSCRIPTIONS_R2, "R2 subscriptions")?;
    let mut subscriptions = Vec::with_capacity(count);
    for _ in 0..count {
        subscriptions.push(StreamSubscriptionR2 {
            stream_id: reader.u16()?,
            batch_samples: reader.u16()?,
            channel_mask: reader.u64()?,
        });
    }
    let configure = ConfigureStreamsR2 {
        transaction_id,
        subscriptions,
    };
    validate_configure_streams_shape(&configure)?;
    Ok(configure)
}

fn validate_configure_streams_shape(configure: &ConfigureStreamsR2) -> Result<(), ProtocolError> {
    if configure.transaction_id == 0 {
        return invalid("R2 transaction id must be non-zero");
    }
    validate_count(
        configure.subscriptions.len(),
        1,
        MAX_SUBSCRIPTIONS_R2,
        "R2 subscriptions",
    )?;
    let mut stream_ids = BTreeSet::new();
    for subscription in &configure.subscriptions {
        if subscription.stream_id == 0
            || subscription.batch_samples == 0
            || usize::from(subscription.batch_samples) > MAX_BATCH_SAMPLES
            || subscription.channel_mask == 0
            || !stream_ids.insert(subscription.stream_id)
        {
            return invalid(
                "R2 subscriptions require unique non-zero streams, batches, and channel masks",
            );
        }
    }
    Ok(())
}

fn encode_stream_sample_batch_r2(
    batch: &StreamSampleBatchR2,
    bytes: &mut Vec<u8>,
) -> Result<(), ProtocolError> {
    let start_len = bytes.len();
    let expected_len = stream_sample_batch_r2_payload_len(batch)?;
    put_u16(bytes, batch.stream_id);
    put_u32(bytes, batch.stream_revision);
    bytes.push(batch.domain as u8);
    bytes.push(batch.capture_phase as u8);
    put_u16(bytes, batch.consistency_group);
    put_u64(bytes, batch.first_row_sequence);
    put_u16(bytes, batch.row_count);
    put_u32(bytes, batch.sample_period_ticks);
    put_u16(bytes, checked_u16(batch.channel_ids.len(), "R2 channels")?);
    bytes.push(batch.metadata_encoding as u8);
    bytes.push(0);
    put_u32(
        bytes,
        u32::try_from(batch.sample_data.len()).map_err(|_| ProtocolError::LengthOverflow)?,
    );
    let metadata_len = metadata_encoded_len(batch)?;
    put_u32(
        bytes,
        u32::try_from(metadata_len).map_err(|_| ProtocolError::LengthOverflow)?,
    );
    for channel_id in &batch.channel_ids {
        put_u16(bytes, *channel_id);
    }
    bytes.extend_from_slice(&batch.sample_data);
    encode_metadata_r2(batch, bytes)?;
    if bytes.len().saturating_sub(start_len) != expected_len {
        return invalid("R2 payload length calculator disagrees with encoder");
    }
    Ok(())
}

fn decode_stream_sample_batch_r2(
    reader: &mut PayloadReader<'_>,
) -> Result<StreamSampleBatchR2, ProtocolError> {
    let stream_id = reader.u16()?;
    let stream_revision = reader.u32()?;
    let domain = SampleDomain::try_from(reader.u8()?)?;
    let capture_phase = CapturePhase::try_from(reader.u8()?)?;
    let consistency_group = reader.u16()?;
    let first_row_sequence = reader.u64()?;
    let row_count = reader.u16()?;
    let sample_period_ticks = reader.u32()?;
    let channel_count = usize::from(reader.u16()?);
    let metadata_encoding = MetadataEncodingR2::try_from(reader.u8()?)?;
    if reader.u8()? != 0 {
        return invalid("R2 SAMPLE_BATCH reserved field must be zero");
    }
    let sample_data_len =
        usize::try_from(reader.u32()?).map_err(|_| ProtocolError::LengthOverflow)?;
    let metadata_len = usize::try_from(reader.u32()?).map_err(|_| ProtocolError::LengthOverflow)?;
    validate_count(channel_count, 1, MAX_CHANNEL_COUNT, "R2 selected channels")?;
    let mut channel_ids = Vec::with_capacity(channel_count);
    for _ in 0..channel_count {
        channel_ids.push(reader.u16()?);
    }
    let sample_data = reader.bytes(sample_data_len, "R2 sample data")?.to_vec();
    let metadata_bytes = reader.bytes(metadata_len, "R2 metadata")?;
    let (row_sequence_step, logical_cycle_step, row_metadata) =
        decode_metadata_r2(metadata_encoding, row_count, metadata_bytes)?;
    let batch = StreamSampleBatchR2 {
        stream_id,
        stream_revision,
        domain,
        capture_phase,
        consistency_group,
        first_row_sequence,
        row_sequence_step,
        logical_cycle_step,
        sample_period_ticks,
        row_count,
        channel_ids,
        sample_data,
        metadata_encoding,
        row_metadata,
    };
    validate_stream_sample_batch_r2_header(&batch)?;
    Ok(batch)
}

fn validate_stream_sample_batch_r2_header(
    batch: &StreamSampleBatchR2,
) -> Result<(), ProtocolError> {
    if batch.stream_id == 0
        || batch.stream_revision == 0
        || batch.consistency_group == 0
        || batch.row_count == 0
        || usize::from(batch.row_count) > MAX_BATCH_SAMPLES
        || batch.sample_period_ticks == 0
        || batch.row_sequence_step == 0
        || batch.logical_cycle_step == 0
    {
        return invalid("R2 sample batch has an invalid non-zero header field");
    }
    if batch.row_metadata.len() != usize::from(batch.row_count) {
        return invalid("R2 row metadata count does not match row count");
    }
    if batch
        .row_metadata
        .first()
        .is_none_or(|row| row.row_sequence != batch.first_row_sequence)
    {
        return invalid("R2 first row sequence does not match metadata");
    }
    validate_count(
        batch.channel_ids.len(),
        1,
        MAX_CHANNEL_COUNT,
        "R2 selected channels",
    )?;
    let mut channel_ids = BTreeSet::new();
    for channel_id in &batch.channel_ids {
        if usize::from(*channel_id) >= MAX_CHANNEL_COUNT || !channel_ids.insert(*channel_id) {
            return invalid("R2 sample batch has an invalid or duplicate channel id");
        }
    }
    Ok(())
}

fn metadata_encoded_len(batch: &StreamSampleBatchR2) -> Result<usize, ProtocolError> {
    match batch.metadata_encoding {
        MetadataEncodingR2::Explicit => usize::from(batch.row_count)
            .checked_mul(R2_EXPLICIT_METADATA_ROW_BYTES)
            .ok_or(ProtocolError::LengthOverflow),
        MetadataEncodingR2::AffineWithOverrides => {
            let base = affine_base(batch)?;
            let mut len = R2_AFFINE_METADATA_BYTES;
            for (index, row) in batch.row_metadata.iter().enumerate() {
                let mask = affine_override_mask(batch, base, index, *row)?;
                if mask != 0 {
                    len = len
                        .checked_add(4)
                        .and_then(|value| value.checked_add(override_value_len(mask)))
                        .ok_or(ProtocolError::LengthOverflow)?;
                }
            }
            Ok(len)
        }
    }
}

#[derive(Clone, Copy)]
struct AffineBase {
    first_row: u64,
    first_logical: u64,
    source_delta: i64,
    applied_delta: i64,
    common_flags: u32,
}

fn affine_base(batch: &StreamSampleBatchR2) -> Result<AffineBase, ProtocolError> {
    let first = *batch
        .row_metadata
        .first()
        .ok_or_else(|| invalid_error("R2 affine metadata requires at least one row"))?;
    Ok(AffineBase {
        first_row: first.row_sequence,
        first_logical: first.logical_cycle_sequence,
        source_delta: signed_delta(first.source_sequence, first.logical_cycle_sequence)?,
        applied_delta: signed_delta(first.applied_sequence, first.logical_cycle_sequence)?,
        common_flags: first.valid_flags,
    })
}

fn affine_override_mask(
    batch: &StreamSampleBatchR2,
    base: AffineBase,
    index: usize,
    row: SnapshotMeta,
) -> Result<u16, ProtocolError> {
    let offset = u64::try_from(index).map_err(|_| ProtocolError::LengthOverflow)?;
    let expected_row = checked_affine(base.first_row, batch.row_sequence_step, offset)?;
    let expected_logical = checked_affine(base.first_logical, batch.logical_cycle_step, offset)?;
    let expected_source = add_signed(expected_logical, base.source_delta)?;
    let expected_applied = add_signed(expected_logical, base.applied_delta)?;
    let mut mask = 0_u16;
    if row.row_sequence != expected_row {
        mask |= OVERRIDE_ROW_SEQUENCE;
    }
    if row.logical_cycle_sequence != expected_logical {
        mask |= OVERRIDE_LOGICAL_CYCLE;
    }
    if row.source_sequence != expected_source {
        mask |= OVERRIDE_SOURCE_SEQUENCE;
    }
    if row.applied_sequence != expected_applied {
        mask |= OVERRIDE_APPLIED_SEQUENCE;
    }
    if row.valid_flags != base.common_flags {
        mask |= OVERRIDE_VALID_FLAGS;
    }
    Ok(mask)
}

fn encode_metadata_r2(
    batch: &StreamSampleBatchR2,
    bytes: &mut Vec<u8>,
) -> Result<(), ProtocolError> {
    match batch.metadata_encoding {
        MetadataEncodingR2::Explicit => {
            for row in &batch.row_metadata {
                put_snapshot_meta(bytes, *row);
            }
        }
        MetadataEncodingR2::AffineWithOverrides => {
            let base = affine_base(batch)?;
            let mut overrides = Vec::new();
            for (index, row) in batch.row_metadata.iter().enumerate() {
                let mask = affine_override_mask(batch, base, index, *row)?;
                if mask != 0 {
                    overrides.push((
                        u16::try_from(index).map_err(|_| ProtocolError::LengthOverflow)?,
                        mask,
                        *row,
                    ));
                }
            }
            if overrides.len() > MAX_METADATA_OVERRIDES_R2 {
                return invalid("too many R2 metadata overrides");
            }
            put_u64(bytes, base.first_row);
            put_u32(bytes, batch.row_sequence_step);
            put_u64(bytes, base.first_logical);
            put_u32(bytes, batch.logical_cycle_step);
            put_i64(bytes, base.source_delta);
            put_i64(bytes, base.applied_delta);
            put_u32(bytes, base.common_flags);
            put_u16(
                bytes,
                checked_u16(overrides.len(), "R2 metadata overrides")?,
            );
            put_u16(bytes, 0);
            for (row_offset, mask, row) in overrides {
                put_u16(bytes, row_offset);
                put_u16(bytes, mask);
                put_override_values(bytes, mask, row);
            }
        }
    }
    Ok(())
}

fn decode_metadata_r2(
    encoding: MetadataEncodingR2,
    row_count: u16,
    bytes: &[u8],
) -> Result<(u32, u32, Vec<SnapshotMeta>), ProtocolError> {
    let mut reader = PayloadReader::new(bytes);
    let result = match encoding {
        MetadataEncodingR2::Explicit => {
            let expected_len = usize::from(row_count)
                .checked_mul(R2_EXPLICIT_METADATA_ROW_BYTES)
                .ok_or(ProtocolError::LengthOverflow)?;
            if bytes.len() != expected_len {
                return invalid("R2 explicit metadata length does not match row count");
            }
            let mut rows = Vec::with_capacity(usize::from(row_count));
            for _ in 0..row_count {
                rows.push(read_snapshot_meta(&mut reader)?);
            }
            let row_step = infer_step(&rows, |row| row.row_sequence)?;
            let logical_step = infer_step(&rows, |row| row.logical_cycle_sequence)?;
            (row_step, logical_step, rows)
        }
        MetadataEncodingR2::AffineWithOverrides => {
            let first_row = reader.u64()?;
            let row_step = reader.u32()?;
            let first_logical = reader.u64()?;
            let logical_step = reader.u32()?;
            let source_delta = reader.i64()?;
            let applied_delta = reader.i64()?;
            let common_flags = reader.u32()?;
            let override_count = usize::from(reader.u16()?);
            if reader.u16()? != 0 {
                return invalid("R2 affine metadata reserved field must be zero");
            }
            if row_step == 0 || logical_step == 0 {
                return invalid("R2 affine row and logical steps must be non-zero");
            }
            validate_count(
                override_count,
                0,
                usize::from(row_count),
                "R2 metadata overrides",
            )?;
            let mut rows = Vec::with_capacity(usize::from(row_count));
            for index in 0..row_count {
                let offset = u64::from(index);
                let logical_cycle_sequence = checked_affine(first_logical, logical_step, offset)?;
                rows.push(SnapshotMeta {
                    row_sequence: checked_affine(first_row, row_step, offset)?,
                    logical_cycle_sequence,
                    source_sequence: add_signed(logical_cycle_sequence, source_delta)?,
                    applied_sequence: add_signed(logical_cycle_sequence, applied_delta)?,
                    valid_flags: common_flags,
                });
            }
            let mut previous_offset = None;
            for _ in 0..override_count {
                let row_offset = reader.u16()?;
                let mask = reader.u16()?;
                if usize::from(row_offset) >= rows.len()
                    || previous_offset.is_some_and(|previous| row_offset <= previous)
                    || mask == 0
                    || mask & !OVERRIDE_KNOWN_MASK != 0
                {
                    return invalid(
                        "R2 metadata overrides must be unique, ordered, in range, and known",
                    );
                }
                previous_offset = Some(row_offset);
                apply_override_values(&mut reader, mask, &mut rows[usize::from(row_offset)])?;
            }
            (row_step, logical_step, rows)
        }
    };
    reader.finish()?;
    Ok(result)
}

fn put_snapshot_meta(bytes: &mut Vec<u8>, row: SnapshotMeta) {
    put_u64(bytes, row.row_sequence);
    put_u64(bytes, row.logical_cycle_sequence);
    put_u64(bytes, row.source_sequence);
    put_u64(bytes, row.applied_sequence);
    put_u32(bytes, row.valid_flags);
}

fn read_snapshot_meta(reader: &mut PayloadReader<'_>) -> Result<SnapshotMeta, ProtocolError> {
    Ok(SnapshotMeta {
        row_sequence: reader.u64()?,
        logical_cycle_sequence: reader.u64()?,
        source_sequence: reader.u64()?,
        applied_sequence: reader.u64()?,
        valid_flags: reader.u32()?,
    })
}

fn put_override_values(bytes: &mut Vec<u8>, mask: u16, row: SnapshotMeta) {
    if mask & OVERRIDE_ROW_SEQUENCE != 0 {
        put_u64(bytes, row.row_sequence);
    }
    if mask & OVERRIDE_LOGICAL_CYCLE != 0 {
        put_u64(bytes, row.logical_cycle_sequence);
    }
    if mask & OVERRIDE_SOURCE_SEQUENCE != 0 {
        put_u64(bytes, row.source_sequence);
    }
    if mask & OVERRIDE_APPLIED_SEQUENCE != 0 {
        put_u64(bytes, row.applied_sequence);
    }
    if mask & OVERRIDE_VALID_FLAGS != 0 {
        put_u32(bytes, row.valid_flags);
    }
}

fn apply_override_values(
    reader: &mut PayloadReader<'_>,
    mask: u16,
    row: &mut SnapshotMeta,
) -> Result<(), ProtocolError> {
    if mask & OVERRIDE_ROW_SEQUENCE != 0 {
        row.row_sequence = reader.u64()?;
    }
    if mask & OVERRIDE_LOGICAL_CYCLE != 0 {
        row.logical_cycle_sequence = reader.u64()?;
    }
    if mask & OVERRIDE_SOURCE_SEQUENCE != 0 {
        row.source_sequence = reader.u64()?;
    }
    if mask & OVERRIDE_APPLIED_SEQUENCE != 0 {
        row.applied_sequence = reader.u64()?;
    }
    if mask & OVERRIDE_VALID_FLAGS != 0 {
        row.valid_flags = reader.u32()?;
    }
    Ok(())
}

fn override_value_len(mask: u16) -> usize {
    usize::from((mask & OVERRIDE_ROW_SEQUENCE != 0) as u8) * 8
        + usize::from((mask & OVERRIDE_LOGICAL_CYCLE != 0) as u8) * 8
        + usize::from((mask & OVERRIDE_SOURCE_SEQUENCE != 0) as u8) * 8
        + usize::from((mask & OVERRIDE_APPLIED_SEQUENCE != 0) as u8) * 8
        + usize::from((mask & OVERRIDE_VALID_FLAGS != 0) as u8) * 4
}

fn infer_step(
    rows: &[SnapshotMeta],
    select: impl Fn(&SnapshotMeta) -> u64,
) -> Result<u32, ProtocolError> {
    if rows.len() < 2 {
        return Ok(1);
    }
    let step = select(&rows[1])
        .checked_sub(select(&rows[0]))
        .ok_or_else(|| invalid_error("R2 explicit metadata is not monotonic"))?;
    u32::try_from(step).map_err(|_| ProtocolError::LengthOverflow)
}

fn checked_affine(first: u64, step: u32, offset: u64) -> Result<u64, ProtocolError> {
    u64::from(step)
        .checked_mul(offset)
        .and_then(|delta| first.checked_add(delta))
        .ok_or(ProtocolError::LengthOverflow)
}

fn signed_delta(value: u64, base: u64) -> Result<i64, ProtocolError> {
    let delta = i128::from(value) - i128::from(base);
    i64::try_from(delta).map_err(|_| ProtocolError::LengthOverflow)
}

fn add_signed(value: u64, delta: i64) -> Result<u64, ProtocolError> {
    if delta >= 0 {
        value
            .checked_add(delta as u64)
            .ok_or(ProtocolError::LengthOverflow)
    } else {
        value
            .checked_sub(delta.unsigned_abs())
            .ok_or(ProtocolError::LengthOverflow)
    }
}

fn decode_engineering_value(
    reader: &mut PayloadReader<'_>,
    descriptor: &super::protocol::ChannelDescriptor,
) -> Result<f32, ProtocolError> {
    let value = match descriptor.wire_format {
        WireFormat::I16 => f32::from(reader.i16()?) * descriptor.scale + descriptor.offset,
        WireFormat::I32 => reader.i32()? as f32 * descriptor.scale + descriptor.offset,
        WireFormat::F32 => reader.f32()?,
        WireFormat::U8 => {
            let raw = reader.u8()?;
            if descriptor.kind == ChannelKind::Digital && raw > 1 {
                return invalid(format!(
                    "digital channel {} has invalid value {raw}",
                    descriptor.channel_id
                ));
            }
            f32::from(raw) * descriptor.scale + descriptor.offset
        }
    };
    Ok(value)
}

fn checked_u16(value: usize, label: &str) -> Result<u16, ProtocolError> {
    u16::try_from(value).map_err(|_| invalid_error(format!("too many {label}")))
}

fn validate_count(
    value: usize,
    minimum: usize,
    maximum: usize,
    label: &str,
) -> Result<(), ProtocolError> {
    if value < minimum || value > maximum {
        return invalid(format!("{label} {value} is outside {minimum}..={maximum}"));
    }
    Ok(())
}

fn invalid<T>(message: impl Into<String>) -> Result<T, ProtocolError> {
    Err(invalid_error(message))
}

fn invalid_error(message: impl Into<String>) -> ProtocolError {
    ProtocolError::InvalidPayload(message.into())
}

fn put_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_i16(bytes: &mut Vec<u8>, value: i16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_i64(bytes: &mut Vec<u8>, value: i64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

struct PayloadReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> PayloadReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn bytes(&mut self, len: usize, label: &str) -> Result<&'a [u8], ProtocolError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(ProtocolError::LengthOverflow)?;
        let value = self.bytes.get(self.offset..end).ok_or_else(|| {
            invalid_error(format!(
                "truncated {label}: need {len} bytes at offset {}, payload has {}",
                self.offset,
                self.bytes.len()
            ))
        })?;
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.bytes(1, "u8")?[0])
    }

    fn u16(&mut self) -> Result<u16, ProtocolError> {
        Ok(u16::from_le_bytes(
            self.bytes(2, "u16")?.try_into().expect("fixed width"),
        ))
    }

    fn i16(&mut self) -> Result<i16, ProtocolError> {
        Ok(i16::from_le_bytes(
            self.bytes(2, "i16")?.try_into().expect("fixed width"),
        ))
    }

    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_le_bytes(
            self.bytes(4, "u32")?.try_into().expect("fixed width"),
        ))
    }

    fn i32(&mut self) -> Result<i32, ProtocolError> {
        Ok(i32::from_le_bytes(
            self.bytes(4, "i32")?.try_into().expect("fixed width"),
        ))
    }

    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_le_bytes(
            self.bytes(8, "u64")?.try_into().expect("fixed width"),
        ))
    }

    fn i64(&mut self) -> Result<i64, ProtocolError> {
        Ok(i64::from_le_bytes(
            self.bytes(8, "i64")?.try_into().expect("fixed width"),
        ))
    }

    fn f32(&mut self) -> Result<f32, ProtocolError> {
        Ok(f32::from_le_bytes(
            self.bytes(4, "f32")?.try_into().expect("fixed width"),
        ))
    }

    fn finish(self) -> Result<(), ProtocolError> {
        if self.offset != self.bytes.len() {
            return invalid(format!(
                "payload contains {} unexpected trailing bytes",
                self.bytes.len() - self.offset
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::{
        protocol::{ChannelDescriptor, ChannelKind},
        snapshot::{APPLIED_SEQUENCE_VALID, FROZEN_ROW, SNAPSHOT_VALID, SOURCE_SEQUENCE_VALID},
    };

    fn table() -> StreamTableR2 {
        StreamTableR2 {
            revision: 2,
            causal_groups: vec![CausalGroupDescriptorR2 {
                consistency_group: 1,
                logical_cycle_rate_hz: 32_000,
                max_reorder_cycles: 64,
            }],
            streams: vec![
                stream(1, SampleDomain::Fast32k, 1, vec![0]),
                stream(2, SampleDomain::Control8k, 4, vec![1]),
                stream(3, SampleDomain::Slow1k, 32, vec![2]),
            ],
            bindings: vec![
                binding(0, 1, SignalOwner::Cpu1Cla1, SignalRole::ControlInput),
                binding(1, 2, SignalOwner::Cpu1, SignalRole::ControlOutput),
                binding(2, 3, SignalOwner::Cpu2, SignalRole::AppliedCommand),
            ],
            causal_relations: vec![CausalRelation {
                input_stream_id: 1,
                result_stream_id: 2,
                application_stream_id: 3,
                result_input_offset: 0,
                application_result_offset: 4,
            }],
        }
    }

    fn stream(
        stream_id: u16,
        domain: SampleDomain,
        logical_cycle_step: u32,
        channel_ids: Vec<u16>,
    ) -> StreamDescriptorR2 {
        StreamDescriptorR2 {
            stream_id,
            domain,
            capture_phase: domain.fixed_capture_phase(),
            sample_rate_hz: domain.fixed_sample_rate_hz(),
            consistency_group: 1,
            logical_cycle_step,
            channel_ids,
        }
    }

    fn binding(
        channel_id: u16,
        stream_id: u16,
        owner: SignalOwner,
        role: SignalRole,
    ) -> StreamChannelBinding {
        StreamChannelBinding {
            channel_id,
            stream_id,
            owner,
            role,
        }
    }

    fn batch(encoding: MetadataEncodingR2, rows: u16) -> StreamSampleBatchR2 {
        StreamSampleBatchR2 {
            stream_id: 2,
            stream_revision: 2,
            domain: SampleDomain::Control8k,
            capture_phase: CapturePhase::ControlCycleEnd,
            consistency_group: 1,
            first_row_sequence: 10,
            row_sequence_step: 1,
            logical_cycle_step: 4,
            sample_period_ticks: 4_000,
            row_count: rows,
            channel_ids: vec![1],
            sample_data: vec![0; usize::from(rows) * 2],
            metadata_encoding: encoding,
            row_metadata: (0..rows)
                .map(|offset| {
                    let logical = 40 + u64::from(offset) * 4;
                    SnapshotMeta {
                        row_sequence: 10 + u64::from(offset),
                        logical_cycle_sequence: logical,
                        source_sequence: logical,
                        applied_sequence: logical.saturating_sub(4),
                        valid_flags: SNAPSHOT_VALID
                            | FROZEN_ROW
                            | SOURCE_SEQUENCE_VALID
                            | APPLIED_SEQUENCE_VALID,
                    }
                })
                .collect(),
        }
    }

    #[test]
    fn r2_message_numbers_and_capabilities_are_frozen() {
        assert_eq!(CAPABILITY_V2_STREAMS_R2, 1 << 4);
        assert_eq!(CAPABILITY_V2_MULTI_STREAM, 1 << 5);
        assert_eq!(CAPABILITY_V2_COMPRESSED_METADATA, 1 << 6);
        assert_eq!(CAPABILITY_V2_HARDWARE_CAPTURE_R2, 1 << 7);
        assert_eq!(MSG_SAMPLE_BATCH_V2_R2, 0x33);
        assert_eq!(MSG_CONFIGURE_STREAMS_R2, 0x34);
        assert_eq!(MSG_STREAM_TABLE_R2, 0x35);
        assert_eq!(MSG_CAPTURE_DATA_R2, 0x47);
    }

    #[test]
    fn revision_dispatch_never_guesses_from_payload_length() {
        let r2_payload = MessageV2R2::ConfigureStreams(ConfigureStreamsR2 {
            transaction_id: 1,
            subscriptions: vec![StreamSubscriptionR2 {
                stream_id: 1,
                batch_samples: 1,
                channel_mask: 1,
            }],
        })
        .encode_payload()
        .unwrap();
        assert!(matches!(
            MessageV2R2::decode(
                super::super::protocol_v2::MSG_CONFIGURE_STREAM_R1,
                &r2_payload
            ),
            Err(ProtocolError::UnknownMessageType(
                super::super::protocol_v2::MSG_CONFIGURE_STREAM_R1
            ))
        ));
        assert!(matches!(
            super::super::protocol_v2::MessageV2::decode(MSG_CONFIGURE_STREAMS_R2, &r2_payload),
            Err(ProtocolError::UnknownMessageType(MSG_CONFIGURE_STREAMS_R2))
        ));
    }

    #[test]
    fn stream_table_and_atomic_subscriptions_round_trip() {
        let table = table();
        let message = MessageV2R2::StreamTable(table.clone());
        assert_eq!(
            MessageV2R2::decode(message.message_type(), &message.encode_payload().unwrap())
                .unwrap(),
            message
        );
        let configure = ConfigureStreamsR2 {
            transaction_id: 9,
            subscriptions: vec![
                StreamSubscriptionR2 {
                    stream_id: 1,
                    batch_samples: 32,
                    channel_mask: 1,
                },
                StreamSubscriptionR2 {
                    stream_id: 2,
                    batch_samples: 16,
                    channel_mask: 2,
                },
                StreamSubscriptionR2 {
                    stream_id: 3,
                    batch_samples: 4,
                    channel_mask: 4,
                },
            ],
        };
        let message = MessageV2R2::ConfigureStreams(configure);
        assert_eq!(
            MessageV2R2::decode(message.message_type(), &message.encode_payload().unwrap())
                .unwrap(),
            message
        );
    }

    #[test]
    fn configure_streams_r2_golden_payload_is_frozen() {
        let message = MessageV2R2::ConfigureStreams(ConfigureStreamsR2 {
            transaction_id: 0x0102_0304,
            subscriptions: vec![StreamSubscriptionR2 {
                stream_id: 1,
                batch_samples: 32,
                channel_mask: 5,
            }],
        });
        assert_eq!(
            message.encode_payload().unwrap(),
            vec![
                0x04, 0x03, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x20, 0x00, 0x05, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]
        );
    }

    #[test]
    fn configure_streams_r2_golden_frame_is_frozen() {
        let frame = MessageV2R2::ConfigureStreams(ConfigureStreamsR2 {
            transaction_id: 0x0102_0304,
            subscriptions: vec![StreamSubscriptionR2 {
                stream_id: 1,
                batch_samples: 32,
                channel_mask: 5,
            }],
        })
        .into_frame(0x1122, 0x0102_0304, 0x0506_0708, 0x0102_0304_0506_0708)
        .unwrap()
        .encode()
        .unwrap();
        assert_eq!(
            frame,
            vec![
                0x53, 0x43, 0x50, 0x31, 0x02, 0x34, 0x22, 0x11, 0x04, 0x03, 0x02, 0x01, 0x14, 0x00,
                0x00, 0x00, 0x08, 0x07, 0x06, 0x05, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
                0x04, 0x03, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x20, 0x00, 0x05, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x8f, 0xa9, 0x80,
            ]
        );
    }

    #[test]
    fn affine_and_explicit_metadata_round_trip_exactly() {
        for encoding in [
            MetadataEncodingR2::AffineWithOverrides,
            MetadataEncodingR2::Explicit,
        ] {
            let mut batch = batch(encoding, 4);
            batch.row_metadata[2].valid_flags ^= SOURCE_SEQUENCE_VALID;
            batch.row_metadata[3].source_sequence += 7;
            let message = MessageV2R2::StreamSampleBatch(batch);
            assert_eq!(
                MessageV2R2::decode(message.message_type(), &message.encode_payload().unwrap())
                    .unwrap(),
                message
            );
        }
    }

    #[test]
    fn compressed_16_channel_8khz_budget_is_safe_but_explicit_is_not() {
        let mut compressed = batch(MetadataEncodingR2::AffineWithOverrides, 128);
        compressed.channel_ids = (0..16).collect();
        compressed.sample_data = vec![0; 16 * 2 * 128];
        let compressed_utilization =
            calculate_r2_serial_utilization(&compressed, 8_000, 4_000_000).unwrap();
        assert!(compressed_utilization <= 0.70);

        let mut sparse = compressed.clone();
        sparse.row_metadata[17].valid_flags ^= SOURCE_SEQUENCE_VALID;
        sparse.row_metadata[91].source_sequence += 1;
        assert!(calculate_r2_serial_utilization(&sparse, 8_000, 4_000_000).unwrap() <= 0.70);

        let mut explicit = compressed;
        explicit.metadata_encoding = MetadataEncodingR2::Explicit;
        assert!(calculate_r2_serial_utilization(&explicit, 8_000, 4_000_000).unwrap() > 0.70);

        let mut fast = batch(MetadataEncodingR2::AffineWithOverrides, 128);
        fast.channel_ids = (0..8).collect();
        fast.sample_data = vec![0; 8 * 2 * 128];
        assert!(calculate_r2_serial_utilization(&fast, 32_000, 4_000_000).unwrap() > 0.70);
    }

    #[test]
    fn validates_exact_logical_cycle_steps() {
        table().validate().unwrap();
        let mut invalid = table();
        invalid.streams[1].logical_cycle_step = 1;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn validates_atomic_configuration_without_partial_state() {
        let channels = ChannelTable {
            revision: 1,
            channels: vec![descriptor(0), descriptor(1), descriptor(2)],
        };
        let valid = ConfigureStreamsR2 {
            transaction_id: 1,
            subscriptions: vec![
                StreamSubscriptionR2 {
                    stream_id: 1,
                    batch_samples: 8,
                    channel_mask: 1,
                },
                StreamSubscriptionR2 {
                    stream_id: 2,
                    batch_samples: 8,
                    channel_mask: 2,
                },
                StreamSubscriptionR2 {
                    stream_id: 3,
                    batch_samples: 8,
                    channel_mask: 4,
                },
            ],
        };
        validate_configure_streams_r2_for_device(
            &valid,
            &table(),
            &channels,
            128,
            MAX_PAYLOAD_LEN as u32,
        )
        .unwrap();
        let mut invalid = valid;
        invalid.subscriptions[1].channel_mask = 1;
        assert!(validate_configure_streams_r2_for_device(
            &invalid,
            &table(),
            &channels,
            128,
            MAX_PAYLOAD_LEN as u32,
        )
        .is_err());
    }

    fn descriptor(channel_id: u16) -> ChannelDescriptor {
        ChannelDescriptor {
            channel_id,
            kind: ChannelKind::Analog,
            wire_format: WireFormat::I16,
            scale: 1.0,
            offset: 0.0,
            unit: String::new(),
            name: format!("CH{channel_id}"),
        }
    }
}
