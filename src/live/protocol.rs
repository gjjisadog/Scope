use std::collections::VecDeque;
use thiserror::Error;

pub const FRAME_MAGIC: [u8; 4] = *b"SCP1";
pub const PROTOCOL_VERSION: u8 = 1;
pub const FRAME_HEADER_LEN: usize = 28;
pub const FRAME_CRC_LEN: usize = 4;
pub const MAX_PAYLOAD_LEN: usize = 1024 * 1024;

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

        assert_eq!(&encoded[..4], b"SCP1");
        assert_eq!(u32::from_le_bytes(encoded[12..16].try_into().unwrap()), 8);
        assert_eq!(Frame::decode(&encoded).unwrap(), frame);
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
}
