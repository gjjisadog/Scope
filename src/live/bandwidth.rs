use thiserror::Error;

use super::protocol::{
    ChannelTable, Configure, FRAME_CRC_LEN, FRAME_HEADER_LEN, MAX_BATCH_SAMPLES, MAX_PAYLOAD_LEN,
};

pub const SERIAL_SAFE_UTILIZATION: f64 = 0.70;
pub const SERIAL_CRITICAL_UTILIZATION: f64 = 0.90;
const SAMPLE_BATCH_FIXED_PAYLOAD_BYTES: usize = 20;
const CHANNEL_ID_BYTES: usize = 2;
const SERIAL_BITS_PER_BYTE_8N1: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LinkBudgetTransport {
    Serial {
        baud: u32,
    },
    Tcp {
        expected_bits_per_second: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetSeverity {
    Safe,
    Warning,
    Critical,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedChannelBudget {
    pub channel_id: u16,
    pub name: String,
    pub bytes_per_sample: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinkBudgetResult {
    pub selected_channels: Vec<SelectedChannelBudget>,
    pub bytes_per_sample: usize,
    pub sample_data_bytes_per_frame: usize,
    pub payload_bytes_per_frame: usize,
    pub frame_bytes: usize,
    pub frames_per_second: f64,
    pub bytes_per_second: f64,
    pub serial_bits_per_second: Option<f64>,
    pub utilization: Option<f64>,
    pub batch_latency_seconds: f64,
    pub severity: BudgetSeverity,
    pub negotiated_max_payload: usize,
    pub suggested_batch_samples: Option<u16>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LinkBudgetError {
    #[error("sample rate must be greater than zero")]
    ZeroSampleRate,
    #[error("batch samples must be in 1..={MAX_BATCH_SAMPLES}")]
    InvalidBatchSamples,
    #[error("channel mask must select at least one channel")]
    EmptyChannelMask,
    #[error("channel mask selects a channel that is absent from the channel table")]
    UnknownChannel,
    #[error("serial baud must be greater than zero")]
    ZeroBaud,
    #[error("link budget arithmetic overflow")]
    Overflow,
}

pub fn calculate_link_budget(
    table: &ChannelTable,
    configure: &Configure,
    transport: LinkBudgetTransport,
    negotiated_max_payload: usize,
) -> Result<LinkBudgetResult, LinkBudgetError> {
    validate_request(table, configure, transport)?;
    let selected_channels = selected_channels(table, configure.channel_mask);
    let bytes_per_sample = selected_channels
        .iter()
        .try_fold(0_usize, |total, channel| {
            total
                .checked_add(channel.bytes_per_sample)
                .ok_or(LinkBudgetError::Overflow)
        })?;
    let sizes = frame_sizes(
        selected_channels.len(),
        bytes_per_sample,
        usize::from(configure.batch_samples),
    )?;
    let frames_per_second =
        f64::from(configure.sample_rate_hz) / f64::from(configure.batch_samples);
    let bytes_per_second = sizes.frame_bytes as f64 * frames_per_second;
    let batch_latency_seconds =
        f64::from(configure.batch_samples) / f64::from(configure.sample_rate_hz);
    let effective_max_payload = negotiated_max_payload.min(MAX_PAYLOAD_LEN);
    let payload_exceeded = sizes.payload_bytes > effective_max_payload;

    let (serial_bits_per_second, utilization, mut severity) = match transport {
        LinkBudgetTransport::Serial { baud } => {
            let bits = bytes_per_second * SERIAL_BITS_PER_BYTE_8N1 as f64;
            let utilization = bits / f64::from(baud);
            let severity = if utilization <= SERIAL_SAFE_UTILIZATION {
                BudgetSeverity::Safe
            } else if utilization <= SERIAL_CRITICAL_UTILIZATION {
                BudgetSeverity::Warning
            } else {
                BudgetSeverity::Critical
            };
            (Some(bits), Some(utilization), severity)
        }
        LinkBudgetTransport::Tcp {
            expected_bits_per_second,
        } => {
            if let Some(expected) = expected_bits_per_second.filter(|value| *value > 0) {
                let utilization = bytes_per_second * 8.0 / expected as f64;
                let severity = if utilization <= SERIAL_SAFE_UTILIZATION {
                    BudgetSeverity::Safe
                } else if utilization <= SERIAL_CRITICAL_UTILIZATION {
                    BudgetSeverity::Warning
                } else {
                    BudgetSeverity::Critical
                };
                (None, Some(utilization), severity)
            } else {
                (None, None, BudgetSeverity::Unknown)
            }
        }
    };
    if payload_exceeded {
        severity = BudgetSeverity::Critical;
    }

    let suggested_batch_samples = match transport {
        LinkBudgetTransport::Serial { baud } => suggest_safe_batch(
            selected_channels.len(),
            bytes_per_sample,
            configure.sample_rate_hz,
            baud,
            effective_max_payload,
        )?,
        LinkBudgetTransport::Tcp { .. } => None,
    };

    Ok(LinkBudgetResult {
        selected_channels,
        bytes_per_sample,
        sample_data_bytes_per_frame: sizes.sample_data_bytes,
        payload_bytes_per_frame: sizes.payload_bytes,
        frame_bytes: sizes.frame_bytes,
        frames_per_second,
        bytes_per_second,
        serial_bits_per_second,
        utilization,
        batch_latency_seconds,
        severity,
        negotiated_max_payload: effective_max_payload,
        suggested_batch_samples,
    })
}

fn validate_request(
    table: &ChannelTable,
    configure: &Configure,
    transport: LinkBudgetTransport,
) -> Result<(), LinkBudgetError> {
    if configure.sample_rate_hz == 0 {
        return Err(LinkBudgetError::ZeroSampleRate);
    }
    if configure.batch_samples == 0 || usize::from(configure.batch_samples) > MAX_BATCH_SAMPLES {
        return Err(LinkBudgetError::InvalidBatchSamples);
    }
    if configure.channel_mask == 0 {
        return Err(LinkBudgetError::EmptyChannelMask);
    }
    let known_mask = table
        .channels
        .iter()
        .fold(0_u64, |mask, channel| mask | (1_u64 << channel.channel_id));
    if configure.channel_mask & !known_mask != 0 {
        return Err(LinkBudgetError::UnknownChannel);
    }
    if matches!(transport, LinkBudgetTransport::Serial { baud: 0 }) {
        return Err(LinkBudgetError::ZeroBaud);
    }
    Ok(())
}

fn selected_channels(table: &ChannelTable, mask: u64) -> Vec<SelectedChannelBudget> {
    table
        .channels
        .iter()
        .filter(|channel| mask & (1_u64 << channel.channel_id) != 0)
        .map(|channel| SelectedChannelBudget {
            channel_id: channel.channel_id,
            name: channel.name.clone(),
            bytes_per_sample: channel.wire_format.byte_width(),
        })
        .collect()
}

#[derive(Clone, Copy)]
struct FrameSizes {
    sample_data_bytes: usize,
    payload_bytes: usize,
    frame_bytes: usize,
}

fn frame_sizes(
    channel_count: usize,
    bytes_per_sample: usize,
    batch_samples: usize,
) -> Result<FrameSizes, LinkBudgetError> {
    let sample_data_bytes = bytes_per_sample
        .checked_mul(batch_samples)
        .ok_or(LinkBudgetError::Overflow)?;
    let channel_list_bytes = channel_count
        .checked_mul(CHANNEL_ID_BYTES)
        .ok_or(LinkBudgetError::Overflow)?;
    let payload_bytes = SAMPLE_BATCH_FIXED_PAYLOAD_BYTES
        .checked_add(channel_list_bytes)
        .and_then(|value| value.checked_add(sample_data_bytes))
        .ok_or(LinkBudgetError::Overflow)?;
    let frame_bytes = FRAME_HEADER_LEN
        .checked_add(payload_bytes)
        .and_then(|value| value.checked_add(FRAME_CRC_LEN))
        .ok_or(LinkBudgetError::Overflow)?;
    Ok(FrameSizes {
        sample_data_bytes,
        payload_bytes,
        frame_bytes,
    })
}

fn suggest_safe_batch(
    channel_count: usize,
    bytes_per_sample: usize,
    sample_rate_hz: u32,
    baud: u32,
    max_payload: usize,
) -> Result<Option<u16>, LinkBudgetError> {
    for batch in 1..=MAX_BATCH_SAMPLES {
        let sizes = frame_sizes(channel_count, bytes_per_sample, batch)?;
        if sizes.payload_bytes > max_payload {
            break;
        }
        let frames_per_second = f64::from(sample_rate_hz) / batch as f64;
        let utilization =
            sizes.frame_bytes as f64 * frames_per_second * SERIAL_BITS_PER_BYTE_8N1 as f64
                / f64::from(baud);
        if utilization <= SERIAL_SAFE_UTILIZATION {
            return Ok(u16::try_from(batch).ok());
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::protocol::{ChannelDescriptor, ChannelKind, Message, SampleBatch, WireFormat};

    fn table() -> ChannelTable {
        ChannelTable {
            revision: 1,
            channels: vec![
                descriptor(0, "I16", WireFormat::I16),
                descriptor(1, "F32", WireFormat::F32),
                descriptor(2, "U8", WireFormat::U8),
            ],
        }
    }

    fn descriptor(channel_id: u16, name: &str, wire_format: WireFormat) -> ChannelDescriptor {
        ChannelDescriptor {
            channel_id,
            kind: if wire_format == WireFormat::U8 {
                ChannelKind::Digital
            } else {
                ChannelKind::Analog
            },
            wire_format,
            scale: 1.0,
            offset: 0.0,
            unit: String::new(),
            name: name.to_owned(),
        }
    }

    #[test]
    fn predicted_payload_matches_protocol_encoder() {
        let configure = Configure {
            sample_rate_hz: 1_000,
            batch_samples: 10,
            channel_mask: 0b111,
        };
        let result = calculate_link_budget(
            &table(),
            &configure,
            LinkBudgetTransport::Serial { baud: 115_200 },
            MAX_PAYLOAD_LEN,
        )
        .unwrap();
        let message = Message::SampleBatch(SampleBatch {
            channel_table_revision: 1,
            first_sample_index: 0,
            sample_period_ticks: 1,
            sample_count: 10,
            channel_ids: vec![0, 1, 2],
            sample_data: vec![0; 10 * (2 + 4 + 1)],
        });
        let encoded_payload = message.encode_payload().unwrap();
        assert_eq!(result.bytes_per_sample, 7);
        assert_eq!(result.sample_data_bytes_per_frame, 70);
        assert_eq!(result.payload_bytes_per_frame, encoded_payload.len());
        assert_eq!(
            result.frame_bytes,
            FRAME_HEADER_LEN + encoded_payload.len() + FRAME_CRC_LEN
        );
        assert_eq!(result.severity, BudgetSeverity::Critical);
    }

    #[test]
    fn serial_budget_reports_safe_batch_and_latency() {
        let configure = Configure {
            sample_rate_hz: 1_000,
            batch_samples: 1,
            channel_mask: 0b001,
        };
        let result = calculate_link_budget(
            &table(),
            &configure,
            LinkBudgetTransport::Serial { baud: 921_600 },
            MAX_PAYLOAD_LEN,
        )
        .unwrap();
        assert_eq!(result.severity, BudgetSeverity::Safe);
        assert_eq!(result.suggested_batch_samples, Some(1));
        assert!((result.batch_latency_seconds - 0.001).abs() < f64::EPSILON);
    }

    #[test]
    fn tcp_without_expected_capacity_is_advisory() {
        let configure = Configure {
            sample_rate_hz: 10_000,
            batch_samples: 100,
            channel_mask: 0b011,
        };
        let result = calculate_link_budget(
            &table(),
            &configure,
            LinkBudgetTransport::Tcp {
                expected_bits_per_second: None,
            },
            MAX_PAYLOAD_LEN,
        )
        .unwrap();
        assert_eq!(result.severity, BudgetSeverity::Unknown);
        assert_eq!(result.utilization, None);
        assert_eq!(result.suggested_batch_samples, None);
    }

    #[test]
    fn payload_limit_is_critical_even_when_serial_capacity_is_high() {
        let configure = Configure {
            sample_rate_hz: 100,
            batch_samples: 100,
            channel_mask: 0b011,
        };
        let result = calculate_link_budget(
            &table(),
            &configure,
            LinkBudgetTransport::Serial { baud: 10_000_000 },
            128,
        )
        .unwrap();
        assert!(result.payload_bytes_per_frame > 128);
        assert_eq!(result.severity, BudgetSeverity::Critical);
        assert_eq!(result.suggested_batch_samples, Some(1));
    }

    #[test]
    fn rejects_unknown_channels_and_zero_baud() {
        let unknown = Configure {
            sample_rate_hz: 1_000,
            batch_samples: 10,
            channel_mask: 1 << 8,
        };
        assert_eq!(
            calculate_link_budget(
                &table(),
                &unknown,
                LinkBudgetTransport::Serial { baud: 115_200 },
                MAX_PAYLOAD_LEN,
            ),
            Err(LinkBudgetError::UnknownChannel)
        );
        let valid = Configure {
            channel_mask: 1,
            ..unknown
        };
        assert_eq!(
            calculate_link_budget(
                &table(),
                &valid,
                LinkBudgetTransport::Serial { baud: 0 },
                MAX_PAYLOAD_LEN,
            ),
            Err(LinkBudgetError::ZeroBaud)
        );
    }
}
