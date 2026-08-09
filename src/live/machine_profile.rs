//! Machine-readable protocol contracts used by hardware acceptance tools.

use std::{collections::BTreeSet, fs, path::Path};

use serde::Deserialize;
use thiserror::Error;

use super::{
    protocol::{ChannelDescriptor, ChannelTable, WireFormat},
    protocol_v2::SampleDomain,
    protocol_v2_r2::{
        StreamTableR2, CAPABILITY_V2_COMPRESSED_METADATA, CAPABILITY_V2_MULTI_STREAM,
        CAPABILITY_V2_STREAMS_R2,
    },
};

const SCALE_EPSILON: f32 = 1.0e-6;
const HYBRID30K_R2_PROFILE_JSON: &str = include_str!("../../profiles/hybrid30k-r2.json");

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineProfile {
    pub profile_version: u32,
    pub profile_name: String,
    pub protocol: String,
    pub protocol_revision: u8,
    pub stream_table_revision: u32,
    pub required_capabilities: Vec<String>,
    pub causal_groups: Vec<ProfileCausalGroup>,
    pub streams: Vec<ProfileStream>,
    pub channels: Vec<ProfileChannel>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCausalGroup {
    pub consistency_group: u16,
    pub logical_cycle_rate_hz: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileStream {
    pub name: String,
    pub required: bool,
    pub domain: String,
    pub rate_hz: u32,
    pub logical_cycle_step: u32,
    pub consistency_group: u16,
    pub required_channels: Vec<String>,
    pub optional_channels: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileChannel {
    pub name: String,
    pub wire_format: String,
    pub scale: f32,
    pub unit: String,
}

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("cannot read machine profile: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid machine profile schema: {0}")]
    Schema(#[from] serde_json::Error),
    #[error("invalid machine profile: {0}")]
    Invalid(String),
    #[error("profile requires capability {0}")]
    CapabilityMismatch(String),
    #[error("profile stream {0} is missing")]
    MissingStream(String),
    #[error("profile stream {stream} has domain {actual}, expected {expected}")]
    StreamDomainMismatch {
        stream: String,
        expected: String,
        actual: String,
    },
    #[error("profile stream {stream} has rate {actual}, expected {expected}")]
    StreamRateMismatch {
        stream: String,
        expected: u32,
        actual: u32,
    },
    #[error("profile stream {stream} has logical step {actual}, expected {expected}")]
    LogicalStepMismatch {
        stream: String,
        expected: u32,
        actual: u32,
    },
    #[error("profile stream {stream} has consistency group {actual}, expected {expected}")]
    ConsistencyGroupMismatch {
        stream: String,
        expected: u16,
        actual: u16,
    },
    #[error("profile required channel {0} is missing")]
    MissingChannel(String),
    #[error("profile channel {channel} has format {actual}, expected {expected}")]
    ChannelFormatMismatch {
        channel: String,
        expected: String,
        actual: String,
    },
    #[error("profile channel {channel} has scale {actual}, expected {expected}")]
    ChannelScaleMismatch {
        channel: String,
        expected: f32,
        actual: f32,
    },
    #[error("profile channel {channel} has unit {actual:?}, expected {expected:?}")]
    ChannelUnitMismatch {
        channel: String,
        expected: String,
        actual: String,
    },
    #[error("profile protocol mismatch: {0}")]
    ProtocolMismatch(String),
}

impl ProfileError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "profile_io_error",
            Self::Schema(_) | Self::Invalid(_) => "profile_schema_invalid",
            Self::CapabilityMismatch(_) => "profile_capability_mismatch",
            Self::MissingStream(_) => "profile_missing_stream",
            Self::StreamDomainMismatch { .. } => "profile_stream_domain_mismatch",
            Self::StreamRateMismatch { .. } => "profile_stream_rate_mismatch",
            Self::LogicalStepMismatch { .. } => "profile_logical_step_mismatch",
            Self::ConsistencyGroupMismatch { .. } => "profile_consistency_group_mismatch",
            Self::MissingChannel(_) => "profile_missing_channel",
            Self::ChannelFormatMismatch { .. } => "profile_channel_format_mismatch",
            Self::ChannelScaleMismatch { .. } => "profile_channel_scale_mismatch",
            Self::ChannelUnitMismatch { .. } => "profile_channel_unit_mismatch",
            Self::ProtocolMismatch(_) => "profile_protocol_mismatch",
        }
    }
}

impl MachineProfile {
    pub fn load_named(name: &str) -> Result<Self, ProfileError> {
        match name {
            "hybrid30k" | "hybrid30k-r2" => Self::from_json(HYBRID30K_R2_PROFILE_JSON),
            other => Self::load(other),
        }
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ProfileError> {
        let json = fs::read_to_string(path)?;
        Self::from_json(&json)
    }

    pub fn from_json(json: &str) -> Result<Self, ProfileError> {
        let profile: Self = serde_json::from_str(json)?;
        profile.validate_schema()?;
        Ok(profile)
    }

    pub fn validate_compatibility(
        &self,
        capabilities: u32,
        channels: &ChannelTable,
        streams: &StreamTableR2,
    ) -> Result<(), ProfileError> {
        if self.protocol != "scp1-v2-r2" || self.protocol_revision != 2 {
            return Err(ProfileError::ProtocolMismatch(format!(
                "expected scp1-v2-r2 revision 2, got {} revision {}",
                self.protocol, self.protocol_revision
            )));
        }
        if streams.revision != self.stream_table_revision {
            return Err(ProfileError::ProtocolMismatch(format!(
                "stream table revision {} does not match {}",
                streams.revision, self.stream_table_revision
            )));
        }
        for capability in &self.required_capabilities {
            let bit = capability_bit(capability)?;
            if capabilities & bit == 0 {
                return Err(ProfileError::CapabilityMismatch(capability.clone()));
            }
        }
        for expected in &self.streams {
            let domain = parse_domain(&expected.domain)?;
            let Some(actual) = streams.streams.iter().find(|value| value.domain == domain) else {
                if expected.required {
                    return Err(ProfileError::MissingStream(expected.name.clone()));
                }
                continue;
            };
            if actual.sample_rate_hz != expected.rate_hz {
                return Err(ProfileError::StreamRateMismatch {
                    stream: expected.name.clone(),
                    expected: expected.rate_hz,
                    actual: actual.sample_rate_hz,
                });
            }
            if actual.logical_cycle_step != expected.logical_cycle_step {
                return Err(ProfileError::LogicalStepMismatch {
                    stream: expected.name.clone(),
                    expected: expected.logical_cycle_step,
                    actual: actual.logical_cycle_step,
                });
            }
            if actual.consistency_group != expected.consistency_group {
                return Err(ProfileError::ConsistencyGroupMismatch {
                    stream: expected.name.clone(),
                    expected: expected.consistency_group,
                    actual: actual.consistency_group,
                });
            }
            for name in &expected.required_channels {
                let channel = channels
                    .channels
                    .iter()
                    .find(|value| {
                        value.name == *name && actual.channel_ids.contains(&value.channel_id)
                    })
                    .ok_or_else(|| ProfileError::MissingChannel(name.clone()))?;
                self.validate_channel(channel)?;
            }
            for name in &expected.optional_channels {
                if let Some(channel) = channels.channels.iter().find(|value| {
                    value.name == *name && actual.channel_ids.contains(&value.channel_id)
                }) {
                    self.validate_channel(channel)?;
                }
            }
        }
        Ok(())
    }

    pub fn stream(&self, domain: SampleDomain) -> Option<&ProfileStream> {
        self.streams
            .iter()
            .find(|stream| parse_domain(&stream.domain).ok() == Some(domain))
    }

    fn validate_schema(&self) -> Result<(), ProfileError> {
        if self.profile_version == 0 || self.profile_name.trim().is_empty() {
            return Err(ProfileError::Invalid(
                "profile_version and profile_name are required".to_owned(),
            ));
        }
        if self.causal_groups.is_empty() || self.streams.is_empty() || self.channels.is_empty() {
            return Err(ProfileError::Invalid(
                "causal_groups, streams, and channels must not be empty".to_owned(),
            ));
        }
        let names = self
            .channels
            .iter()
            .map(|channel| channel.name.as_str())
            .collect::<BTreeSet<_>>();
        if names.len() != self.channels.len() {
            return Err(ProfileError::Invalid(
                "channel names must be unique".to_owned(),
            ));
        }
        for stream in &self.streams {
            parse_domain(&stream.domain)?;
            if stream.rate_hz == 0 || stream.logical_cycle_step == 0 {
                return Err(ProfileError::Invalid(format!(
                    "stream {} rate and logical step must be non-zero",
                    stream.name
                )));
            }
            for name in stream
                .required_channels
                .iter()
                .chain(&stream.optional_channels)
            {
                if !names.contains(name.as_str()) {
                    return Err(ProfileError::Invalid(format!(
                        "stream {} references undefined channel {name}",
                        stream.name
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_channel(&self, actual: &ChannelDescriptor) -> Result<(), ProfileError> {
        let expected = self
            .channels
            .iter()
            .find(|value| value.name == actual.name)
            .ok_or_else(|| ProfileError::MissingChannel(actual.name.clone()))?;
        let format = wire_format_name(actual.wire_format);
        if format != expected.wire_format {
            return Err(ProfileError::ChannelFormatMismatch {
                channel: actual.name.clone(),
                expected: expected.wire_format.clone(),
                actual: format.to_owned(),
            });
        }
        if (actual.scale - expected.scale).abs() > SCALE_EPSILON {
            return Err(ProfileError::ChannelScaleMismatch {
                channel: actual.name.clone(),
                expected: expected.scale,
                actual: actual.scale,
            });
        }
        if actual.unit != expected.unit {
            return Err(ProfileError::ChannelUnitMismatch {
                channel: actual.name.clone(),
                expected: expected.unit.clone(),
                actual: actual.unit.clone(),
            });
        }
        Ok(())
    }
}

fn capability_bit(name: &str) -> Result<u32, ProfileError> {
    match name {
        "CAPABILITY_V2_STREAMS_R2" => Ok(CAPABILITY_V2_STREAMS_R2),
        "CAPABILITY_V2_MULTI_STREAM" => Ok(CAPABILITY_V2_MULTI_STREAM),
        "CAPABILITY_V2_COMPRESSED_METADATA" => Ok(CAPABILITY_V2_COMPRESSED_METADATA),
        _ => Err(ProfileError::Invalid(format!(
            "unknown required capability {name}"
        ))),
    }
}

fn parse_domain(name: &str) -> Result<SampleDomain, ProfileError> {
    match name {
        "FAST32K" => Ok(SampleDomain::Fast32k),
        "CTRL8K" => Ok(SampleDomain::Control8k),
        "SLOW1K" => Ok(SampleDomain::Slow1k),
        _ => Err(ProfileError::Invalid(format!(
            "unknown stream domain {name}"
        ))),
    }
}

fn wire_format_name(format: WireFormat) -> &'static str {
    match format {
        WireFormat::I16 => "I16",
        WireFormat::I32 => "I32",
        WireFormat::F32 => "F32",
        WireFormat::U8 => "U8",
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::*;
    use crate::live::{
        protocol::{ChannelDescriptor, ChannelKind},
        protocol_v2::{CapturePhase, SignalOwner, SignalRole, StreamChannelBinding},
        protocol_v2_r2::{CausalGroupDescriptorR2, StreamDescriptorR2},
    };

    fn compatible_tables() -> (ChannelTable, StreamTableR2) {
        let channels = ChannelTable {
            revision: 1,
            channels: vec![
                channel(0, "Ia", "A", WireFormat::I16, 0.01),
                channel(1, "Ib", "A", WireFormat::I16, 0.01),
                channel(2, "Ic", "A", WireFormat::I16, 0.01),
                channel(3, "Vdc", "V", WireFormat::I16, 0.1),
                channel(4, "SampleValid", "", WireFormat::U8, 1.0),
                channel(5, "RunState", "", WireFormat::U8, 1.0),
            ],
        };
        let streams = StreamTableR2 {
            revision: 3,
            causal_groups: vec![CausalGroupDescriptorR2 {
                consistency_group: 1,
                logical_cycle_rate_hz: 32_000,
                max_reorder_cycles: 64,
            }],
            streams: vec![
                stream(2, SampleDomain::Control8k, 8_000, 4, vec![0, 1, 2, 3, 4]),
                stream(3, SampleDomain::Slow1k, 1_000, 32, vec![5]),
            ],
            bindings: (0..=5)
                .map(|channel_id| StreamChannelBinding {
                    channel_id,
                    stream_id: if channel_id < 5 { 2 } else { 3 },
                    owner: SignalOwner::Cpu2,
                    role: SignalRole::Metadata,
                })
                .collect(),
            causal_relations: Vec::new(),
        };
        (channels, streams)
    }

    fn channel(
        id: u16,
        name: &str,
        unit: &str,
        format: WireFormat,
        scale: f32,
    ) -> ChannelDescriptor {
        ChannelDescriptor {
            channel_id: id,
            kind: ChannelKind::Analog,
            wire_format: format,
            scale,
            offset: 0.0,
            unit: unit.to_owned(),
            name: name.to_owned(),
        }
    }

    fn stream(
        id: u16,
        domain: SampleDomain,
        rate: u32,
        step: u32,
        channel_ids: Vec<u16>,
    ) -> StreamDescriptorR2 {
        StreamDescriptorR2 {
            stream_id: id,
            domain,
            capture_phase: match domain {
                SampleDomain::Fast32k => CapturePhase::AfterClaComplete,
                SampleDomain::Control8k => CapturePhase::ControlCycleEnd,
                SampleDomain::Slow1k => CapturePhase::LogicTaskEnd,
            },
            sample_rate_hz: rate,
            consistency_group: 1,
            logical_cycle_step: step,
            channel_ids,
        }
    }

    #[test]
    fn hybrid30k_profile_loads_and_matches_required_contract() {
        let profile = MachineProfile::load_named("hybrid30k").unwrap();
        let (channels, streams) = compatible_tables();
        profile
            .validate_compatibility(
                CAPABILITY_V2_STREAMS_R2
                    | CAPABILITY_V2_MULTI_STREAM
                    | CAPABILITY_V2_COMPRESSED_METADATA,
                &channels,
                &streams,
            )
            .unwrap();
    }

    #[test]
    fn built_in_profile_aliases_do_not_require_a_runtime_source_tree() {
        for name in ["hybrid30k", "hybrid30k-r2"] {
            let profile = MachineProfile::load_named(name).unwrap();
            assert_eq!(profile.profile_name, "hybrid30k-r2");
        }
    }

    #[test]
    fn external_profile_paths_report_valid_invalid_and_missing_files() {
        let suffix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let valid = std::env::temp_dir().join(format!("scope-profile-{suffix}.json"));
        let invalid = std::env::temp_dir().join(format!("scope-profile-invalid-{suffix}.json"));
        let missing = std::env::temp_dir().join(format!("scope-profile-missing-{suffix}.json"));

        fs::write(&valid, HYBRID30K_R2_PROFILE_JSON).unwrap();
        fs::write(&invalid, r#"{"profile_version":1,"unknown":true}"#).unwrap();

        assert_eq!(
            MachineProfile::load_named(valid.to_str().unwrap())
                .unwrap()
                .profile_name,
            "hybrid30k-r2"
        );
        assert_eq!(
            MachineProfile::load_named(invalid.to_str().unwrap())
                .unwrap_err()
                .code(),
            "profile_schema_invalid"
        );
        assert_eq!(
            MachineProfile::load_named(missing.to_str().unwrap())
                .unwrap_err()
                .code(),
            "profile_io_error"
        );

        fs::remove_file(valid).unwrap();
        fs::remove_file(invalid).unwrap();
    }

    #[test]
    fn illegal_schema_is_rejected() {
        let error =
            MachineProfile::from_json(r#"{"profile_version":1,"unknown":true}"#).unwrap_err();
        assert_eq!(error.code(), "profile_schema_invalid");
    }

    #[test]
    fn profile_reports_precise_stream_channel_rate_step_and_scale_errors() {
        let profile = MachineProfile::load_named("hybrid30k").unwrap();
        let capabilities = CAPABILITY_V2_STREAMS_R2
            | CAPABILITY_V2_MULTI_STREAM
            | CAPABILITY_V2_COMPRESSED_METADATA;
        let (channels, mut streams) = compatible_tables();
        streams.streams.remove(0);
        assert_eq!(
            profile
                .validate_compatibility(capabilities, &channels, &streams)
                .unwrap_err()
                .code(),
            "profile_missing_stream"
        );

        let (mut channels, mut streams) = compatible_tables();
        channels.channels.remove(0);
        streams.streams[0].channel_ids.remove(0);
        streams.bindings.remove(0);
        assert_eq!(
            profile
                .validate_compatibility(capabilities, &channels, &streams)
                .unwrap_err()
                .code(),
            "profile_missing_channel"
        );

        let (channels, mut streams) = compatible_tables();
        streams.streams[0].sample_rate_hz = 7_999;
        assert_eq!(
            profile
                .validate_compatibility(capabilities, &channels, &streams)
                .unwrap_err()
                .code(),
            "profile_stream_rate_mismatch"
        );

        let (channels, mut streams) = compatible_tables();
        streams.streams[0].logical_cycle_step = 5;
        assert_eq!(
            profile
                .validate_compatibility(capabilities, &channels, &streams)
                .unwrap_err()
                .code(),
            "profile_logical_step_mismatch"
        );

        let (mut channels, streams) = compatible_tables();
        channels.channels[0].scale = 0.02;
        assert_eq!(
            profile
                .validate_compatibility(capabilities, &channels, &streams)
                .unwrap_err()
                .code(),
            "profile_channel_scale_mismatch"
        );
    }
}
