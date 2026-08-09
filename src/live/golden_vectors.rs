use std::{collections::BTreeMap, fs, path::Path};

use serde::Deserialize;

use super::{
    protocol::{Frame, Message},
    protocol_v2::SampleDomain,
    protocol_v2_r2::{MessageV2R2, MetadataEncodingR2},
};

#[derive(Debug, Deserialize)]
struct ExpectedFrame {
    message_type: u8,
    payload_length: usize,
    frame_length: usize,
    crc32c: String,
    sequence: u32,
    session_id: u32,
    hex: String,
}

#[test]
fn frozen_r2_dsp_vectors_encode_decode_and_crc_match() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("scp1-v2-r2");
    let expected: BTreeMap<String, ExpectedFrame> =
        serde_json::from_slice(&fs::read(directory.join("expected.json")).unwrap()).unwrap();
    assert_eq!(expected.len(), 8);

    for (name, metadata) in expected {
        let golden = fs::read(directory.join(&name)).unwrap();
        assert_eq!(golden.len(), metadata.frame_length, "{name}");
        assert_eq!(hex(&golden), metadata.hex, "{name}");
        let crc = u32::from_le_bytes(golden[golden.len() - 4..].try_into().unwrap());
        assert_eq!(format!("0x{crc:08x}"), metadata.crc32c, "{name}");

        let frame = Frame::decode(&golden).unwrap();
        assert_eq!(frame.message_type, metadata.message_type, "{name}");
        assert_eq!(frame.payload.len(), metadata.payload_length, "{name}");
        assert_eq!(frame.sequence, metadata.sequence, "{name}");
        assert_eq!(frame.session_id, metadata.session_id, "{name}");

        let encoded = match frame.message_type {
            0x33..=0x35 => {
                let message = MessageV2R2::decode(frame.message_type, &frame.payload).unwrap();
                assert_r2_key_fields(&name, &message);
                message
                    .into_frame(
                        frame.flags,
                        frame.sequence,
                        frame.session_id,
                        frame.timestamp_ticks,
                    )
                    .unwrap()
                    .encode()
                    .unwrap()
            }
            _ => {
                let message = Message::decode(frame.message_type, &frame.payload).unwrap();
                assert_common_key_fields(&name, &message);
                let payload = message.encode_payload().unwrap();
                Frame::new_v2(
                    message.message_type(),
                    frame.flags,
                    frame.sequence,
                    frame.session_id,
                    frame.timestamp_ticks,
                    payload,
                )
                .encode()
                .unwrap()
            }
        };
        assert_eq!(encoded, golden, "official encoder drifted for {name}");
    }
}

fn assert_common_key_fields(name: &str, message: &Message) {
    match (name, message) {
        ("hello_ack.bin", Message::HelloAck(value)) => {
            assert_eq!(value.tick_hz, 32_000_000);
            assert_eq!(value.channel_count, 6);
            assert_eq!(value.firmware_name, "hybrid30k-r2-golden");
        }
        ("channel_table.bin", Message::ChannelTable(value)) => {
            assert_eq!(value.revision, 1);
            assert_eq!(value.channels[0].name, "Ia");
            assert_eq!(value.channels[3].name, "Vdc");
            assert_eq!(value.channels[4].name, "SampleValid");
        }
        ("ping.bin", Message::Ping(value)) | ("pong.bin", Message::Pong(value)) => {
            assert_eq!(*value, 0x0102_0304_0506_0708);
        }
        _ => panic!("unexpected common fixture {name}"),
    }
}

fn assert_r2_key_fields(name: &str, message: &MessageV2R2) {
    match (name, message) {
        ("stream_table_r2.bin", MessageV2R2::StreamTable(value)) => {
            assert_eq!(value.revision, 3);
            assert_eq!(value.causal_groups[0].logical_cycle_rate_hz, 32_000);
            assert_eq!(value.streams[0].domain, SampleDomain::Control8k);
            assert_eq!(value.streams[0].sample_rate_hz, 8_000);
            assert_eq!(value.streams[0].logical_cycle_step, 4);
            assert_eq!(value.streams[1].domain, SampleDomain::Slow1k);
            assert_eq!(value.streams[1].logical_cycle_step, 32);
        }
        ("configure_streams_r2.bin", MessageV2R2::ConfigureStreams(value)) => {
            assert_eq!(value.transaction_id, 0x4833_304b);
            assert_eq!(value.subscriptions.len(), 2);
            assert_eq!(value.subscriptions[0].batch_samples, 8);
            assert_eq!(value.subscriptions[1].batch_samples, 1);
        }
        ("sample_batch_ctrl8k_affine.bin", MessageV2R2::StreamSampleBatch(value)) => {
            assert_eq!(value.domain, SampleDomain::Control8k);
            assert_eq!(value.logical_cycle_step, 4);
            assert_eq!(value.row_count, 8);
            assert_eq!(
                value.metadata_encoding,
                MetadataEncodingR2::AffineWithOverrides
            );
        }
        ("sample_batch_slow1k_affine.bin", MessageV2R2::StreamSampleBatch(value)) => {
            assert_eq!(value.domain, SampleDomain::Slow1k);
            assert_eq!(value.logical_cycle_step, 32);
            assert_eq!(value.row_count, 1);
            assert_eq!(
                value.metadata_encoding,
                MetadataEncodingR2::AffineWithOverrides
            );
        }
        _ => panic!("unexpected R2 fixture {name}"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|value| format!("{value:02x}")).collect()
}
