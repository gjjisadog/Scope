use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use thiserror::Error;

pub const FRAME_MAGIC: [u8; 4] = *b"SCP1";
pub const PROTOCOL_VERSION: u8 = 1;
pub const FRAME_HEADER_LEN: usize = 28;
pub const FRAME_CRC_LEN: usize = 4;
pub const MAX_PAYLOAD_LEN: usize = 1024 * 1024;

pub const MSG_HELLO: u8 = 0x01;
pub const MSG_HELLO_ACK: u8 = 0x02;
pub const MSG_CHANNEL_TABLE: u8 = 0x03;
pub const MSG_CONFIGURE: u8 = 0x10;
pub const MSG_START: u8 = 0x11;
pub const MSG_STOP: u8 = 0x12;
pub const MSG_COMMAND_RESULT: u8 = 0x13;
pub const MSG_PING: u8 = 0x14;
pub const MSG_PONG: u8 = 0x15;
pub const MSG_SAMPLE_BATCH: u8 = 0x20;
pub const MSG_STATUS: u8 = 0x21;
pub const MSG_ERROR: u8 = 0x22;

pub const MAX_CHANNEL_COUNT: usize = 64;
pub const MAX_BATCH_SAMPLES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub client_capabilities: u32,
    pub max_payload: u32,
    pub client_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelloAck {
    pub device_capabilities: u32,
    pub max_payload: u32,
    pub tick_hz: u64,
    pub channel_count: u16,
    pub max_batch_samples: u16,
    pub device_id: [u8; 16],
    pub firmware_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum ChannelKind {
    Analog = 0,
    Digital = 1,
}

impl TryFrom<u8> for ChannelKind {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Analog),
            1 => Ok(Self::Digital),
            _ => Err(ProtocolError::InvalidPayload(format!(
                "unknown channel kind {value}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum WireFormat {
    I16 = 1,
    I32 = 2,
    F32 = 3,
    U8 = 4,
}

impl WireFormat {
    pub fn byte_width(self) -> usize {
        match self {
            Self::I16 => 2,
            Self::I32 | Self::F32 => 4,
            Self::U8 => 1,
        }
    }
}

impl TryFrom<u8> for WireFormat {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::I16),
            2 => Ok(Self::I32),
            3 => Ok(Self::F32),
            4 => Ok(Self::U8),
            _ => Err(ProtocolError::InvalidPayload(format!(
                "unknown wire format {value}"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelDescriptor {
    pub channel_id: u16,
    pub kind: ChannelKind,
    pub wire_format: WireFormat,
    pub scale: f32,
    pub offset: f32,
    pub unit: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelTable {
    pub revision: u32,
    pub channels: Vec<ChannelDescriptor>,
}

impl ChannelTable {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.channels.is_empty() || self.channels.len() > MAX_CHANNEL_COUNT {
            return Err(ProtocolError::InvalidPayload(format!(
                "channel table count {} is outside 1..={MAX_CHANNEL_COUNT}",
                self.channels.len()
            )));
        }
        let mut seen = 0_u64;
        for descriptor in &self.channels {
            if usize::from(descriptor.channel_id) >= MAX_CHANNEL_COUNT {
                return Err(ProtocolError::InvalidPayload(format!(
                    "channel id {} is outside 0..{}",
                    descriptor.channel_id,
                    MAX_CHANNEL_COUNT - 1
                )));
            }
            let bit = 1_u64 << descriptor.channel_id;
            if seen & bit != 0 {
                return Err(ProtocolError::InvalidPayload(format!(
                    "duplicate channel id {}",
                    descriptor.channel_id
                )));
            }
            seen |= bit;
            if descriptor.name.is_empty() {
                return Err(ProtocolError::InvalidPayload(format!(
                    "channel {} has an empty name",
                    descriptor.channel_id
                )));
            }
            if !descriptor.scale.is_finite() || !descriptor.offset.is_finite() {
                return Err(ProtocolError::InvalidPayload(format!(
                    "channel {} has non-finite scaling",
                    descriptor.channel_id
                )));
            }
            if descriptor.wire_format == WireFormat::F32
                && (descriptor.scale != 1.0 || descriptor.offset != 0.0)
            {
                return Err(ProtocolError::InvalidPayload(format!(
                    "f32 channel {} must use scale 1 and offset 0",
                    descriptor.channel_id
                )));
            }
            ensure_u8_string(&descriptor.unit, "channel unit")?;
            ensure_u8_string(&descriptor.name, "channel name")?;
        }
        Ok(())
    }

    pub fn channel(&self, channel_id: u16) -> Option<&ChannelDescriptor> {
        self.channels
            .iter()
            .find(|descriptor| descriptor.channel_id == channel_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Configure {
    pub sample_rate_hz: u32,
    pub batch_samples: u16,
    pub channel_mask: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum ResultCode {
    Ok = 0,
    Unsupported = 1,
    InvalidState = 2,
    InvalidArgument = 3,
    Busy = 4,
    InternalError = 5,
}

impl TryFrom<u16> for ResultCode {
    type Error = ProtocolError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::Unsupported),
            2 => Ok(Self::InvalidState),
            3 => Ok(Self::InvalidArgument),
            4 => Ok(Self::Busy),
            5 => Ok(Self::InternalError),
            _ => Err(ProtocolError::InvalidPayload(format!(
                "unknown command result code {value}"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    pub request_sequence: u32,
    pub result_code: ResultCode,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DeviceState {
    Idle = 0,
    Configured = 1,
    Streaming = 2,
}

impl TryFrom<u8> for DeviceState {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Idle),
            1 => Ok(Self::Configured),
            2 => Ok(Self::Streaming),
            _ => Err(ProtocolError::InvalidPayload(format!(
                "unknown device state {value}"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: DeviceState,
    pub produced_samples: u64,
    pub dropped_samples: u64,
    pub tx_overruns: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleBatch {
    pub channel_table_revision: u32,
    pub first_sample_index: u64,
    pub sample_period_ticks: u32,
    pub sample_count: u16,
    pub channel_ids: Vec<u16>,
    pub sample_data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedSampleBatch {
    pub revision: u32,
    pub first_sample_index: u64,
    pub sample_period_ticks: u32,
    pub timestamp_ticks: u64,
    pub channel_ids: Vec<u16>,
    pub channels: Vec<Vec<f32>>,
    pub raw_frame: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Hello(Hello),
    HelloAck(HelloAck),
    ChannelTable(ChannelTable),
    Configure(Configure),
    Start,
    Stop,
    CommandResult(CommandResult),
    Ping(u64),
    Pong(u64),
    SampleBatch(SampleBatch),
    Status(Status),
    Error(CommandResult),
}

impl Message {
    pub fn message_type(&self) -> u8 {
        match self {
            Self::Hello(_) => MSG_HELLO,
            Self::HelloAck(_) => MSG_HELLO_ACK,
            Self::ChannelTable(_) => MSG_CHANNEL_TABLE,
            Self::Configure(_) => MSG_CONFIGURE,
            Self::Start => MSG_START,
            Self::Stop => MSG_STOP,
            Self::CommandResult(_) => MSG_COMMAND_RESULT,
            Self::Ping(_) => MSG_PING,
            Self::Pong(_) => MSG_PONG,
            Self::SampleBatch(_) => MSG_SAMPLE_BATCH,
            Self::Status(_) => MSG_STATUS,
            Self::Error(_) => MSG_ERROR,
        }
    }

    pub fn encode_payload(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut bytes = Vec::new();
        match self {
            Self::Hello(message) => {
                validate_max_payload(message.max_payload)?;
                put_u32(&mut bytes, message.client_capabilities);
                put_u32(&mut bytes, message.max_payload);
                put_string_u16(&mut bytes, &message.client_name, "client name")?;
            }
            Self::HelloAck(message) => {
                validate_max_payload(message.max_payload)?;
                if message.tick_hz == 0 {
                    return invalid("device tick_hz must be greater than zero");
                }
                validate_count(
                    usize::from(message.channel_count),
                    1,
                    MAX_CHANNEL_COUNT,
                    "device channel count",
                )?;
                validate_count(
                    usize::from(message.max_batch_samples),
                    1,
                    MAX_BATCH_SAMPLES,
                    "maximum batch samples",
                )?;
                put_u32(&mut bytes, message.device_capabilities);
                put_u32(&mut bytes, message.max_payload);
                put_u64(&mut bytes, message.tick_hz);
                put_u16(&mut bytes, message.channel_count);
                put_u16(&mut bytes, message.max_batch_samples);
                bytes.extend_from_slice(&message.device_id);
                put_string_u16(&mut bytes, &message.firmware_name, "firmware name")?;
            }
            Self::ChannelTable(table) => {
                table.validate()?;
                put_u32(&mut bytes, table.revision);
                put_u16(
                    &mut bytes,
                    u16::try_from(table.channels.len())
                        .map_err(|_| invalid_error("too many channel descriptors"))?,
                );
                for descriptor in &table.channels {
                    put_u16(&mut bytes, descriptor.channel_id);
                    bytes.push(descriptor.kind as u8);
                    bytes.push(descriptor.wire_format as u8);
                    put_f32(&mut bytes, descriptor.scale);
                    put_f32(&mut bytes, descriptor.offset);
                    ensure_u8_string(&descriptor.unit, "channel unit")?;
                    ensure_u8_string(&descriptor.name, "channel name")?;
                    bytes.push(descriptor.unit.len() as u8);
                    bytes.push(descriptor.name.len() as u8);
                    bytes.extend_from_slice(descriptor.unit.as_bytes());
                    bytes.extend_from_slice(descriptor.name.as_bytes());
                }
            }
            Self::Configure(message) => {
                if message.sample_rate_hz == 0 {
                    return invalid("sample rate must be greater than zero");
                }
                validate_count(
                    usize::from(message.batch_samples),
                    1,
                    MAX_BATCH_SAMPLES,
                    "batch samples",
                )?;
                if message.channel_mask == 0 {
                    return invalid("channel mask must select at least one channel");
                }
                put_u32(&mut bytes, message.sample_rate_hz);
                put_u16(&mut bytes, message.batch_samples);
                put_u16(&mut bytes, 0);
                put_u64(&mut bytes, message.channel_mask);
            }
            Self::Start | Self::Stop => {}
            Self::CommandResult(result) | Self::Error(result) => {
                put_u32(&mut bytes, result.request_sequence);
                put_u16(&mut bytes, result.result_code as u16);
                put_string_u16(&mut bytes, &result.detail, "command detail")?;
            }
            Self::Ping(nonce) | Self::Pong(nonce) => put_u64(&mut bytes, *nonce),
            Self::SampleBatch(batch) => {
                validate_sample_batch_header(batch)?;
                put_u32(&mut bytes, batch.channel_table_revision);
                put_u64(&mut bytes, batch.first_sample_index);
                put_u32(&mut bytes, batch.sample_period_ticks);
                put_u16(&mut bytes, batch.sample_count);
                put_u16(
                    &mut bytes,
                    u16::try_from(batch.channel_ids.len())
                        .map_err(|_| invalid_error("too many selected channels"))?,
                );
                for &channel_id in &batch.channel_ids {
                    put_u16(&mut bytes, channel_id);
                }
                bytes.extend_from_slice(&batch.sample_data);
            }
            Self::Status(status) => {
                bytes.push(status.state as u8);
                bytes.extend_from_slice(&[0; 3]);
                put_u64(&mut bytes, status.produced_samples);
                put_u64(&mut bytes, status.dropped_samples);
                put_u32(&mut bytes, status.tx_overruns);
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
            MSG_HELLO => {
                let message = Hello {
                    client_capabilities: reader.u32()?,
                    max_payload: reader.u32()?,
                    client_name: reader.string_u16("client name")?,
                };
                validate_max_payload(message.max_payload)?;
                Self::Hello(message)
            }
            MSG_HELLO_ACK => {
                let device_capabilities = reader.u32()?;
                let max_payload = reader.u32()?;
                let tick_hz = reader.u64()?;
                let channel_count = reader.u16()?;
                let max_batch_samples = reader.u16()?;
                let mut device_id = [0_u8; 16];
                device_id.copy_from_slice(reader.bytes(16, "device id")?);
                let firmware_name = reader.string_u16("firmware name")?;
                validate_max_payload(max_payload)?;
                if tick_hz == 0 {
                    return invalid("device tick_hz must be greater than zero");
                }
                validate_count(
                    usize::from(channel_count),
                    1,
                    MAX_CHANNEL_COUNT,
                    "device channel count",
                )?;
                validate_count(
                    usize::from(max_batch_samples),
                    1,
                    MAX_BATCH_SAMPLES,
                    "maximum batch samples",
                )?;
                Self::HelloAck(HelloAck {
                    device_capabilities,
                    max_payload,
                    tick_hz,
                    channel_count,
                    max_batch_samples,
                    device_id,
                    firmware_name,
                })
            }
            MSG_CHANNEL_TABLE => {
                let revision = reader.u32()?;
                let descriptor_count = usize::from(reader.u16()?);
                validate_count(
                    descriptor_count,
                    1,
                    MAX_CHANNEL_COUNT,
                    "channel descriptor count",
                )?;
                let mut channels = Vec::with_capacity(descriptor_count);
                for _ in 0..descriptor_count {
                    let channel_id = reader.u16()?;
                    let kind = ChannelKind::try_from(reader.u8()?)?;
                    let wire_format = WireFormat::try_from(reader.u8()?)?;
                    let scale = reader.f32()?;
                    let offset = reader.f32()?;
                    let unit_len = usize::from(reader.u8()?);
                    let name_len = usize::from(reader.u8()?);
                    channels.push(ChannelDescriptor {
                        channel_id,
                        kind,
                        wire_format,
                        scale,
                        offset,
                        unit: reader.string(unit_len, "channel unit")?,
                        name: reader.string(name_len, "channel name")?,
                    });
                }
                let table = ChannelTable { revision, channels };
                table.validate()?;
                Self::ChannelTable(table)
            }
            MSG_CONFIGURE => {
                let sample_rate_hz = reader.u32()?;
                let batch_samples = reader.u16()?;
                let reserved = reader.u16()?;
                let channel_mask = reader.u64()?;
                if reserved != 0 {
                    return invalid("CONFIGURE reserved field must be zero");
                }
                let message = Configure {
                    sample_rate_hz,
                    batch_samples,
                    channel_mask,
                };
                Self::Configure(validate_configure(message)?)
            }
            MSG_START => Self::Start,
            MSG_STOP => Self::Stop,
            MSG_COMMAND_RESULT => Self::CommandResult(decode_command_result(&mut reader)?),
            MSG_PING => Self::Ping(reader.u64()?),
            MSG_PONG => Self::Pong(reader.u64()?),
            MSG_SAMPLE_BATCH => {
                let channel_table_revision = reader.u32()?;
                let first_sample_index = reader.u64()?;
                let sample_period_ticks = reader.u32()?;
                let sample_count = reader.u16()?;
                let selected_channel_count = usize::from(reader.u16()?);
                validate_count(
                    selected_channel_count,
                    1,
                    MAX_CHANNEL_COUNT,
                    "selected channel count",
                )?;
                let mut channel_ids = Vec::with_capacity(selected_channel_count);
                for _ in 0..selected_channel_count {
                    channel_ids.push(reader.u16()?);
                }
                let sample_data = reader.remaining().to_vec();
                reader.consume_remaining();
                let batch = SampleBatch {
                    channel_table_revision,
                    first_sample_index,
                    sample_period_ticks,
                    sample_count,
                    channel_ids,
                    sample_data,
                };
                validate_sample_batch_header(&batch)?;
                Self::SampleBatch(batch)
            }
            MSG_STATUS => {
                let state = DeviceState::try_from(reader.u8()?)?;
                if reader.bytes(3, "STATUS reserved")? != [0, 0, 0] {
                    return invalid("STATUS reserved bytes must be zero");
                }
                Self::Status(Status {
                    state,
                    produced_samples: reader.u64()?,
                    dropped_samples: reader.u64()?,
                    tx_overruns: reader.u32()?,
                })
            }
            MSG_ERROR => Self::Error(decode_command_result(&mut reader)?),
            other => return Err(ProtocolError::UnknownMessageType(other)),
        };
        reader.finish()?;
        Ok(message)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub message_type: u8,
    pub flags: u16,
    pub sequence: u32,
    pub session_id: u32,
    pub timestamp_ticks: u64,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(
        message_type: u8,
        flags: u16,
        sequence: u32,
        session_id: u32,
        timestamp_ticks: u64,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            message_type,
            flags,
            sequence,
            session_id,
            timestamp_ticks,
            payload,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        if self.payload.len() > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge(self.payload.len()));
        }
        let payload_len = u32::try_from(self.payload.len())
            .map_err(|_| ProtocolError::PayloadTooLarge(self.payload.len()))?;
        let total_len = FRAME_HEADER_LEN
            .checked_add(self.payload.len())
            .and_then(|len| len.checked_add(FRAME_CRC_LEN))
            .ok_or(ProtocolError::LengthOverflow)?;
        let mut bytes = Vec::with_capacity(total_len);
        bytes.extend_from_slice(&FRAME_MAGIC);
        bytes.push(PROTOCOL_VERSION);
        bytes.push(self.message_type);
        bytes.extend_from_slice(&self.flags.to_le_bytes());
        bytes.extend_from_slice(&self.sequence.to_le_bytes());
        bytes.extend_from_slice(&payload_len.to_le_bytes());
        bytes.extend_from_slice(&self.session_id.to_le_bytes());
        bytes.extend_from_slice(&self.timestamp_ticks.to_le_bytes());
        bytes.extend_from_slice(&self.payload);
        let checksum = crc32c(&bytes[4..]);
        bytes.extend_from_slice(&checksum.to_le_bytes());
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < FRAME_HEADER_LEN + FRAME_CRC_LEN {
            return Err(ProtocolError::Truncated {
                minimum: FRAME_HEADER_LEN + FRAME_CRC_LEN,
                actual: bytes.len(),
            });
        }
        if bytes[..4] != FRAME_MAGIC {
            return Err(ProtocolError::InvalidMagic);
        }
        if bytes[4] != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(bytes[4]));
        }
        let payload_len = read_u32(bytes, 12)? as usize;
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge(payload_len));
        }
        let expected_len = FRAME_HEADER_LEN
            .checked_add(payload_len)
            .and_then(|len| len.checked_add(FRAME_CRC_LEN))
            .ok_or(ProtocolError::LengthOverflow)?;
        if bytes.len() != expected_len {
            return Err(ProtocolError::LengthMismatch {
                expected: expected_len,
                actual: bytes.len(),
            });
        }
        let payload_end = FRAME_HEADER_LEN + payload_len;
        let expected_crc = read_u32(bytes, payload_end)?;
        let actual_crc = crc32c(&bytes[4..payload_end]);
        if expected_crc != actual_crc {
            return Err(ProtocolError::CrcMismatch {
                expected: expected_crc,
                actual: actual_crc,
            });
        }
        Ok(Self {
            message_type: bytes[5],
            flags: read_u16(bytes, 6)?,
            sequence: read_u32(bytes, 8)?,
            session_id: read_u32(bytes, 16)?,
            timestamp_ticks: read_u64(bytes, 20)?,
            payload: bytes[FRAME_HEADER_LEN..payload_end].to_vec(),
        })
    }
}

pub fn decode_sample_frame(
    frame: &Frame,
    table: &ChannelTable,
) -> Result<DecodedSampleBatch, ProtocolError> {
    if frame.message_type != MSG_SAMPLE_BATCH {
        return Err(ProtocolError::UnexpectedMessageType {
            expected: MSG_SAMPLE_BATCH,
            actual: frame.message_type,
        });
    }
    table.validate()?;
    let Message::SampleBatch(batch) = Message::decode(frame.message_type, &frame.payload)? else {
        return Err(ProtocolError::UnexpectedMessageType {
            expected: MSG_SAMPLE_BATCH,
            actual: frame.message_type,
        });
    };
    if batch.channel_table_revision != table.revision {
        return invalid(format!(
            "sample batch references channel table revision {}, current revision is {}",
            batch.channel_table_revision, table.revision
        ));
    }
    let descriptors = batch
        .channel_ids
        .iter()
        .map(|channel_id| {
            table.channel(*channel_id).ok_or_else(|| {
                invalid_error(format!(
                    "sample batch references unknown channel {channel_id}"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let bytes_per_sample = descriptors.iter().try_fold(0_usize, |total, descriptor| {
        total
            .checked_add(descriptor.wire_format.byte_width())
            .ok_or(ProtocolError::LengthOverflow)
    })?;
    let expected_data_len = bytes_per_sample
        .checked_mul(usize::from(batch.sample_count))
        .ok_or(ProtocolError::LengthOverflow)?;
    if batch.sample_data.len() != expected_data_len {
        return invalid(format!(
            "sample data length mismatch: expected {expected_data_len}, got {}",
            batch.sample_data.len()
        ));
    }
    let last_sample_offset = u64::from(batch.sample_count - 1);
    let tick_offset = u64::from(batch.sample_period_ticks)
        .checked_mul(last_sample_offset)
        .ok_or_else(|| invalid_error("sample timestamp offset overflow"))?;
    frame
        .timestamp_ticks
        .checked_add(tick_offset)
        .ok_or_else(|| invalid_error("sample timestamp overflow"))?;
    let mut reader = PayloadReader::new(&batch.sample_data);
    let mut channels = descriptors
        .iter()
        .map(|_| Vec::with_capacity(usize::from(batch.sample_count)))
        .collect::<Vec<_>>();
    for _ in 0..batch.sample_count {
        for (channel_index, descriptor) in descriptors.iter().enumerate() {
            let value = match descriptor.wire_format {
                WireFormat::I16 => {
                    let raw = reader.i16()? as f32;
                    raw * descriptor.scale + descriptor.offset
                }
                WireFormat::I32 => {
                    let raw = reader.i32()? as f32;
                    raw * descriptor.scale + descriptor.offset
                }
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
            channels[channel_index].push(value);
        }
    }
    reader.finish()?;
    Ok(DecodedSampleBatch {
        revision: batch.channel_table_revision,
        first_sample_index: batch.first_sample_index,
        sample_period_ticks: batch.sample_period_ticks,
        timestamp_ticks: frame.timestamp_ticks,
        channel_ids: batch.channel_ids,
        channels,
        raw_frame: frame.encode()?,
    })
}

fn validate_max_payload(max_payload: u32) -> Result<(), ProtocolError> {
    if max_payload == 0 || max_payload as usize > MAX_PAYLOAD_LEN {
        return invalid(format!(
            "maximum payload {max_payload} is outside 1..={MAX_PAYLOAD_LEN}"
        ));
    }
    Ok(())
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

fn validate_configure(message: Configure) -> Result<Configure, ProtocolError> {
    if message.sample_rate_hz == 0 {
        return invalid("sample rate must be greater than zero");
    }
    validate_count(
        usize::from(message.batch_samples),
        1,
        MAX_BATCH_SAMPLES,
        "batch samples",
    )?;
    if message.channel_mask == 0 {
        return invalid("channel mask must select at least one channel");
    }
    Ok(message)
}

pub fn validate_configure_for_device(
    configure: &Configure,
    hello: &HelloAck,
    table: &ChannelTable,
) -> Result<(), ProtocolError> {
    validate_configure(configure.clone())?;
    table.validate()?;
    if usize::from(hello.channel_count) != table.channels.len() {
        return invalid(format!(
            "HELLO_ACK channel count {} does not match channel table count {}",
            hello.channel_count,
            table.channels.len()
        ));
    }
    if configure.sample_rate_hz as u64 > hello.tick_hz {
        return invalid(format!(
            "sample rate {} exceeds device tick rate {}",
            configure.sample_rate_hz, hello.tick_hz
        ));
    }
    if configure.batch_samples > hello.max_batch_samples {
        return invalid(format!(
            "batch samples {} exceeds device maximum {}",
            configure.batch_samples, hello.max_batch_samples
        ));
    }
    let known_mask = table
        .channels
        .iter()
        .fold(0_u64, |mask, channel| mask | (1_u64 << channel.channel_id));
    if configure.channel_mask & !known_mask != 0 {
        return invalid(format!(
            "channel mask 0x{:016x} selects unknown device channels",
            configure.channel_mask
        ));
    }
    let selected = table
        .channels
        .iter()
        .filter(|channel| configure.channel_mask & (1_u64 << channel.channel_id) != 0)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return invalid("channel mask selects no device channels");
    }
    let bytes_per_sample = selected.iter().try_fold(0_usize, |total, channel| {
        total
            .checked_add(channel.wire_format.byte_width())
            .ok_or(ProtocolError::LengthOverflow)
    })?;
    let payload_len = 20_usize
        .checked_add(
            selected
                .len()
                .checked_mul(2)
                .ok_or(ProtocolError::LengthOverflow)?,
        )
        .and_then(|length| {
            bytes_per_sample
                .checked_mul(usize::from(configure.batch_samples))
                .and_then(|sample_bytes| length.checked_add(sample_bytes))
        })
        .ok_or(ProtocolError::LengthOverflow)?;
    let negotiated_max = usize::try_from(hello.max_payload)
        .unwrap_or(usize::MAX)
        .min(MAX_PAYLOAD_LEN);
    if payload_len > negotiated_max {
        return invalid(format!(
            "configured sample payload {payload_len} exceeds negotiated maximum {negotiated_max}"
        ));
    }
    Ok(())
}

pub fn encode_configure_result_detail(configure: &Configure) -> String {
    format!(
        "sample_rate_hz={};batch_samples={};channel_mask=0x{:016x}",
        configure.sample_rate_hz, configure.batch_samples, configure.channel_mask
    )
}

pub fn decode_configure_result_detail(detail: &str) -> Result<Configure, ProtocolError> {
    let mut sample_rate_hz = None;
    let mut batch_samples = None;
    let mut channel_mask = None;
    for field in detail.split(';') {
        let (name, value) = field
            .split_once('=')
            .ok_or_else(|| invalid_error("invalid CONFIGURE result detail"))?;
        match name {
            "sample_rate_hz" if sample_rate_hz.is_none() => {
                sample_rate_hz = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| invalid_error("invalid configured sample rate"))?,
                );
            }
            "batch_samples" if batch_samples.is_none() => {
                batch_samples = Some(
                    value
                        .parse::<u16>()
                        .map_err(|_| invalid_error("invalid configured batch samples"))?,
                );
            }
            "channel_mask" if channel_mask.is_none() => {
                let value = value
                    .strip_prefix("0x")
                    .ok_or_else(|| invalid_error("configured channel mask must use 0x prefix"))?;
                channel_mask = Some(
                    u64::from_str_radix(value, 16)
                        .map_err(|_| invalid_error("invalid configured channel mask"))?,
                );
            }
            _ => {
                return invalid(format!(
                    "unknown or duplicate CONFIGURE result field {name}"
                ))
            }
        }
    }
    validate_configure(Configure {
        sample_rate_hz: sample_rate_hz
            .ok_or_else(|| invalid_error("CONFIGURE result is missing sample_rate_hz"))?,
        batch_samples: batch_samples
            .ok_or_else(|| invalid_error("CONFIGURE result is missing batch_samples"))?,
        channel_mask: channel_mask
            .ok_or_else(|| invalid_error("CONFIGURE result is missing channel_mask"))?,
    })
}

fn validate_sample_batch_header(batch: &SampleBatch) -> Result<(), ProtocolError> {
    if batch.sample_period_ticks == 0 {
        return invalid("sample period ticks must be greater than zero");
    }
    validate_count(
        usize::from(batch.sample_count),
        1,
        MAX_BATCH_SAMPLES,
        "sample count",
    )?;
    batch
        .first_sample_index
        .checked_add(u64::from(batch.sample_count - 1))
        .ok_or_else(|| invalid_error("sample index overflow"))?;
    validate_count(
        batch.channel_ids.len(),
        1,
        MAX_CHANNEL_COUNT,
        "selected channel count",
    )?;
    let mut seen = 0_u64;
    for &channel_id in &batch.channel_ids {
        if usize::from(channel_id) >= MAX_CHANNEL_COUNT {
            return invalid(format!("selected channel id {channel_id} is out of range"));
        }
        let bit = 1_u64 << channel_id;
        if seen & bit != 0 {
            return invalid(format!("duplicate selected channel id {channel_id}"));
        }
        seen |= bit;
    }
    Ok(())
}

fn decode_command_result(reader: &mut PayloadReader<'_>) -> Result<CommandResult, ProtocolError> {
    Ok(CommandResult {
        request_sequence: reader.u32()?,
        result_code: ResultCode::try_from(reader.u16()?)?,
        detail: reader.string_u16("command detail")?,
    })
}

fn ensure_u8_string(value: &str, label: &str) -> Result<(), ProtocolError> {
    if value.len() > u8::MAX as usize {
        return invalid(format!("{label} exceeds {} UTF-8 bytes", u8::MAX));
    }
    Ok(())
}

fn ensure_u16_string(value: &str, label: &str) -> Result<(), ProtocolError> {
    if value.len() > u16::MAX as usize {
        return invalid(format!("{label} exceeds {} UTF-8 bytes", u16::MAX));
    }
    Ok(())
}

fn put_u16(bytes: &mut Vec<u8>, value: u16) {
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

fn put_string_u16(bytes: &mut Vec<u8>, value: &str, label: &str) -> Result<(), ProtocolError> {
    ensure_u16_string(value, label)?;
    put_u16(bytes, value.len() as u16);
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
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

    fn string_u16(&mut self, label: &str) -> Result<String, ProtocolError> {
        let len = usize::from(self.u16()?);
        self.string(len, label)
    }

    fn string(&mut self, len: usize, label: &str) -> Result<String, ProtocolError> {
        let value = self.bytes(len, label)?;
        std::str::from_utf8(value)
            .map(str::to_owned)
            .map_err(|error| invalid_error(format!("{label} is not valid UTF-8: {error}")))
    }

    fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.offset..]
    }

    fn consume_remaining(&mut self) {
        self.offset = self.bytes.len();
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

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("invalid SCP1 frame magic")]
    InvalidMagic,
    #[error("unsupported SCP1 protocol version {0}")]
    UnsupportedVersion(u8),
    #[error("payload length {0} exceeds the 1 MiB limit")]
    PayloadTooLarge(usize),
    #[error("frame length overflow")]
    LengthOverflow,
    #[error("truncated frame: need at least {minimum} bytes, got {actual}")]
    Truncated { minimum: usize, actual: usize },
    #[error("frame length mismatch: expected {expected}, got {actual}")]
    LengthMismatch { expected: usize, actual: usize },
    #[error("CRC32C mismatch: expected {expected:#010x}, got {actual:#010x}")]
    CrcMismatch { expected: u32, actual: u32 },
    #[error("invalid SCP1 payload: {0}")]
    InvalidPayload(String),
    #[error("unknown SCP1 message type {0:#04x}")]
    UnknownMessageType(u8),
    #[error("expected SCP1 message type {expected:#04x}, got {actual:#04x}")]
    UnexpectedMessageType { expected: u8, actual: u8 },
}

pub fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecoderStats {
    pub decoded_frames: u64,
    pub crc_errors: u64,
    pub malformed_headers: u64,
    pub discarded_bytes: u64,
}

#[derive(Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    ready: VecDeque<Frame>,
    stats: DecoderStats,
}

impl FrameDecoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
        self.parse_available();
    }

    pub fn drain_frames(&mut self) -> Vec<Frame> {
        self.ready.drain(..).collect()
    }

    pub fn stats(&self) -> DecoderStats {
        self.stats
    }

    fn parse_available(&mut self) {
        loop {
            let Some(magic_offset) = find_magic(&self.buffer) else {
                self.retain_possible_magic_prefix();
                break;
            };
            if magic_offset > 0 {
                self.discard_front(magic_offset);
            }
            if self.buffer.len() < FRAME_HEADER_LEN {
                break;
            }
            if self.buffer[4] != PROTOCOL_VERSION {
                self.stats.malformed_headers = self.stats.malformed_headers.saturating_add(1);
                self.discard_front(1);
                continue;
            }
            let payload_len = u32::from_le_bytes(
                self.buffer[12..16]
                    .try_into()
                    .expect("fixed header length checked"),
            ) as usize;
            if payload_len > MAX_PAYLOAD_LEN {
                self.stats.malformed_headers = self.stats.malformed_headers.saturating_add(1);
                self.discard_front(1);
                continue;
            }
            let Some(frame_len) = FRAME_HEADER_LEN
                .checked_add(payload_len)
                .and_then(|len| len.checked_add(FRAME_CRC_LEN))
            else {
                self.stats.malformed_headers = self.stats.malformed_headers.saturating_add(1);
                self.discard_front(1);
                continue;
            };
            if self.buffer.len() < frame_len {
                break;
            }
            match Frame::decode(&self.buffer[..frame_len]) {
                Ok(frame) => {
                    self.ready.push_back(frame);
                    self.stats.decoded_frames = self.stats.decoded_frames.saturating_add(1);
                    self.buffer.drain(..frame_len);
                }
                Err(ProtocolError::CrcMismatch { .. }) => {
                    self.stats.crc_errors = self.stats.crc_errors.saturating_add(1);
                    self.discard_front(1);
                }
                Err(_) => {
                    self.stats.malformed_headers = self.stats.malformed_headers.saturating_add(1);
                    self.discard_front(1);
                }
            }
        }
    }

    fn retain_possible_magic_prefix(&mut self) {
        let keep = (1..FRAME_MAGIC.len())
            .rev()
            .find(|&len| {
                self.buffer.len() >= len
                    && self.buffer[self.buffer.len() - len..] == FRAME_MAGIC[..len]
            })
            .unwrap_or(0);
        let discard = self.buffer.len().saturating_sub(keep);
        self.discard_front(discard);
    }

    fn discard_front(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        self.buffer.drain(..count);
        self.stats.discarded_bytes = self.stats.discarded_bytes.saturating_add(count as u64);
    }
}

fn find_magic(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(FRAME_MAGIC.len())
        .position(|window| window == FRAME_MAGIC)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, ProtocolError> {
    let raw = bytes
        .get(offset..offset + 2)
        .ok_or(ProtocolError::Truncated {
            minimum: offset + 2,
            actual: bytes.len(),
        })?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ProtocolError> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or(ProtocolError::Truncated {
            minimum: offset + 4,
            actual: bytes.len(),
        })?;
    Ok(u32::from_le_bytes(
        raw.try_into().expect("slice length checked"),
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, ProtocolError> {
    let raw = bytes
        .get(offset..offset + 8)
        .ok_or(ProtocolError::Truncated {
            minimum: offset + 8,
            actual: bytes.len(),
        })?;
    Ok(u64::from_le_bytes(
        raw.try_into().expect("slice length checked"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_matches_castagnoli_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }

    #[test]
    fn frame_round_trip_matches_golden_layout() {
        let frame = Frame::new(0x14, 3, 7, 11, 13, 17_u64.to_le_bytes().to_vec());

        let encoded = frame.encode().unwrap();

        assert_eq!(
            encoded,
            [
                0x53, 0x43, 0x50, 0x31, 0x01, 0x14, 0x03, 0x00, 0x07, 0x00, 0x00, 0x00, 0x08, 0x00,
                0x00, 0x00, 0x0b, 0x00, 0x00, 0x00, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x11, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1d, 0x23, 0x97, 0xcb,
            ]
        );
        assert_eq!(Frame::decode(&encoded).unwrap(), frame);
    }

    #[test]
    fn channel_table_descriptor_matches_frozen_v1_byte_layout() {
        let message = Message::ChannelTable(ChannelTable {
            revision: 0x0102_0304,
            channels: vec![ChannelDescriptor {
                channel_id: 2,
                kind: ChannelKind::Analog,
                wire_format: WireFormat::I16,
                scale: 1.0,
                offset: 0.0,
                unit: "V".to_owned(),
                name: "Ua".to_owned(),
            }],
        });

        let payload = message.encode_payload().unwrap();

        assert_eq!(
            payload,
            [
                0x04, 0x03, 0x02, 0x01, 0x01, 0x00, 0x02, 0x00, 0x00, 0x01, 0x00, 0x00, 0x80, 0x3f,
                0x00, 0x00, 0x00, 0x00, 0x01, 0x02, b'V', b'U', b'a',
            ]
        );
        assert_eq!(
            Message::decode(MSG_CHANNEL_TABLE, &payload).unwrap(),
            message
        );
    }

    #[test]
    fn decoder_handles_fragmentation_noise_and_crc_recovery() {
        let first = Frame::new(0x14, 0, 1, 9, 0, vec![1]).encode().unwrap();
        let mut corrupt = Frame::new(0x20, 0, 2, 9, 4, vec![2, 3]).encode().unwrap();
        corrupt[29] ^= 0x55;
        let last = Frame::new(0x15, 0, 3, 9, 0, vec![4]).encode().unwrap();
        let mut decoder = FrameDecoder::default();

        decoder.push(b"noiseS");
        decoder.push(&first[..9]);
        decoder.push(&first[9..]);
        decoder.push(&corrupt);
        decoder.push(&last);
        let frames = decoder.drain_frames();

        assert_eq!(
            frames
                .iter()
                .map(|frame| frame.sequence)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert!(decoder.stats().crc_errors >= 1);
        assert!(decoder.stats().discarded_bytes >= 5);
    }

    #[test]
    fn hello_message_round_trips() {
        let message = Message::Hello(Hello {
            client_capabilities: 0b111,
            max_payload: MAX_PAYLOAD_LEN as u32,
            client_name: "ScopeAnalyzer".to_owned(),
        });

        let payload = message.encode_payload().unwrap();

        assert_eq!(Message::decode(MSG_HELLO, &payload).unwrap(), message);
    }

    #[test]
    fn channel_table_rejects_duplicate_channel_ids() {
        let descriptor = ChannelDescriptor {
            channel_id: 0,
            kind: ChannelKind::Analog,
            wire_format: WireFormat::I16,
            scale: 0.1,
            offset: 0.0,
            unit: "V".to_owned(),
            name: "Ua".to_owned(),
        };
        let message = Message::ChannelTable(ChannelTable {
            revision: 1,
            channels: vec![descriptor.clone(), descriptor],
        });

        assert!(matches!(
            message.encode_payload(),
            Err(ProtocolError::InvalidPayload(_))
        ));
    }

    #[test]
    fn sample_batch_decodes_mixed_wire_formats() {
        let table = ChannelTable {
            revision: 5,
            channels: vec![
                ChannelDescriptor {
                    channel_id: 0,
                    kind: ChannelKind::Analog,
                    wire_format: WireFormat::I16,
                    scale: 0.5,
                    offset: 1.0,
                    unit: "V".to_owned(),
                    name: "Ua".to_owned(),
                },
                ChannelDescriptor {
                    channel_id: 1,
                    kind: ChannelKind::Digital,
                    wire_format: WireFormat::U8,
                    scale: 1.0,
                    offset: 0.0,
                    unit: String::new(),
                    name: "Trip".to_owned(),
                },
                ChannelDescriptor {
                    channel_id: 2,
                    kind: ChannelKind::Analog,
                    wire_format: WireFormat::F32,
                    scale: 1.0,
                    offset: 0.0,
                    unit: "A".to_owned(),
                    name: "Ia".to_owned(),
                },
            ],
        };
        let mut sample_data = Vec::new();
        sample_data.extend_from_slice(&(-2_i16).to_le_bytes());
        sample_data.push(1);
        sample_data.extend_from_slice(&1.5_f32.to_le_bytes());
        sample_data.extend_from_slice(&4_i16.to_le_bytes());
        sample_data.push(0);
        sample_data.extend_from_slice(&(-2.5_f32).to_le_bytes());
        let message = Message::SampleBatch(SampleBatch {
            channel_table_revision: 5,
            first_sample_index: 100,
            sample_period_ticks: 10,
            sample_count: 2,
            channel_ids: vec![0, 1, 2],
            sample_data,
        });
        let frame = Frame::new(
            MSG_SAMPLE_BATCH,
            0,
            9,
            7,
            1_000,
            message.encode_payload().unwrap(),
        );

        let decoded = decode_sample_frame(&frame, &table).unwrap();

        assert_eq!(decoded.first_sample_index, 100);
        assert_eq!(decoded.timestamp_ticks, 1_000);
        assert_eq!(decoded.channel_ids, vec![0, 1, 2]);
        assert_eq!(decoded.channels[0], vec![0.0, 3.0]);
        assert_eq!(decoded.channels[1], vec![1.0, 0.0]);
        assert_eq!(decoded.channels[2], vec![1.5, -2.5]);
        assert_eq!(decoded.raw_frame, frame.encode().unwrap());
    }

    #[test]
    fn control_messages_round_trip_and_reject_trailing_bytes() {
        let messages = vec![
            Message::HelloAck(HelloAck {
                device_capabilities: 3,
                max_payload: 4096,
                tick_hz: 1_000_000,
                channel_count: 2,
                max_batch_samples: 100,
                device_id: *b"SCOPE-SIM-V1----",
                firmware_name: "scope-sim".to_owned(),
            }),
            Message::Configure(Configure {
                sample_rate_hz: 10_000,
                batch_samples: 100,
                channel_mask: 3,
            }),
            Message::Start,
            Message::Stop,
            Message::CommandResult(CommandResult {
                request_sequence: 7,
                result_code: ResultCode::Ok,
                detail: "ok".to_owned(),
            }),
            Message::Ping(11),
            Message::Pong(11),
            Message::Status(Status {
                state: DeviceState::Streaming,
                produced_samples: 1000,
                dropped_samples: 2,
                tx_overruns: 1,
            }),
            Message::Error(CommandResult {
                request_sequence: 0,
                result_code: ResultCode::InternalError,
                detail: "fault".to_owned(),
            }),
        ];
        for message in messages {
            let payload = message.encode_payload().unwrap();
            assert_eq!(
                Message::decode(message.message_type(), &payload).unwrap(),
                message
            );
        }

        let mut ping = Message::Ping(5).encode_payload().unwrap();
        ping.push(0);
        assert!(matches!(
            Message::decode(MSG_PING, &ping),
            Err(ProtocolError::InvalidPayload(_))
        ));
    }

    #[test]
    fn configure_is_validated_against_negotiated_device_limits() {
        let hello = HelloAck {
            device_capabilities: 0,
            max_payload: 128,
            tick_hz: 1_000,
            channel_count: 1,
            max_batch_samples: 10,
            device_id: [1; 16],
            firmware_name: "test".to_owned(),
        };
        let table = ChannelTable {
            revision: 1,
            channels: vec![ChannelDescriptor {
                channel_id: 2,
                kind: ChannelKind::Analog,
                wire_format: WireFormat::I16,
                scale: 1.0,
                offset: 0.0,
                unit: "V".to_owned(),
                name: "Ua".to_owned(),
            }],
        };

        assert!(validate_configure_for_device(
            &Configure {
                sample_rate_hz: 1_000,
                batch_samples: 10,
                channel_mask: 1 << 2,
            },
            &hello,
            &table,
        )
        .is_ok());
        for invalid in [
            Configure {
                sample_rate_hz: 1_001,
                batch_samples: 10,
                channel_mask: 1 << 2,
            },
            Configure {
                sample_rate_hz: 1_000,
                batch_samples: 11,
                channel_mask: 1 << 2,
            },
            Configure {
                sample_rate_hz: 1_000,
                batch_samples: 10,
                channel_mask: 1 << 3,
            },
        ] {
            assert!(validate_configure_for_device(&invalid, &hello, &table).is_err());
        }
    }

    #[test]
    fn configure_command_result_detail_round_trips_actual_parameters() {
        let configure = Configure {
            sample_rate_hz: 20_000,
            batch_samples: 64,
            channel_mask: 0x8000_0000_0000_0001,
        };

        let detail = encode_configure_result_detail(&configure);

        assert_eq!(
            detail,
            "sample_rate_hz=20000;batch_samples=64;channel_mask=0x8000000000000001"
        );
        assert_eq!(decode_configure_result_detail(&detail).unwrap(), configure);
        assert!(decode_configure_result_detail("ok").is_err());
    }

    #[test]
    fn hello_rejects_invalid_utf8() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&0_u32.to_le_bytes());
        payload.extend_from_slice(&1024_u32.to_le_bytes());
        payload.extend_from_slice(&1_u16.to_le_bytes());
        payload.push(0xff);

        assert!(matches!(
            Message::decode(MSG_HELLO, &payload),
            Err(ProtocolError::InvalidPayload(_))
        ));
    }

    #[test]
    fn sample_batch_rejects_sample_index_overflow() {
        let batch = Message::SampleBatch(SampleBatch {
            channel_table_revision: 1,
            first_sample_index: u64::MAX,
            sample_period_ticks: 1,
            sample_count: 2,
            channel_ids: vec![0],
            sample_data: vec![0; 4],
        });

        assert!(matches!(
            batch.encode_payload(),
            Err(ProtocolError::InvalidPayload(_))
        ));
    }

    #[test]
    fn sample_batch_rejects_timestamp_overflow() {
        let table = ChannelTable {
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
        };
        let message = Message::SampleBatch(SampleBatch {
            channel_table_revision: 1,
            first_sample_index: 0,
            sample_period_ticks: 2,
            sample_count: 2,
            channel_ids: vec![0],
            sample_data: vec![0; 4],
        });
        let frame = Frame::new(
            MSG_SAMPLE_BATCH,
            0,
            1,
            1,
            u64::MAX,
            message.encode_payload().unwrap(),
        );

        assert!(matches!(
            decode_sample_frame(&frame, &table),
            Err(ProtocolError::InvalidPayload(_))
        ));
    }

    #[test]
    fn channel_table_rejects_unknown_wire_format() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&1_u32.to_le_bytes());
        payload.extend_from_slice(&1_u16.to_le_bytes());
        payload.extend_from_slice(&0_u16.to_le_bytes());
        payload.push(ChannelKind::Analog as u8);
        payload.push(99);
        payload.extend_from_slice(&1.0_f32.to_le_bytes());
        payload.extend_from_slice(&0.0_f32.to_le_bytes());
        payload.push(1);
        payload.extend_from_slice(b"V");
        payload.push(2);
        payload.extend_from_slice(b"Ua");

        assert!(matches!(
            Message::decode(MSG_CHANNEL_TABLE, &payload),
            Err(ProtocolError::InvalidPayload(_))
        ));
    }

    #[test]
    fn sample_batch_rejects_revision_mismatch_and_truncated_data() {
        let table = ChannelTable {
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
        };
        let make_frame = |revision, sample_data| {
            let message = Message::SampleBatch(SampleBatch {
                channel_table_revision: revision,
                first_sample_index: 0,
                sample_period_ticks: 1,
                sample_count: 1,
                channel_ids: vec![0],
                sample_data,
            });
            Frame::new(
                MSG_SAMPLE_BATCH,
                0,
                1,
                1,
                0,
                message.encode_payload().unwrap(),
            )
        };

        assert!(matches!(
            decode_sample_frame(&make_frame(2, vec![0; 2]), &table),
            Err(ProtocolError::InvalidPayload(_))
        ));
        assert!(matches!(
            decode_sample_frame(&make_frame(1, vec![0]), &table),
            Err(ProtocolError::InvalidPayload(_))
        ));
    }
}
