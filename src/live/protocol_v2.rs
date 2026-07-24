//! SCP1 V2 stream semantics.
//!
//! V2 deliberately leaves the frozen V1 messages untouched. A V2 session uses
//! version-2 frames and these new message types after capability negotiation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::protocol::{
    ChannelKind, ChannelTable, Frame, ProtocolError, WireFormat, MAX_BATCH_SAMPLES,
    MAX_CHANNEL_COUNT, MAX_PAYLOAD_LEN, PROTOCOL_VERSION_V2,
};
pub use super::snapshot::{
    SnapshotMeta, ADC_SAMPLE_VALID, APPLIED_SEQUENCE_VALID, CLA_RESULT_VALID, FROZEN_ROW,
    SNAPSHOT_KNOWN_FLAGS, SNAPSHOT_VALID, SOURCE_SEQUENCE_VALID,
};

/// Advertised in the existing HELLO / HELLO_ACK capability bitfield.
pub const CAPABILITY_V2_STREAMS: u32 = 1 << 3;

pub const MSG_STREAM_TABLE: u8 = 0x30;
pub const MSG_CONFIGURE_STREAM: u8 = 0x31;
pub const MSG_SAMPLE_BATCH_V2: u8 = 0x32;
/// Compatibility name retained for the initial V2 foundation.
pub const MSG_STREAM_SAMPLE_BATCH: u8 = MSG_SAMPLE_BATCH_V2;
pub const MSG_ARM_CAPTURE: u8 = 0x40;
pub const MSG_MANUAL_TRIGGER: u8 = 0x41;
pub const MSG_CANCEL_CAPTURE: u8 = 0x42;
pub const MSG_CAPTURE_STATUS: u8 = 0x43;
pub const MSG_CAPTURE_BEGIN: u8 = 0x44;
pub const MSG_CAPTURE_DATA: u8 = 0x45;
pub const MSG_CAPTURE_END: u8 = 0x46;

pub const MAX_STREAM_COUNT: usize = 64;
pub const MAX_CAUSAL_RELATION_COUNT: usize = 64;
pub const MAX_CAPTURE_ROWS: u32 = 1_048_576;
/// Capture limits are deliberately protocol constants: reject malformed or
/// hostile uploads before allocating their advertised amount of memory.
pub const MAX_CAPTURE_BLOCKS: u32 = 4_096;
pub const MAX_CAPTURE_BLOCK_ROWS: u16 = 4_096;
pub const MAX_CAPTURE_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

pub const VALID_FLAG_CLA_COMPLETE: u32 = CLA_RESULT_VALID;
pub const VALID_FLAG_ADC_VALID: u32 = ADC_SAMPLE_VALID;
pub const VALID_FLAG_DATA_FROZEN: u32 = FROZEN_ROW;
pub const VALID_FLAG_SOURCE_VALID: u32 = SOURCE_SEQUENCE_VALID;
pub const VALID_FLAG_APPLIED_VALID: u32 = APPLIED_SEQUENCE_VALID;
pub const VALID_FLAG_KNOWN_MASK: u32 = SNAPSHOT_KNOWN_FLAGS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SampleDomain {
    Fast32k = 0,
    Control8k = 1,
    Slow1k = 2,
}

impl SampleDomain {
    pub const fn fixed_sample_rate_hz(self) -> u32 {
        match self {
            Self::Fast32k => 32_000,
            Self::Control8k => 8_000,
            Self::Slow1k => 1_000,
        }
    }

    pub const fn fixed_capture_phase(self) -> CapturePhase {
        match self {
            Self::Fast32k => CapturePhase::AfterClaComplete,
            Self::Control8k => CapturePhase::ControlCycleEnd,
            Self::Slow1k => CapturePhase::LogicTaskEnd,
        }
    }
}

impl TryFrom<u8> for SampleDomain {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Fast32k),
            1 => Ok(Self::Control8k),
            2 => Ok(Self::Slow1k),
            _ => invalid(format!("unknown sample domain {value}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum CapturePhase {
    AfterClaComplete = 0,
    ControlCycleEnd = 1,
    LogicTaskEnd = 2,
}

impl TryFrom<u8> for CapturePhase {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::AfterClaComplete),
            1 => Ok(Self::ControlCycleEnd),
            2 => Ok(Self::LogicTaskEnd),
            _ => invalid(format!("unknown capture phase {value}")),
        }
    }
}

/// DSP execution unit responsible for a V2 channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SignalOwner {
    Cpu1 = 0,
    Cpu1Cla1 = 1,
    Cpu2 = 2,
}

impl TryFrom<u8> for SignalOwner {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Cpu1),
            1 => Ok(Self::Cpu1Cla1),
            2 => Ok(Self::Cpu2),
            _ => invalid(format!("unknown signal owner {value}")),
        }
    }
}

/// Compatibility spelling used by the initial V2 table codec.
pub type ProcessingUnit = SignalOwner;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SignalRole {
    PhysicalSample = 0,
    ControlInput = 1,
    ControlOutput = 2,
    Command = 3,
    AppliedCommand = 4,
    State = 5,
    Fault = 6,
    Diagnostic = 7,
    Metadata = 8,
}

impl TryFrom<u8> for SignalRole {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::PhysicalSample),
            1 => Ok(Self::ControlInput),
            2 => Ok(Self::ControlOutput),
            3 => Ok(Self::Command),
            4 => Ok(Self::AppliedCommand),
            5 => Ok(Self::State),
            6 => Ok(Self::Fault),
            7 => Ok(Self::Diagnostic),
            8 => Ok(Self::Metadata),
            _ => invalid(format!("unknown signal role {value}")),
        }
    }
}

/// One stream has exactly one sampling domain, frequency, and capture phase.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamDescriptor {
    pub stream_id: u16,
    pub domain: SampleDomain,
    pub capture_phase: CapturePhase,
    pub sample_rate_hz: u32,
    /// Non-zero logical control-cycle namespace used to correlate causal indices.
    pub consistency_group: u16,
    pub channel_ids: Vec<u16>,
}

/// V2 extension; the V1 `ChannelDescriptor` byte layout remains unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelDescriptorV2 {
    pub base: super::protocol::ChannelDescriptor,
    pub owner: SignalOwner,
    pub domain: SampleDomain,
    pub capture_phase: CapturePhase,
    pub consistency_group: u16,
    pub role: SignalRole,
}

/// Binds every real-time channel to one, and only one, V2 stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamChannelBinding {
    pub channel_id: u16,
    pub stream_id: u16,
    pub owner: SignalOwner,
    pub role: SignalRole,
}

/// Declares a CPU/CLA causal pipeline in one consistency group.
///
/// Offsets use the logical control-cycle index carried by `StreamSampleBatch`,
/// not the per-stream sample index. For example, `(0, 1)` means a result uses
/// input cycle N and CPU1 applies that result in cycle N+1.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CausalRelation {
    pub input_stream_id: u16,
    pub result_stream_id: u16,
    pub application_stream_id: u16,
    pub result_input_offset: i16,
    pub application_result_offset: i16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamTable {
    pub revision: u32,
    pub streams: Vec<StreamDescriptor>,
    pub bindings: Vec<StreamChannelBinding>,
    pub causal_relations: Vec<CausalRelation>,
}

impl StreamTable {
    pub fn stream(&self, stream_id: u16) -> Option<&StreamDescriptor> {
        self.streams
            .iter()
            .find(|stream| stream.stream_id == stream_id)
    }

    pub fn binding(&self, channel_id: u16) -> Option<&StreamChannelBinding> {
        self.bindings
            .iter()
            .find(|binding| binding.channel_id == channel_id)
    }

    /// Reconstructs V2 descriptors from the frozen V1 channel base table plus
    /// the V2 binding metadata carried by STREAM_TABLE.
    pub fn channel_descriptors_v2(
        &self,
        channels: &ChannelTable,
    ) -> Result<Vec<ChannelDescriptorV2>, ProtocolError> {
        self.validate_against_channels(channels)?;
        self.bindings
            .iter()
            .map(|binding| {
                let stream = self
                    .stream(binding.stream_id)
                    .expect("validated stream binding");
                Ok(ChannelDescriptorV2 {
                    base: channels
                        .channel(binding.channel_id)
                        .expect("validated channel binding")
                        .clone(),
                    owner: binding.owner,
                    domain: stream.domain,
                    capture_phase: stream.capture_phase,
                    consistency_group: stream.consistency_group,
                    role: binding.role,
                })
            })
            .collect()
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_count(
            self.streams.len(),
            1,
            MAX_STREAM_COUNT,
            "stream descriptor count",
        )?;
        validate_count(
            self.bindings.len(),
            1,
            MAX_CHANNEL_COUNT,
            "stream channel binding count",
        )?;
        validate_count(
            self.causal_relations.len(),
            0,
            MAX_CAUSAL_RELATION_COUNT,
            "causal relation count",
        )?;

        let mut stream_ids = BTreeSet::new();
        let mut descriptor_channel_ids = BTreeSet::new();
        for stream in &self.streams {
            if stream.stream_id == 0 {
                return invalid("stream id must be non-zero");
            }
            if !stream_ids.insert(stream.stream_id) {
                return invalid(format!("duplicate stream id {}", stream.stream_id));
            }
            if stream.consistency_group == 0 {
                return invalid(format!(
                    "stream {} must declare a non-zero consistency group",
                    stream.stream_id
                ));
            }
            if stream.sample_rate_hz != stream.domain.fixed_sample_rate_hz() {
                return invalid(format!(
                    "stream {} has rate {}, but {:?} requires {} Hz",
                    stream.stream_id,
                    stream.sample_rate_hz,
                    stream.domain,
                    stream.domain.fixed_sample_rate_hz()
                ));
            }
            if stream.capture_phase != stream.domain.fixed_capture_phase() {
                return invalid(format!(
                    "stream {} has phase {:?}, but {:?} requires {:?}",
                    stream.stream_id,
                    stream.capture_phase,
                    stream.domain,
                    stream.domain.fixed_capture_phase()
                ));
            }
            if stream.channel_ids.is_empty() {
                return invalid(format!("stream {} has no channel ids", stream.stream_id));
            }
            let mut stream_channels = BTreeSet::new();
            for channel_id in &stream.channel_ids {
                if usize::from(*channel_id) >= MAX_CHANNEL_COUNT {
                    return invalid(format!("stream channel id {channel_id} is out of range"));
                }
                if !stream_channels.insert(*channel_id) {
                    return invalid(format!(
                        "stream {} repeats channel {channel_id}",
                        stream.stream_id
                    ));
                }
                if !descriptor_channel_ids.insert(*channel_id) {
                    return invalid(format!(
                        "channel {channel_id} appears in more than one stream descriptor"
                    ));
                }
            }
        }

        let mut binding_channel_ids = BTreeSet::new();
        for binding in &self.bindings {
            if usize::from(binding.channel_id) >= MAX_CHANNEL_COUNT {
                return invalid(format!(
                    "stream binding channel id {} is out of range",
                    binding.channel_id
                ));
            }
            if !binding_channel_ids.insert(binding.channel_id) {
                return invalid(format!(
                    "channel {} is bound to more than one stream",
                    binding.channel_id
                ));
            }
            if self.stream(binding.stream_id).is_none() {
                return invalid(format!(
                    "channel {} references unknown stream {}",
                    binding.channel_id, binding.stream_id
                ));
            }
        }
        for stream in &self.streams {
            let descriptor_set = stream.channel_ids.iter().copied().collect::<BTreeSet<_>>();
            let binding_set = self
                .bindings
                .iter()
                .filter(|binding| binding.stream_id == stream.stream_id)
                .map(|binding| binding.channel_id)
                .collect::<BTreeSet<_>>();
            if descriptor_set != binding_set {
                return invalid(format!(
                    "stream {} channel ids and bindings disagree",
                    stream.stream_id
                ));
            }
        }

        let mut relations = BTreeSet::new();
        let mut result_definitions = BTreeMap::new();
        for relation in &self.causal_relations {
            let input = self.stream(relation.input_stream_id).ok_or_else(|| {
                invalid_error(format!(
                    "causal relation references unknown input stream {}",
                    relation.input_stream_id
                ))
            })?;
            let result = self.stream(relation.result_stream_id).ok_or_else(|| {
                invalid_error(format!(
                    "causal relation references unknown result stream {}",
                    relation.result_stream_id
                ))
            })?;
            let application = self.stream(relation.application_stream_id).ok_or_else(|| {
                invalid_error(format!(
                    "causal relation references unknown application stream {}",
                    relation.application_stream_id
                ))
            })?;
            if relation.input_stream_id == relation.result_stream_id
                || relation.result_stream_id == relation.application_stream_id
            {
                return invalid(
                    "causal result stream must be distinct from input and application streams",
                );
            }
            if input.consistency_group != result.consistency_group
                || result.consistency_group != application.consistency_group
            {
                return invalid("causal relation streams must share one consistency group");
            }
            let relation_key = (
                relation.input_stream_id,
                relation.result_stream_id,
                relation.application_stream_id,
                relation.result_input_offset,
                relation.application_result_offset,
            );
            if !relations.insert(relation_key) {
                return invalid("duplicate causal relation");
            }
            if result_definitions
                .insert(relation.result_stream_id, relation_key)
                .is_some()
            {
                return invalid(format!(
                    "result stream {} has more than one causal relation",
                    relation.result_stream_id
                ));
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
                    "stream binding references unknown channel {}",
                    binding.channel_id
                ));
            }
        }
        Ok(())
    }
}

/// Validates the exact tick period advertised by a fixed-rate V2 stream.
///
/// SCP1 V2 deliberately freezes the rule to an integral division.  A device
/// whose timer cannot represent a domain period exactly must reject the V2
/// stream instead of silently rounding and accumulating timestamp drift.
pub fn validate_stream_sample_period(
    tick_hz: u64,
    descriptor: &StreamDescriptor,
    sample_period_ticks: u32,
) -> Result<(), ProtocolError> {
    if tick_hz == 0 {
        return invalid("HELLO_ACK tick_hz must be non-zero for SCP1 V2 streams");
    }
    let rate = u64::from(descriptor.sample_rate_hz);
    if !tick_hz.is_multiple_of(rate) {
        return invalid(format!(
            "tick_hz {tick_hz} is not exactly divisible by stream {} rate {}",
            descriptor.stream_id, descriptor.sample_rate_hz
        ));
    }
    let expected = tick_hz / rate;
    if expected == 0 || expected > u64::from(u32::MAX) {
        return invalid("fixed stream sample period is outside the wire range");
    }
    if u64::from(sample_period_ticks) != expected {
        return invalid(format!(
            "stream {} sample period {sample_period_ticks} does not match fixed period {expected}",
            descriptor.stream_id
        ));
    }
    Ok(())
}

/// V2 stream configuration. There is no caller-chosen sample rate: the stream
/// descriptor fixes it, and selected channels must all belong to `stream_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigureStream {
    pub stream_id: u16,
    pub batch_samples: u16,
    pub channel_mask: u64,
}

/// Per-row metadata is transmitted once for every sampled row, rather than
/// being duplicated in each ordinary signal channel. It is the logical
/// metadata-channel set `META_ROW_SEQ`, `META_LOGICAL_CYCLE_SEQ`, `META_SOURCE_SEQ`,
/// `META_APPLIED_SEQ`, `META_VALID_FLAGS`, and `META_CLA_COMPLETED_SEQ`.
pub type StreamRowMetadata = SnapshotMeta;

impl SnapshotMeta {
    pub fn source_alignment(&self) -> CausalAlignment {
        causal_alignment(
            self.logical_cycle_sequence,
            self.source_sequence,
            self.valid_flags,
            VALID_FLAG_SOURCE_VALID,
        )
    }

    pub fn applied_alignment(&self) -> CausalAlignment {
        causal_alignment(
            self.logical_cycle_sequence,
            self.applied_sequence,
            self.valid_flags,
            VALID_FLAG_APPLIED_VALID,
        )
    }
}

/// A logical causal relationship, not a physical simultaneity claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CausalAlignment {
    SameShot,
    PreviousShot,
    FutureShot,
    Invalid,
    Mismatch,
}

impl CausalAlignment {
    /// Stable UI wording for a causal relationship. This deliberately avoids
    /// any claim that two CPU domains were physically simultaneous.
    pub const fn chinese_label(self) -> &'static str {
        match self {
            Self::SameShot => "同拍",
            Self::PreviousShot => "上一拍",
            Self::FutureShot | Self::Mismatch => "序号不匹配",
            Self::Invalid => "无效",
        }
    }
}

/// V2 samples carry a single stream and per-row causal metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamSampleBatch {
    pub stream_id: u16,
    pub stream_revision: u32,
    pub domain: SampleDomain,
    pub capture_phase: CapturePhase,
    pub consistency_group: u16,
    pub first_row_sequence: u64,
    pub sample_period_ticks: u32,
    pub row_count: u16,
    pub channel_ids: Vec<u16>,
    pub sample_data: Vec<u8>,
    pub row_metadata: Vec<StreamRowMetadata>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedStreamSampleBatch {
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
    pub row_metadata: Vec<StreamRowMetadata>,
    pub raw_frame: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum CaptureTriggerKind {
    Manual = 0,
    Edge = 1,
    FaultFlag = 2,
}

impl TryFrom<u8> for CaptureTriggerKind {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Manual),
            1 => Ok(Self::Edge),
            2 => Ok(Self::FaultFlag),
            _ => invalid(format!("unknown capture trigger kind {value}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum CaptureEdge {
    Rising = 0,
    Falling = 1,
    Either = 2,
}

impl TryFrom<u8> for CaptureEdge {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Rising),
            1 => Ok(Self::Falling),
            2 => Ok(Self::Either),
            _ => invalid(format!("unknown capture edge {value}")),
        }
    }
}

/// DSP-side trigger definition. `channel_id` is used for analog triggers and
/// `flag_mask` / `flag_value` are used for hardware-flag triggers.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureTrigger {
    pub kind: CaptureTriggerKind,
    pub channel_id: u16,
    pub level: f32,
    pub edge: CaptureEdge,
    pub hysteresis: f32,
    pub flag_mask: u32,
    pub flag_value: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArmCapture {
    pub capture_id: u32,
    pub stream_id: u16,
    pub pretrigger_rows: u32,
    pub posttrigger_rows: u32,
    pub timeout_samples: u32,
    pub trigger: CaptureTrigger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManualTrigger {
    pub capture_id: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CancelCapture {
    pub capture_id: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum CaptureState {
    Idle = 0,
    Armed = 1,
    Triggered = 2,
    PostCapture = 3,
    Complete = 4,
    Uploading = 5,
    Cancelled = 6,
    Timeout = 7,
    BufferOverrun = 8,
    InvalidConfig = 9,
    DeviceReset = 10,
}

impl TryFrom<u8> for CaptureState {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Idle),
            1 => Ok(Self::Armed),
            2 => Ok(Self::Triggered),
            3 => Ok(Self::PostCapture),
            4 => Ok(Self::Complete),
            5 => Ok(Self::Uploading),
            6 => Ok(Self::Cancelled),
            7 => Ok(Self::Timeout),
            8 => Ok(Self::BufferOverrun),
            9 => Ok(Self::InvalidConfig),
            10 => Ok(Self::DeviceReset),
            _ => invalid(format!("unknown capture state {value}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureStatus {
    pub capture_id: u32,
    pub state: CaptureState,
    pub captured_rows: u32,
    pub dropped_rows: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureBegin {
    pub capture_id: u32,
    pub stream_id: u16,
    pub row_count: u32,
    pub trigger_row_seq: u64,
}

/// Capture data is uploaded as independently-valid stream batches so the
/// client can decode, validate, and persist it using the normal V2 data path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureData {
    pub capture_id: u32,
    pub block_index: u32,
    pub batch: StreamSampleBatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureEnd {
    pub capture_id: u32,
    pub state: CaptureState,
    pub uploaded_rows: u32,
    pub dropped_rows: u32,
    pub total_blocks: u32,
    pub total_samples: u32,
    pub integrity_summary: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MessageV2 {
    StreamTable(StreamTable),
    ConfigureStream(ConfigureStream),
    StreamSampleBatch(StreamSampleBatch),
    ArmCapture(ArmCapture),
    ManualTrigger(ManualTrigger),
    CancelCapture(CancelCapture),
    CaptureStatus(CaptureStatus),
    CaptureBegin(CaptureBegin),
    CaptureData(CaptureData),
    CaptureEnd(CaptureEnd),
}

impl MessageV2 {
    pub fn message_type(&self) -> u8 {
        match self {
            Self::StreamTable(_) => MSG_STREAM_TABLE,
            Self::ConfigureStream(_) => MSG_CONFIGURE_STREAM,
            Self::StreamSampleBatch(_) => MSG_STREAM_SAMPLE_BATCH,
            Self::ArmCapture(_) => MSG_ARM_CAPTURE,
            Self::ManualTrigger(_) => MSG_MANUAL_TRIGGER,
            Self::CancelCapture(_) => MSG_CANCEL_CAPTURE,
            Self::CaptureStatus(_) => MSG_CAPTURE_STATUS,
            Self::CaptureBegin(_) => MSG_CAPTURE_BEGIN,
            Self::CaptureData(_) => MSG_CAPTURE_DATA,
            Self::CaptureEnd(_) => MSG_CAPTURE_END,
        }
    }

    pub fn encode_payload(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut bytes = Vec::new();
        match self {
            Self::StreamTable(table) => encode_stream_table(table, &mut bytes)?,
            Self::ConfigureStream(configure) => {
                validate_configure_stream_shape(configure)?;
                put_u16(&mut bytes, configure.stream_id);
                put_u16(&mut bytes, configure.batch_samples);
                put_u64(&mut bytes, configure.channel_mask);
            }
            Self::StreamSampleBatch(batch) => encode_stream_sample_batch(batch, &mut bytes)?,
            Self::ArmCapture(capture) => encode_arm_capture(capture, &mut bytes)?,
            Self::ManualTrigger(trigger) => put_u32(&mut bytes, trigger.capture_id),
            Self::CancelCapture(cancel) => put_u32(&mut bytes, cancel.capture_id),
            Self::CaptureStatus(status) => encode_capture_status(status, &mut bytes),
            Self::CaptureBegin(begin) => encode_capture_begin(begin, &mut bytes),
            Self::CaptureData(data) => encode_capture_data(data, &mut bytes)?,
            Self::CaptureEnd(end) => encode_capture_end(end, &mut bytes),
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
            MSG_STREAM_TABLE => Self::StreamTable(decode_stream_table(&mut reader)?),
            MSG_CONFIGURE_STREAM => {
                let configure = ConfigureStream {
                    stream_id: reader.u16()?,
                    batch_samples: reader.u16()?,
                    channel_mask: reader.u64()?,
                };
                validate_configure_stream_shape(&configure)?;
                Self::ConfigureStream(configure)
            }
            MSG_STREAM_SAMPLE_BATCH => {
                Self::StreamSampleBatch(decode_stream_sample_batch(&mut reader)?)
            }
            MSG_ARM_CAPTURE => Self::ArmCapture(decode_arm_capture(&mut reader)?),
            MSG_MANUAL_TRIGGER => Self::ManualTrigger(ManualTrigger {
                capture_id: reader.u32()?,
            }),
            MSG_CANCEL_CAPTURE => Self::CancelCapture(CancelCapture {
                capture_id: reader.u32()?,
            }),
            MSG_CAPTURE_STATUS => Self::CaptureStatus(decode_capture_status(&mut reader)?),
            MSG_CAPTURE_BEGIN => Self::CaptureBegin(decode_capture_begin(&mut reader)?),
            MSG_CAPTURE_DATA => Self::CaptureData(decode_capture_data(&mut reader)?),
            MSG_CAPTURE_END => Self::CaptureEnd(decode_capture_end(&mut reader)?),
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
        let message_type = self.message_type();
        Ok(Frame::new_v2(
            message_type,
            flags,
            sequence,
            session_id,
            timestamp_ticks,
            self.encode_payload()?,
        ))
    }
}

/// Computes the frozen CAPTURE_END integrity summary.
///
/// The input is the little-endian capture id followed by the encoded
/// CAPTURE_DATA payloads in ascending block-index order.  The payload bytes,
/// rather than decoded floating-point values, make this independent of host
/// architecture and preserve exactly what the DSP uploaded.
pub fn capture_integrity_summary(
    capture_id: u32,
    blocks: &[CaptureData],
) -> Result<u32, ProtocolError> {
    let mut sorted = blocks.to_vec();
    sorted.sort_by_key(|block| block.block_index);
    let mut bytes = capture_id.to_le_bytes().to_vec();
    for block in sorted {
        bytes.extend_from_slice(&MessageV2::CaptureData(block).encode_payload()?);
        if bytes.len() > MAX_CAPTURE_PAYLOAD_BYTES {
            return Err(ProtocolError::PayloadTooLarge(bytes.len()));
        }
    }
    Ok(crc32c(&bytes))
}

/// Validates the negotiated V2 configuration and rejects any mixed-domain mask.
pub fn validate_configure_stream_for_device(
    configure: &ConfigureStream,
    streams: &StreamTable,
    channels: &ChannelTable,
    max_batch_samples: u16,
    max_payload: u32,
) -> Result<(), ProtocolError> {
    validate_configure_stream_shape(configure)?;
    streams.validate_against_channels(channels)?;
    if max_batch_samples == 0 || max_batch_samples as usize > MAX_BATCH_SAMPLES {
        return invalid(format!(
            "device maximum batch samples {max_batch_samples} is outside 1..={MAX_BATCH_SAMPLES}"
        ));
    }
    if configure.batch_samples > max_batch_samples {
        return invalid(format!(
            "batch samples {} exceeds device maximum {max_batch_samples}",
            configure.batch_samples
        ));
    }
    let stream = streams.stream(configure.stream_id).ok_or_else(|| {
        invalid_error(format!("unknown configured stream {}", configure.stream_id))
    })?;
    let selected = channels
        .channels
        .iter()
        .filter(|channel| configure.channel_mask & (1_u64 << channel.channel_id) != 0)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return invalid("channel mask selects no device channels");
    }
    for channel in &selected {
        let binding = streams.binding(channel.channel_id).ok_or_else(|| {
            invalid_error(format!(
                "channel {} has no stream binding",
                channel.channel_id
            ))
        })?;
        if binding.stream_id != stream.stream_id {
            return invalid(format!(
                "channel {} belongs to stream {}, not configured stream {}",
                channel.channel_id, binding.stream_id, stream.stream_id
            ));
        }
    }
    let known_mask = channels
        .channels
        .iter()
        .fold(0_u64, |mask, channel| mask | (1_u64 << channel.channel_id));
    if configure.channel_mask & !known_mask != 0 {
        return invalid(format!(
            "channel mask 0x{:016x} selects unknown device channels",
            configure.channel_mask
        ));
    }
    if max_payload == 0 || max_payload as usize > MAX_PAYLOAD_LEN {
        return invalid(format!(
            "maximum payload {max_payload} is outside 1..={MAX_PAYLOAD_LEN}"
        ));
    }
    let bytes_per_sample = selected.iter().try_fold(0_usize, |total, channel| {
        total
            .checked_add(channel.wire_format.byte_width())
            .ok_or(ProtocolError::LengthOverflow)
    })?;
    let payload_len = 26_usize
        .checked_add(
            selected
                .len()
                .checked_mul(2)
                .ok_or(ProtocolError::LengthOverflow)?,
        )
        .and_then(|value| {
            usize::from(configure.batch_samples)
                .checked_mul(bytes_per_sample)
                .and_then(|sample_data_len| value.checked_add(sample_data_len))
                .and_then(|value| {
                    usize::from(configure.batch_samples)
                        .checked_mul(SnapshotMeta::ENCODED_LEN)
                        .and_then(|metadata_len| value.checked_add(metadata_len))
                })
        })
        .ok_or(ProtocolError::LengthOverflow)?;
    if payload_len > max_payload as usize {
        return invalid(format!(
            "configured stream payload {payload_len} exceeds negotiated maximum {max_payload}"
        ));
    }
    Ok(())
}

pub fn decode_stream_sample_frame(
    frame: &Frame,
    channels: &ChannelTable,
    streams: &StreamTable,
) -> Result<DecodedStreamSampleBatch, ProtocolError> {
    if frame.version != PROTOCOL_VERSION_V2 {
        return Err(ProtocolError::UnsupportedVersion(frame.version));
    }
    if frame.message_type != MSG_STREAM_SAMPLE_BATCH {
        return Err(ProtocolError::UnexpectedMessageType {
            expected: MSG_STREAM_SAMPLE_BATCH,
            actual: frame.message_type,
        });
    }
    streams.validate_against_channels(channels)?;
    let MessageV2::StreamSampleBatch(batch) =
        MessageV2::decode(frame.message_type, &frame.payload)?
    else {
        return Err(ProtocolError::UnexpectedMessageType {
            expected: MSG_STREAM_SAMPLE_BATCH,
            actual: frame.message_type,
        });
    };
    let stream = streams.stream(batch.stream_id).ok_or_else(|| {
        invalid_error(format!(
            "sample batch references unknown stream {}",
            batch.stream_id
        ))
    })?;
    if batch.stream_revision != streams.revision {
        return invalid(format!(
            "stream sample batch references stream revision {}, current revision is {}",
            batch.stream_revision, streams.revision
        ));
    }
    if batch.domain != stream.domain
        || batch.capture_phase != stream.capture_phase
        || batch.consistency_group != stream.consistency_group
    {
        return invalid(
            "stream sample batch domain, phase, or consistency group disagrees with descriptor",
        );
    }
    let descriptors = batch
        .channel_ids
        .iter()
        .map(|channel_id| {
            let descriptor = channels.channel(*channel_id).ok_or_else(|| {
                invalid_error(format!(
                    "stream sample batch references unknown channel {channel_id}"
                ))
            })?;
            let binding = streams.binding(*channel_id).ok_or_else(|| {
                invalid_error(format!("channel {channel_id} has no stream binding"))
            })?;
            if binding.stream_id != stream.stream_id {
                Err(invalid_error(format!(
                    "stream sample batch {} includes channel {} from stream {}",
                    stream.stream_id, channel_id, binding.stream_id
                )))
            } else {
                Ok(descriptor)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let bytes_per_sample = descriptors.iter().try_fold(0_usize, |total, descriptor| {
        total
            .checked_add(descriptor.wire_format.byte_width())
            .ok_or(ProtocolError::LengthOverflow)
    })?;
    let expected_data_len = bytes_per_sample
        .checked_mul(usize::from(batch.row_count))
        .ok_or(ProtocolError::LengthOverflow)?;
    if batch.sample_data.len() != expected_data_len {
        return invalid(format!(
            "stream sample data length mismatch: expected {expected_data_len}, got {}",
            batch.sample_data.len()
        ));
    }
    let last_sample_offset = u64::from(batch.row_count - 1);
    let tick_offset = u64::from(batch.sample_period_ticks)
        .checked_mul(last_sample_offset)
        .ok_or_else(|| invalid_error("sample timestamp offset overflow"))?;
    frame
        .timestamp_ticks
        .checked_add(tick_offset)
        .ok_or_else(|| invalid_error("sample timestamp overflow"))?;
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
    Ok(DecodedStreamSampleBatch {
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
        row_metadata: batch.row_metadata,
        raw_frame: frame.encode()?,
    })
}

fn encode_stream_table(table: &StreamTable, bytes: &mut Vec<u8>) -> Result<(), ProtocolError> {
    table.validate()?;
    put_u32(bytes, table.revision);
    put_u16(
        bytes,
        u16::try_from(table.streams.len()).map_err(|_| invalid_error("too many streams"))?,
    );
    put_u16(
        bytes,
        u16::try_from(table.bindings.len())
            .map_err(|_| invalid_error("too many stream channel bindings"))?,
    );
    put_u16(
        bytes,
        u16::try_from(table.causal_relations.len())
            .map_err(|_| invalid_error("too many causal relations"))?,
    );
    put_u16(bytes, 0);
    for stream in &table.streams {
        put_u16(bytes, stream.stream_id);
        bytes.push(stream.domain as u8);
        bytes.push(stream.capture_phase as u8);
        put_u32(bytes, stream.sample_rate_hz);
        put_u16(bytes, stream.consistency_group);
        put_u16(
            bytes,
            u16::try_from(stream.channel_ids.len())
                .map_err(|_| invalid_error("too many stream channel ids"))?,
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

fn decode_stream_table(reader: &mut PayloadReader<'_>) -> Result<StreamTable, ProtocolError> {
    let revision = reader.u32()?;
    let stream_count = usize::from(reader.u16()?);
    let binding_count = usize::from(reader.u16()?);
    let causal_relation_count = usize::from(reader.u16()?);
    if reader.u16()? != 0 {
        return invalid("STREAM_TABLE reserved field must be zero");
    }
    validate_count(stream_count, 1, MAX_STREAM_COUNT, "stream descriptor count")?;
    validate_count(
        binding_count,
        1,
        MAX_CHANNEL_COUNT,
        "stream channel binding count",
    )?;
    validate_count(
        causal_relation_count,
        0,
        MAX_CAUSAL_RELATION_COUNT,
        "causal relation count",
    )?;
    let mut streams = Vec::with_capacity(stream_count);
    for _ in 0..stream_count {
        let stream_id = reader.u16()?;
        let domain = SampleDomain::try_from(reader.u8()?)?;
        let capture_phase = CapturePhase::try_from(reader.u8()?)?;
        let sample_rate_hz = reader.u32()?;
        let consistency_group = reader.u16()?;
        let channel_count = usize::from(reader.u16()?);
        validate_count(
            channel_count,
            1,
            MAX_CHANNEL_COUNT,
            "stream channel id count",
        )?;
        let mut channel_ids = Vec::with_capacity(channel_count);
        for _ in 0..channel_count {
            channel_ids.push(reader.u16()?);
        }
        streams.push(StreamDescriptor {
            stream_id,
            domain,
            capture_phase,
            sample_rate_hz,
            consistency_group,
            channel_ids,
        });
    }
    let mut bindings = Vec::with_capacity(binding_count);
    for _ in 0..binding_count {
        let channel_id = reader.u16()?;
        let stream_id = reader.u16()?;
        let owner = SignalOwner::try_from(reader.u8()?)?;
        let role = SignalRole::try_from(reader.u8()?)?;
        bindings.push(StreamChannelBinding {
            channel_id,
            stream_id,
            owner,
            role,
        });
    }
    let mut causal_relations = Vec::with_capacity(causal_relation_count);
    for _ in 0..causal_relation_count {
        causal_relations.push(CausalRelation {
            input_stream_id: reader.u16()?,
            result_stream_id: reader.u16()?,
            application_stream_id: reader.u16()?,
            result_input_offset: reader.i16()?,
            application_result_offset: reader.i16()?,
        });
    }
    let table = StreamTable {
        revision,
        streams,
        bindings,
        causal_relations,
    };
    table.validate()?;
    Ok(table)
}

fn encode_stream_sample_batch(
    batch: &StreamSampleBatch,
    bytes: &mut Vec<u8>,
) -> Result<(), ProtocolError> {
    validate_stream_sample_batch_header(batch)?;
    put_u16(bytes, batch.stream_id);
    put_u32(bytes, batch.stream_revision);
    bytes.push(batch.domain as u8);
    bytes.push(batch.capture_phase as u8);
    put_u16(bytes, batch.consistency_group);
    put_u64(bytes, batch.first_row_sequence);
    put_u16(bytes, batch.row_count);
    put_u32(bytes, batch.sample_period_ticks);
    put_u16(
        bytes,
        u16::try_from(batch.channel_ids.len())
            .map_err(|_| invalid_error("too many selected stream channels"))?,
    );
    for channel_id in &batch.channel_ids {
        put_u16(bytes, *channel_id);
    }
    bytes.extend_from_slice(&batch.sample_data);
    for row in &batch.row_metadata {
        put_u64(bytes, row.row_sequence);
        put_u64(bytes, row.logical_cycle_sequence);
        put_u64(bytes, row.source_sequence);
        put_u64(bytes, row.applied_sequence);
        put_u32(bytes, row.valid_flags);
    }
    Ok(())
}

fn decode_stream_sample_batch(
    reader: &mut PayloadReader<'_>,
) -> Result<StreamSampleBatch, ProtocolError> {
    let stream_id = reader.u16()?;
    let stream_revision = reader.u32()?;
    let domain = SampleDomain::try_from(reader.u8()?)?;
    let capture_phase = CapturePhase::try_from(reader.u8()?)?;
    let consistency_group = reader.u16()?;
    let first_row_sequence = reader.u64()?;
    let row_count = reader.u16()?;
    let sample_period_ticks = reader.u32()?;
    let channel_count = usize::from(reader.u16()?);
    validate_count(
        channel_count,
        1,
        MAX_CHANNEL_COUNT,
        "selected stream channel count",
    )?;
    let mut channel_ids = Vec::with_capacity(channel_count);
    for _ in 0..channel_count {
        channel_ids.push(reader.u16()?);
    }
    let channel_data_len = reader.remaining().len();
    let metadata_len = usize::from(row_count)
        .checked_mul(SnapshotMeta::ENCODED_LEN)
        .ok_or(ProtocolError::LengthOverflow)?;
    if channel_data_len < metadata_len {
        return invalid("stream sample batch is missing per-row metadata");
    }
    let signal_data_len = channel_data_len - metadata_len;
    let sample_data = reader
        .bytes(signal_data_len, "stream sample data")?
        .to_vec();
    let mut row_metadata = Vec::with_capacity(usize::from(row_count));
    for _ in 0..row_count {
        row_metadata.push(StreamRowMetadata {
            row_sequence: reader.u64()?,
            logical_cycle_sequence: reader.u64()?,
            source_sequence: reader.u64()?,
            applied_sequence: reader.u64()?,
            valid_flags: reader.u32()?,
        });
    }
    let batch = StreamSampleBatch {
        stream_id,
        stream_revision,
        domain,
        capture_phase,
        consistency_group,
        first_row_sequence,
        sample_period_ticks,
        row_count,
        channel_ids,
        sample_data,
        row_metadata,
    };
    validate_stream_sample_batch_header(&batch)?;
    Ok(batch)
}

fn encode_arm_capture(capture: &ArmCapture, bytes: &mut Vec<u8>) -> Result<(), ProtocolError> {
    validate_arm_capture(capture)?;
    put_u32(bytes, capture.capture_id);
    put_u16(bytes, capture.stream_id);
    put_u16(bytes, 0);
    put_u32(bytes, capture.pretrigger_rows);
    put_u32(bytes, capture.posttrigger_rows);
    put_u32(bytes, capture.timeout_samples);
    bytes.push(capture.trigger.kind as u8);
    bytes.push(capture.trigger.edge as u8);
    put_u16(bytes, capture.trigger.channel_id);
    put_f32(bytes, capture.trigger.level);
    put_f32(bytes, capture.trigger.hysteresis);
    put_u32(bytes, capture.trigger.flag_mask);
    put_u32(bytes, capture.trigger.flag_value);
    Ok(())
}

fn decode_arm_capture(reader: &mut PayloadReader<'_>) -> Result<ArmCapture, ProtocolError> {
    let capture_id = reader.u32()?;
    let stream_id = reader.u16()?;
    if reader.u16()? != 0 {
        return invalid("ARM_CAPTURE reserved field must be zero");
    }
    let pretrigger_rows = reader.u32()?;
    let posttrigger_rows = reader.u32()?;
    let timeout_samples = reader.u32()?;
    let kind = CaptureTriggerKind::try_from(reader.u8()?)?;
    let edge = CaptureEdge::try_from(reader.u8()?)?;
    let trigger = CaptureTrigger {
        kind,
        channel_id: reader.u16()?,
        level: reader.f32()?,
        edge,
        hysteresis: reader.f32()?,
        flag_mask: reader.u32()?,
        flag_value: reader.u32()?,
    };
    let capture = ArmCapture {
        capture_id,
        stream_id,
        pretrigger_rows,
        posttrigger_rows,
        timeout_samples,
        trigger,
    };
    validate_arm_capture(&capture)?;
    Ok(capture)
}

fn encode_capture_status(status: &CaptureStatus, bytes: &mut Vec<u8>) {
    put_u32(bytes, status.capture_id);
    bytes.push(status.state as u8);
    bytes.extend_from_slice(&[0; 3]);
    put_u32(bytes, status.captured_rows);
    put_u32(bytes, status.dropped_rows);
}

fn decode_capture_status(reader: &mut PayloadReader<'_>) -> Result<CaptureStatus, ProtocolError> {
    let capture_id = reader.u32()?;
    let state = CaptureState::try_from(reader.u8()?)?;
    if reader.bytes(3, "CAPTURE_STATUS reserved")? != [0, 0, 0] {
        return invalid("CAPTURE_STATUS reserved bytes must be zero");
    }
    Ok(CaptureStatus {
        capture_id,
        state,
        captured_rows: reader.u32()?,
        dropped_rows: reader.u32()?,
    })
}

fn encode_capture_begin(begin: &CaptureBegin, bytes: &mut Vec<u8>) {
    put_u32(bytes, begin.capture_id);
    put_u16(bytes, begin.stream_id);
    put_u16(bytes, 0);
    put_u32(bytes, begin.row_count);
    put_u64(bytes, begin.trigger_row_seq);
}

fn decode_capture_begin(reader: &mut PayloadReader<'_>) -> Result<CaptureBegin, ProtocolError> {
    let capture_id = reader.u32()?;
    let stream_id = reader.u16()?;
    if reader.u16()? != 0 {
        return invalid("CAPTURE_BEGIN reserved field must be zero");
    }
    let row_count = reader.u32()?;
    if row_count == 0 || row_count > MAX_CAPTURE_ROWS {
        return invalid(format!(
            "capture row count {row_count} is outside 1..={MAX_CAPTURE_ROWS}"
        ));
    }
    Ok(CaptureBegin {
        capture_id,
        stream_id,
        row_count,
        trigger_row_seq: reader.u64()?,
    })
}

fn encode_capture_data(data: &CaptureData, bytes: &mut Vec<u8>) -> Result<(), ProtocolError> {
    if data.capture_id == 0 {
        return invalid("capture id must be non-zero");
    }
    let mut batch = Vec::new();
    encode_stream_sample_batch(&data.batch, &mut batch)?;
    put_u32(bytes, data.capture_id);
    put_u32(bytes, data.block_index);
    put_u32(
        bytes,
        u32::try_from(batch.len()).map_err(|_| ProtocolError::PayloadTooLarge(batch.len()))?,
    );
    bytes.extend_from_slice(&batch);
    Ok(())
}

fn decode_capture_data(reader: &mut PayloadReader<'_>) -> Result<CaptureData, ProtocolError> {
    let capture_id = reader.u32()?;
    if capture_id == 0 {
        return invalid("capture id must be non-zero");
    }
    let block_index = reader.u32()?;
    let batch_len = reader.u32()? as usize;
    if batch_len > reader.remaining().len() {
        return invalid("CAPTURE_DATA batch length exceeds payload");
    }
    let mut batch_reader = PayloadReader::new(reader.bytes(batch_len, "capture data batch")?);
    let batch = decode_stream_sample_batch(&mut batch_reader)?;
    batch_reader.finish()?;
    Ok(CaptureData {
        capture_id,
        block_index,
        batch,
    })
}

fn encode_capture_end(end: &CaptureEnd, bytes: &mut Vec<u8>) {
    put_u32(bytes, end.capture_id);
    bytes.push(end.state as u8);
    bytes.extend_from_slice(&[0; 3]);
    put_u32(bytes, end.uploaded_rows);
    put_u32(bytes, end.dropped_rows);
    put_u32(bytes, end.total_blocks);
    put_u32(bytes, end.total_samples);
    put_u32(bytes, end.integrity_summary);
}

fn decode_capture_end(reader: &mut PayloadReader<'_>) -> Result<CaptureEnd, ProtocolError> {
    let capture_id = reader.u32()?;
    let state = CaptureState::try_from(reader.u8()?)?;
    if reader.bytes(3, "CAPTURE_END reserved")? != [0, 0, 0] {
        return invalid("CAPTURE_END reserved bytes must be zero");
    }
    Ok(CaptureEnd {
        capture_id,
        state,
        uploaded_rows: reader.u32()?,
        dropped_rows: reader.u32()?,
        total_blocks: reader.u32()?,
        total_samples: reader.u32()?,
        integrity_summary: reader.u32()?,
    })
}

fn validate_arm_capture(capture: &ArmCapture) -> Result<(), ProtocolError> {
    if capture.capture_id == 0 {
        return invalid("capture id must be non-zero");
    }
    let row_count = capture
        .pretrigger_rows
        .checked_add(capture.posttrigger_rows)
        .and_then(|count| count.checked_add(1))
        .ok_or(ProtocolError::LengthOverflow)?;
    if row_count > MAX_CAPTURE_ROWS {
        return invalid(format!(
            "capture row count {row_count} exceeds maximum {MAX_CAPTURE_ROWS}"
        ));
    }
    if !capture.trigger.level.is_finite() || !capture.trigger.hysteresis.is_finite() {
        return invalid("capture trigger level and hysteresis must be finite");
    }
    if capture.trigger.hysteresis < 0.0 {
        return invalid("capture trigger hysteresis must be non-negative");
    }
    if capture.trigger.kind == CaptureTriggerKind::FaultFlag && capture.trigger.flag_mask == 0 {
        return invalid("hardware capture trigger flag mask must be non-zero");
    }
    Ok(())
}

fn causal_alignment(
    row_seq: u64,
    related_seq: u64,
    valid_flags: u32,
    required_valid_flag: u32,
) -> CausalAlignment {
    if valid_flags & required_valid_flag == 0 {
        return CausalAlignment::Invalid;
    }
    if related_seq == row_seq {
        CausalAlignment::SameShot
    } else if related_seq.checked_add(1) == Some(row_seq) {
        CausalAlignment::PreviousShot
    } else if related_seq > row_seq {
        CausalAlignment::FutureShot
    } else {
        CausalAlignment::Mismatch
    }
}

fn validate_configure_stream_shape(configure: &ConfigureStream) -> Result<(), ProtocolError> {
    validate_count(
        usize::from(configure.batch_samples),
        1,
        MAX_BATCH_SAMPLES,
        "stream batch samples",
    )?;
    if configure.channel_mask == 0 {
        return invalid("stream channel mask must select at least one channel");
    }
    Ok(())
}

fn validate_stream_sample_batch_header(batch: &StreamSampleBatch) -> Result<(), ProtocolError> {
    if batch.stream_id == 0 || batch.stream_revision == 0 || batch.consistency_group == 0 {
        return invalid("stream id, stream revision, and consistency group must be non-zero");
    }
    if batch.capture_phase != batch.domain.fixed_capture_phase() {
        return invalid("stream sample batch has an invalid domain/capture phase combination");
    }
    if batch.sample_period_ticks == 0 {
        return invalid("stream sample period ticks must be greater than zero");
    }
    validate_count(
        usize::from(batch.row_count),
        1,
        MAX_BATCH_SAMPLES,
        "stream sample count",
    )?;
    if batch.row_metadata.len() != usize::from(batch.row_count) {
        return invalid(format!(
            "stream sample metadata count {} does not match sample count {}",
            batch.row_metadata.len(),
            batch.row_count
        ));
    }
    for row in &batch.row_metadata {
        if row.valid_flags & !VALID_FLAG_KNOWN_MASK != 0 {
            return invalid("stream row metadata contains unknown valid flag bits");
        }
    }
    if batch
        .row_metadata
        .first()
        .is_none_or(|row| row.row_sequence != batch.first_row_sequence)
    {
        return invalid("first row sequence does not match the first SnapshotMeta");
    }
    for pair in batch.row_metadata.windows(2) {
        if pair[1].row_sequence != pair[0].row_sequence.saturating_add(1) {
            return invalid("stream row sequences must be continuous within a batch");
        }
    }
    validate_count(
        batch.channel_ids.len(),
        1,
        MAX_CHANNEL_COUNT,
        "selected stream channel count",
    )?;
    let mut seen = 0_u64;
    for channel_id in &batch.channel_ids {
        if usize::from(*channel_id) >= MAX_CHANNEL_COUNT {
            return invalid(format!(
                "selected stream channel id {channel_id} is out of range"
            ));
        }
        let bit = 1_u64 << channel_id;
        if seen & bit != 0 {
            return invalid(format!("duplicate selected stream channel id {channel_id}"));
        }
        seen |= bit;
    }
    Ok(())
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

fn put_f32(bytes: &mut Vec<u8>, value: f32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn invalid<T>(message: impl Into<String>) -> Result<T, ProtocolError> {
    Err(invalid_error(message))
}

fn invalid_error(message: impl Into<String>) -> ProtocolError {
    ProtocolError::InvalidPayload(message.into())
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

    fn i16(&mut self) -> Result<i16, ProtocolError> {
        Ok(i16::from_le_bytes(
            self.bytes(2, "i16")?.try_into().expect("length checked"),
        ))
    }

    fn u16(&mut self) -> Result<u16, ProtocolError> {
        Ok(u16::from_le_bytes(
            self.bytes(2, "u16")?.try_into().expect("length checked"),
        ))
    }

    fn i32(&mut self) -> Result<i32, ProtocolError> {
        Ok(i32::from_le_bytes(
            self.bytes(4, "i32")?.try_into().expect("length checked"),
        ))
    }

    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_le_bytes(
            self.bytes(4, "u32")?.try_into().expect("length checked"),
        ))
    }

    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_le_bytes(
            self.bytes(8, "u64")?.try_into().expect("length checked"),
        ))
    }

    fn f32(&mut self) -> Result<f32, ProtocolError> {
        Ok(f32::from_le_bytes(
            self.bytes(4, "f32")?.try_into().expect("length checked"),
        ))
    }

    fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.offset..]
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
    use crate::live::protocol::{ChannelDescriptor, ChannelKind, PROTOCOL_VERSION_V1};

    fn channels() -> ChannelTable {
        ChannelTable {
            revision: 7,
            channels: vec![
                ChannelDescriptor {
                    channel_id: 0,
                    kind: ChannelKind::Analog,
                    wire_format: WireFormat::I16,
                    scale: 0.1,
                    offset: 0.0,
                    unit: "V".to_owned(),
                    name: "Cpu1Input".to_owned(),
                },
                ChannelDescriptor {
                    channel_id: 1,
                    kind: ChannelKind::Analog,
                    wire_format: WireFormat::I16,
                    scale: 0.1,
                    offset: 0.0,
                    unit: "V".to_owned(),
                    name: "ClaResult".to_owned(),
                },
                ChannelDescriptor {
                    channel_id: 2,
                    kind: ChannelKind::Analog,
                    wire_format: WireFormat::I16,
                    scale: 0.1,
                    offset: 0.0,
                    unit: "V".to_owned(),
                    name: "Cpu2Result".to_owned(),
                },
                ChannelDescriptor {
                    channel_id: 3,
                    kind: ChannelKind::Analog,
                    wire_format: WireFormat::I16,
                    scale: 0.1,
                    offset: 0.0,
                    unit: "V".to_owned(),
                    name: "Cpu1Applied".to_owned(),
                },
            ],
        }
    }

    fn streams() -> StreamTable {
        StreamTable {
            revision: 4,
            streams: vec![
                StreamDescriptor {
                    stream_id: 10,
                    domain: SampleDomain::Fast32k,
                    capture_phase: CapturePhase::AfterClaComplete,
                    sample_rate_hz: 32_000,
                    consistency_group: 1,
                    channel_ids: vec![1],
                },
                StreamDescriptor {
                    stream_id: 20,
                    domain: SampleDomain::Control8k,
                    capture_phase: CapturePhase::ControlCycleEnd,
                    sample_rate_hz: 8_000,
                    consistency_group: 1,
                    channel_ids: vec![0, 3],
                },
                StreamDescriptor {
                    stream_id: 30,
                    domain: SampleDomain::Slow1k,
                    capture_phase: CapturePhase::LogicTaskEnd,
                    sample_rate_hz: 1_000,
                    consistency_group: 1,
                    channel_ids: vec![2],
                },
            ],
            bindings: vec![
                StreamChannelBinding {
                    channel_id: 0,
                    stream_id: 20,
                    owner: SignalOwner::Cpu1,
                    role: SignalRole::ControlInput,
                },
                StreamChannelBinding {
                    channel_id: 1,
                    stream_id: 10,
                    owner: SignalOwner::Cpu1Cla1,
                    role: SignalRole::ControlOutput,
                },
                StreamChannelBinding {
                    channel_id: 2,
                    stream_id: 30,
                    owner: SignalOwner::Cpu2,
                    role: SignalRole::State,
                },
                StreamChannelBinding {
                    channel_id: 3,
                    stream_id: 20,
                    owner: SignalOwner::Cpu1,
                    role: SignalRole::AppliedCommand,
                },
            ],
            causal_relations: vec![CausalRelation {
                input_stream_id: 20,
                result_stream_id: 30,
                application_stream_id: 20,
                result_input_offset: 0,
                application_result_offset: 1,
            }],
        }
    }

    fn rows(first: u64, count: usize) -> Vec<StreamRowMetadata> {
        (0..count)
            .map(|offset| StreamRowMetadata {
                row_sequence: first + offset as u64,
                logical_cycle_sequence: first + offset as u64,
                source_sequence: first + offset as u64,
                applied_sequence: first.saturating_add(offset as u64).saturating_sub(1),
                valid_flags: SNAPSHOT_VALID
                    | VALID_FLAG_CLA_COMPLETE
                    | VALID_FLAG_ADC_VALID
                    | VALID_FLAG_DATA_FROZEN
                    | VALID_FLAG_SOURCE_VALID
                    | VALID_FLAG_APPLIED_VALID,
            })
            .collect()
    }

    fn stream_batch(
        stream_id: u16,
        channel_ids: Vec<u16>,
        sample_data: Vec<u8>,
        row_metadata: Vec<StreamRowMetadata>,
    ) -> StreamSampleBatch {
        let stream = streams().stream(stream_id).unwrap().clone();
        StreamSampleBatch {
            stream_id,
            stream_revision: 4,
            domain: stream.domain,
            capture_phase: stream.capture_phase,
            consistency_group: stream.consistency_group,
            first_row_sequence: row_metadata
                .first()
                .map(|row| row.row_sequence)
                .unwrap_or(0),
            sample_period_ticks: match stream.domain {
                SampleDomain::Fast32k => 31,
                SampleDomain::Control8k => 125,
                SampleDomain::Slow1k => 1_000,
            },
            row_count: u16::try_from(row_metadata.len()).unwrap(),
            channel_ids,
            sample_data,
            row_metadata,
        }
    }

    #[test]
    fn stream_table_round_trips_with_cpu_and_causal_semantics() {
        let table = streams();
        let message = MessageV2::StreamTable(table.clone());
        let payload = message.encode_payload().unwrap();

        assert_eq!(
            MessageV2::decode(MSG_STREAM_TABLE, &payload).unwrap(),
            message
        );
        assert_eq!(table.bindings[2].owner, SignalOwner::Cpu2);
        assert_eq!(table.causal_relations[0].application_result_offset, 1);
    }

    #[test]
    fn v2_manual_trigger_golden_frame_is_little_endian_and_version_isolated() {
        let frame = MessageV2::ManualTrigger(ManualTrigger {
            capture_id: 0xa1b2_c3d4,
        })
        .into_frame(0, 0x1122_3344, 0x5566_7788, 0x0102_0304_0506_0708)
        .unwrap();
        assert_eq!(
            frame.encode().unwrap(),
            vec![
                b'S', b'C', b'P', b'1', 2, 0x41, 0, 0, 0x44, 0x33, 0x22, 0x11, 4, 0, 0, 0, 0x88,
                0x77, 0x66, 0x55, 8, 7, 6, 5, 4, 3, 2, 1, 0xd4, 0xc3, 0xb2, 0xa1, 0x30, 0xc8, 0xde,
                0xcb,
            ]
        );
    }

    #[test]
    fn stream_descriptor_rejects_non_fixed_rate_or_phase() {
        let mut invalid_table = streams();
        invalid_table.streams[0].sample_rate_hz = 8_000;
        assert!(invalid_table.validate().is_err());

        let mut invalid_table = streams();
        invalid_table.streams[1].capture_phase = CapturePhase::LogicTaskEnd;
        assert!(invalid_table.validate().is_err());
    }

    #[test]
    fn stream_table_requires_exact_descriptor_and_binding_sets() {
        let mut missing = streams();
        missing.bindings.retain(|binding| binding.channel_id != 3);
        assert!(missing.validate().is_err());

        let mut extra = streams();
        extra.bindings[0].stream_id = 10;
        assert!(extra.validate().is_err());

        let mut repeated_channel = streams();
        repeated_channel.streams[1].channel_ids.push(1);
        assert!(repeated_channel.validate().is_err());

        let mut contradictory = streams();
        contradictory.causal_relations.push(CausalRelation {
            input_stream_id: 10,
            result_stream_id: 30,
            application_stream_id: 20,
            result_input_offset: 1,
            application_result_offset: 1,
        });
        assert!(contradictory.validate().is_err());
    }

    #[test]
    fn fixed_stream_period_requires_exact_integral_tick_division() {
        let table = streams();
        assert!(
            validate_stream_sample_period(32_000_000, table.stream(10).unwrap(), 1_000).is_ok()
        );
        assert!(
            validate_stream_sample_period(32_000_000, table.stream(20).unwrap(), 4_000).is_ok()
        );
        assert!(
            validate_stream_sample_period(32_000_000, table.stream(30).unwrap(), 32_000).is_ok()
        );
        assert!(validate_stream_sample_period(32_000_000, table.stream(10).unwrap(), 0).is_err());
        assert!(validate_stream_sample_period(32_000_000, table.stream(10).unwrap(), 999).is_err());
        assert!(validate_stream_sample_period(1_000_000, table.stream(10).unwrap(), 31).is_err());
    }

    #[test]
    fn configure_rejects_channels_from_another_stream() {
        let error = validate_configure_stream_for_device(
            &ConfigureStream {
                stream_id: 20,
                batch_samples: 16,
                channel_mask: (1 << 0) | (1 << 2),
            },
            &streams(),
            &channels(),
            128,
            1024,
        )
        .unwrap_err();

        assert!(error.to_string().contains("belongs to stream"));
    }

    #[test]
    fn stream_batch_rejects_channels_from_another_stream() {
        let message = MessageV2::StreamSampleBatch(stream_batch(
            20,
            vec![0, 2],
            [10_i16.to_le_bytes(), 20_i16.to_le_bytes()].concat(),
            rows(100, 1),
        ));
        let frame = message.into_frame(0, 3, 2, 1_000).unwrap();

        assert!(matches!(
            decode_stream_sample_frame(&frame, &channels(), &streams()),
            Err(ProtocolError::InvalidPayload(_))
        ));
    }

    #[test]
    fn stream_batch_round_trips_in_a_v2_frame_with_causality_index() {
        let mut metadata = rows(100, 2);
        for (offset, row) in metadata.iter_mut().enumerate() {
            row.logical_cycle_sequence = 500 + offset as u64;
            row.source_sequence = row.logical_cycle_sequence;
            row.applied_sequence = row.logical_cycle_sequence.saturating_sub(1);
        }
        let message = MessageV2::StreamSampleBatch(stream_batch(
            20,
            vec![0, 3],
            [
                10_i16.to_le_bytes(),
                20_i16.to_le_bytes(),
                30_i16.to_le_bytes(),
                40_i16.to_le_bytes(),
            ]
            .concat(),
            metadata,
        ));
        let frame = message.into_frame(0, 3, 2, 1_000).unwrap();
        assert_eq!(frame.payload.len(), 110);
        let encoded = frame.encode().unwrap();
        let decoded_frame = Frame::decode(&encoded).unwrap();

        assert_eq!(decoded_frame.version, PROTOCOL_VERSION_V2);
        let batch = decode_stream_sample_frame(&decoded_frame, &channels(), &streams()).unwrap();
        assert_eq!(batch.stream_id, 20);
        assert_eq!(batch.row_metadata[0].row_sequence, 100);
        assert_eq!(batch.row_metadata[0].logical_cycle_sequence, 500);
        assert_eq!(
            batch.row_metadata[1].applied_alignment(),
            CausalAlignment::PreviousShot
        );
        assert_eq!(
            batch.row_metadata[1].applied_alignment().chinese_label(),
            "上一拍"
        );
        assert_eq!(batch.channels, vec![vec![1.0, 3.0], vec![2.0, 4.0]]);
        assert_eq!(batch.raw_frame, encoded);

        let v1 = Frame::new(MSG_STREAM_SAMPLE_BATCH, 0, 1, 1, 0, vec![]);
        assert_eq!(v1.version, PROTOCOL_VERSION_V1);
        assert!(matches!(
            decode_stream_sample_frame(&v1, &channels(), &streams()),
            Err(ProtocolError::UnsupportedVersion(PROTOCOL_VERSION_V1))
        ));
    }

    #[test]
    fn stream_batch_requires_one_metadata_row_per_sample() {
        let mut batch = stream_batch(
            20,
            vec![0],
            [10_i16.to_le_bytes(), 20_i16.to_le_bytes()].concat(),
            rows(100, 1),
        );
        batch.row_count = 2;

        assert!(MessageV2::StreamSampleBatch(batch)
            .encode_payload()
            .is_err());
    }

    #[test]
    fn stream_batch_rejects_rows_that_are_not_frozen_or_cla_complete() {
        let mut metadata = rows(100, 1);
        metadata[0].valid_flags &= !VALID_FLAG_DATA_FROZEN;
        let batch = stream_batch(20, vec![0], 10_i16.to_le_bytes().to_vec(), metadata);
        assert!(MessageV2::StreamSampleBatch(batch).encode_payload().is_ok());

        let mut metadata = rows(100, 1);
        metadata[0].valid_flags &= !VALID_FLAG_CLA_COMPLETE;
        let batch = stream_batch(10, vec![1], 10_i16.to_le_bytes().to_vec(), metadata);
        let frame = MessageV2::StreamSampleBatch(batch)
            .into_frame(0, 3, 2, 1_000)
            .unwrap();
        assert!(decode_stream_sample_frame(&frame, &channels(), &streams()).is_ok());
    }

    #[test]
    fn dsp_capture_messages_round_trip_with_frozen_data() {
        let arm = MessageV2::ArmCapture(ArmCapture {
            capture_id: 9,
            stream_id: 10,
            pretrigger_rows: 1_024,
            posttrigger_rows: 2_048,
            timeout_samples: 4_096,
            trigger: CaptureTrigger {
                kind: CaptureTriggerKind::FaultFlag,
                channel_id: 0,
                level: 0.0,
                edge: CaptureEdge::Rising,
                hysteresis: 0.0,
                flag_mask: 0x20,
                flag_value: 0x20,
            },
        });
        let payload = arm.encode_payload().unwrap();
        assert_eq!(MessageV2::decode(MSG_ARM_CAPTURE, &payload).unwrap(), arm);

        let data = MessageV2::CaptureData(CaptureData {
            capture_id: 9,
            block_index: 3,
            batch: stream_batch(10, vec![1], 10_i16.to_le_bytes().to_vec(), rows(88, 1)),
        });
        let payload = data.encode_payload().unwrap();
        assert_eq!(MessageV2::decode(MSG_CAPTURE_DATA, &payload).unwrap(), data);
    }
}
