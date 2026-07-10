use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use thiserror::Error;

use super::protocol::{
    ChannelDescriptor, ChannelKind, ChannelTable, CommandResult, Configure, DeviceState, Frame,
    FrameDecoder, HelloAck, Message, ResultCode, SampleBatch, Status, WireFormat, MAX_PAYLOAD_LEN,
    MSG_CONFIGURE, MSG_HELLO, MSG_PING, MSG_START, MSG_STOP,
};

const SIMULATOR_TICK_HZ: u64 = 1_000_000;
const SIMULATOR_SESSION_ID: u32 = 1;

#[derive(Clone, Debug)]
pub struct SimulatorConfig {
    pub listen: SocketAddr,
    pub sample_rate_hz: u32,
    pub batch_samples: u16,
    pub accelerated: bool,
    pub seed: u64,
    pub drop_every: Option<u64>,
    pub corrupt_every: Option<u64>,
    pub disconnect_after: Option<u64>,
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:19090".parse().expect("valid default address"),
            sample_rate_hz: 10_000,
            batch_samples: 100,
            accelerated: false,
            seed: 1,
            drop_every: None,
            corrupt_every: None,
            disconnect_after: None,
        }
    }
}

impl SimulatorConfig {
    pub fn validate(&self) -> Result<(), SimulatorError> {
        if self.sample_rate_hz == 0 || self.sample_rate_hz as u64 > SIMULATOR_TICK_HZ {
            return Err(SimulatorError::InvalidConfig(format!(
                "sample rate must be within 1..={SIMULATOR_TICK_HZ}"
            )));
        }
        if self.batch_samples == 0 || self.batch_samples > 4096 {
            return Err(SimulatorError::InvalidConfig(
                "batch samples must be within 1..=4096".to_owned(),
            ));
        }
        for (label, value) in [
            ("drop_every", self.drop_every),
            ("corrupt_every", self.corrupt_every),
            ("disconnect_after", self.disconnect_after),
        ] {
            if value == Some(0) {
                return Err(SimulatorError::InvalidConfig(format!(
                    "{label} must be greater than zero"
                )));
            }
        }
        Ok(())
    }
}

pub struct SimulatorHandle {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SimulatorHandle {
    pub fn spawn(config: SimulatorConfig) -> Result<Self, SimulatorError> {
        config.validate()?;
        let listener = TcpListener::bind(config.listen)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::Builder::new()
            .name("scope-dsp-simulator".to_owned())
            .spawn(move || run_listener(listener, config, worker_stop))?;
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn stop(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for SimulatorHandle {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

#[derive(Debug, Error)]
pub enum SimulatorError {
    #[error("invalid simulator configuration: {0}")]
    InvalidConfig(String),
    #[error("simulator I/O error: {0}")]
    Io(#[from] std::io::Error),
}

fn run_listener(listener: TcpListener, config: SimulatorConfig, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let _ = serve_client(stream, &config, &stop);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }
}

fn serve_client(
    mut stream: TcpStream,
    config: &SimulatorConfig,
    stop: &AtomicBool,
) -> Result<(), SimulatorError> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_millis(5)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    let mut decoder = FrameDecoder::default();
    let mut read_buffer = [0_u8; 16 * 1024];
    let mut out_sequence = 1_u32;
    let mut configured = Configure {
        sample_rate_hz: config.sample_rate_hz,
        batch_samples: config.batch_samples,
        channel_mask: 0b1111,
    };
    let mut state = DeviceState::Idle;
    let mut first_sample_index = 0_u64;
    let mut emitted_batches = 0_u64;
    let table = simulator_channel_table();

    while !stop.load(Ordering::Relaxed) {
        match stream.read(&mut read_buffer) {
            Ok(0) => break,
            Ok(count) => {
                decoder.push(&read_buffer[..count]);
                for frame in decoder.drain_frames() {
                    let message = match Message::decode(frame.message_type, &frame.payload) {
                        Ok(message) => message,
                        Err(_) => continue,
                    };
                    match message {
                        Message::Hello(_) if frame.message_type == MSG_HELLO => {
                            send_message(
                                &mut stream,
                                &mut out_sequence,
                                Message::HelloAck(HelloAck {
                                    device_capabilities: 0,
                                    max_payload: MAX_PAYLOAD_LEN as u32,
                                    tick_hz: SIMULATOR_TICK_HZ,
                                    channel_count: table.channels.len() as u16,
                                    max_batch_samples: 4096,
                                    device_id: *b"SCOPE-SIM-V1----",
                                    firmware_name: "scope-dsp-simulator".to_owned(),
                                }),
                                0,
                            )?;
                            send_message(
                                &mut stream,
                                &mut out_sequence,
                                Message::ChannelTable(table.clone()),
                                0,
                            )?;
                        }
                        Message::Configure(request) if frame.message_type == MSG_CONFIGURE => {
                            configured = request;
                            state = DeviceState::Configured;
                            send_result(
                                &mut stream,
                                &mut out_sequence,
                                frame.sequence,
                                ResultCode::Ok,
                            )?;
                        }
                        Message::Start if frame.message_type == MSG_START => {
                            let result = if state == DeviceState::Configured {
                                state = DeviceState::Streaming;
                                ResultCode::Ok
                            } else {
                                ResultCode::InvalidState
                            };
                            send_result(&mut stream, &mut out_sequence, frame.sequence, result)?;
                        }
                        Message::Stop if frame.message_type == MSG_STOP => {
                            let result = if state == DeviceState::Streaming {
                                state = DeviceState::Configured;
                                ResultCode::Ok
                            } else {
                                ResultCode::InvalidState
                            };
                            send_result(&mut stream, &mut out_sequence, frame.sequence, result)?;
                        }
                        Message::Ping(nonce) if frame.message_type == MSG_PING => {
                            send_message(&mut stream, &mut out_sequence, Message::Pong(nonce), 0)?;
                        }
                        _ => {}
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }

        if state == DeviceState::Streaming {
            emitted_batches = emitted_batches.saturating_add(1);
            if config.disconnect_after == Some(emitted_batches) {
                break;
            }
            if !is_periodic_fault(config.drop_every, emitted_batches) {
                let frame = sample_frame(
                    out_sequence,
                    &configured,
                    first_sample_index,
                    &table,
                    config.seed,
                )?;
                out_sequence = out_sequence.wrapping_add(1);
                let mut bytes = frame
                    .encode()
                    .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?;
                if is_periodic_fault(config.corrupt_every, emitted_batches) && bytes.len() > 29 {
                    bytes[29] ^= 0x55;
                }
                stream.write_all(&bytes)?;
            } else {
                out_sequence = out_sequence.wrapping_add(1);
            }
            first_sample_index = first_sample_index
                .checked_add(u64::from(configured.batch_samples))
                .ok_or_else(|| SimulatorError::InvalidConfig("sample index overflow".to_owned()))?;
            if !config.accelerated {
                thread::sleep(Duration::from_secs_f64(
                    f64::from(configured.batch_samples) / f64::from(configured.sample_rate_hz),
                ));
            } else {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    Ok(())
}

fn simulator_channel_table() -> ChannelTable {
    ChannelTable {
        revision: 1,
        channels: vec![
            descriptor(0, "Sine A", "V", WireFormat::I16, 0.001),
            descriptor(1, "Sine B", "V", WireFormat::I16, 0.001),
            descriptor(2, "Saw", "V", WireFormat::I16, 0.001),
            ChannelDescriptor {
                channel_id: 3,
                kind: ChannelKind::Digital,
                wire_format: WireFormat::U8,
                scale: 1.0,
                offset: 0.0,
                unit: String::new(),
                name: "Digital".to_owned(),
            },
        ],
    }
}

fn descriptor(
    channel_id: u16,
    name: &str,
    unit: &str,
    wire_format: WireFormat,
    scale: f32,
) -> ChannelDescriptor {
    ChannelDescriptor {
        channel_id,
        kind: ChannelKind::Analog,
        wire_format,
        scale,
        offset: 0.0,
        unit: unit.to_owned(),
        name: name.to_owned(),
    }
}

fn sample_frame(
    sequence: u32,
    configure: &Configure,
    first_sample_index: u64,
    table: &ChannelTable,
    seed: u64,
) -> Result<Frame, SimulatorError> {
    let channel_ids = table
        .channels
        .iter()
        .filter(|channel| configure.channel_mask & (1_u64 << channel.channel_id) != 0)
        .map(|channel| channel.channel_id)
        .collect::<Vec<_>>();
    if channel_ids.is_empty() {
        return Err(SimulatorError::InvalidConfig(
            "configured channel mask is empty".to_owned(),
        ));
    }
    let sample_period_ticks =
        u32::try_from(SIMULATOR_TICK_HZ / u64::from(configure.sample_rate_hz)).map_err(|_| {
            SimulatorError::InvalidConfig("sample period does not fit u32".to_owned())
        })?;
    if sample_period_ticks == 0 {
        return Err(SimulatorError::InvalidConfig(
            "sample period rounded to zero".to_owned(),
        ));
    }
    let mut sample_data = Vec::new();
    for offset in 0..configure.batch_samples {
        let index = first_sample_index + u64::from(offset);
        let seeded_index = index.wrapping_add(seed);
        let phase = std::f64::consts::TAU * 50.0 * seeded_index as f64
            / f64::from(configure.sample_rate_hz);
        for channel_id in &channel_ids {
            match channel_id {
                0 => {
                    sample_data.extend_from_slice(&((phase.sin() * 10_000.0) as i16).to_le_bytes())
                }
                1 => sample_data.extend_from_slice(
                    &(((phase - std::f64::consts::TAU / 3.0).sin() * 10_000.0) as i16)
                        .to_le_bytes(),
                ),
                2 => {
                    let saw = ((index % 200) as i16 - 100) * 100;
                    sample_data.extend_from_slice(&saw.to_le_bytes());
                }
                3 => sample_data.push(u8::from((index / 50).is_multiple_of(2))),
                _ => {
                    return Err(SimulatorError::InvalidConfig(format!(
                        "unknown simulator channel {channel_id}"
                    )))
                }
            }
        }
    }
    let timestamp_ticks = first_sample_index
        .checked_mul(u64::from(sample_period_ticks))
        .ok_or_else(|| SimulatorError::InvalidConfig("timestamp overflow".to_owned()))?;
    let message = Message::SampleBatch(SampleBatch {
        channel_table_revision: table.revision,
        first_sample_index,
        sample_period_ticks,
        sample_count: configure.batch_samples,
        channel_ids,
        sample_data,
    });
    let payload = message
        .encode_payload()
        .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?;
    Ok(Frame::new(
        message.message_type(),
        0,
        sequence,
        SIMULATOR_SESSION_ID,
        timestamp_ticks,
        payload,
    ))
}

fn send_result(
    stream: &mut TcpStream,
    sequence: &mut u32,
    request_sequence: u32,
    result_code: ResultCode,
) -> Result<(), SimulatorError> {
    send_message(
        stream,
        sequence,
        Message::CommandResult(CommandResult {
            request_sequence,
            result_code,
            detail: if result_code == ResultCode::Ok {
                "ok".to_owned()
            } else {
                "invalid state".to_owned()
            },
        }),
        0,
    )
}

fn send_message(
    stream: &mut TcpStream,
    sequence: &mut u32,
    message: Message,
    timestamp_ticks: u64,
) -> Result<(), SimulatorError> {
    let frame = Frame::new(
        message.message_type(),
        0,
        *sequence,
        SIMULATOR_SESSION_ID,
        timestamp_ticks,
        message
            .encode_payload()
            .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?,
    );
    *sequence = sequence.wrapping_add(1);
    stream.write_all(
        &frame
            .encode()
            .map_err(|error| SimulatorError::InvalidConfig(error.to_string()))?,
    )?;
    Ok(())
}

fn is_periodic_fault(period: Option<u64>, count: u64) -> bool {
    period.is_some_and(|period| count.is_multiple_of(period))
}

#[allow(dead_code)]
fn status_message(state: DeviceState, produced_samples: u64) -> Message {
    Message::Status(Status {
        state,
        produced_samples,
        dropped_samples: 0,
        tx_overruns: 0,
    })
}
