use std::{env, fs, path::Path, process::ExitCode, time::Duration};

use scope_analyzer::live::{
    hardware_smoke::{run, HardwareSmokeConfig, HardwareSmokeError, HardwareSmokeResult},
    transport::TransportConfig,
};
use serde::Serialize;

const SCHEMA_VERSION: u32 = 1;
const COMMAND: &str = "hardware-smoke";

#[derive(Serialize)]
struct SuccessEnvelope {
    schema_version: u32,
    command: &'static str,
    ok: bool,
    result: HardwareSmokeResult,
}

#[derive(Serialize)]
struct ErrorEnvelope {
    schema_version: u32,
    command: &'static str,
    ok: bool,
    error: ErrorPayload,
}

#[derive(Serialize)]
struct ErrorPayload {
    code: &'static str,
    message: String,
}

#[derive(Default)]
struct Options {
    serial_port: Option<String>,
    tcp_address: Option<String>,
    baud: u32,
    output: Option<String>,
    duration_ms: u64,
    sample_rate_hz: Option<u32>,
    batch_samples: Option<u16>,
    channel_count: usize,
}

fn main() -> ExitCode {
    match parse_options(env::args().skip(1).collect()) {
        Ok(Some(options)) => match execute(options) {
            Ok(result) => {
                println!(
                    "{}",
                    serde_json::to_string(&SuccessEnvelope {
                        schema_version: SCHEMA_VERSION,
                        command: COMMAND,
                        ok: true,
                        result,
                    })
                    .expect("hardware smoke result is serializable")
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("scope-hardware-smoke: {error}");
                println!(
                    "{}",
                    serde_json::to_string(&ErrorEnvelope {
                        schema_version: SCHEMA_VERSION,
                        command: COMMAND,
                        ok: false,
                        error: ErrorPayload {
                            code: error_code(&error),
                            message: error.to_string(),
                        },
                    })
                    .expect("hardware smoke error is serializable")
                );
                ExitCode::from(1)
            }
        },
        Ok(None) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("scope-hardware-smoke: {message}");
            print_usage();
            println!(
                "{}",
                serde_json::to_string(&ErrorEnvelope {
                    schema_version: SCHEMA_VERSION,
                    command: COMMAND,
                    ok: false,
                    error: ErrorPayload {
                        code: "usage_error",
                        message,
                    },
                })
                .expect("hardware smoke usage error is serializable")
            );
            ExitCode::from(2)
        }
    }
}

fn execute(options: Options) -> Result<HardwareSmokeResult, HardwareSmokeError> {
    let output = options
        .output
        .ok_or_else(|| HardwareSmokeError::InvalidConfig("--output is required".to_owned()))?;
    let output_path = Path::new(&output);
    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            HardwareSmokeError::InvalidConfig(format!(
                "cannot create output directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    let transport = match (options.serial_port, options.tcp_address) {
        (Some(port), None) => TransportConfig::Serial {
            port,
            baud: options.baud,
        },
        (None, Some(address)) => TransportConfig::Tcp { address },
        (Some(_), Some(_)) => {
            return Err(HardwareSmokeError::InvalidConfig(
                "choose exactly one of --serial-port or --tcp".to_owned(),
            ));
        }
        (None, None) => {
            return Err(HardwareSmokeError::InvalidConfig(
                "one of --serial-port or --tcp is required".to_owned(),
            ));
        }
    };
    run(&HardwareSmokeConfig {
        transport,
        output: output_path.to_path_buf(),
        duration: Duration::from_millis(options.duration_ms),
        sample_rate_hz: options.sample_rate_hz,
        batch_samples: options.batch_samples,
        channel_count: options.channel_count,
    })
}

fn parse_options(args: Vec<String>) -> Result<Option<Options>, String> {
    let mut options = Options {
        baud: 921_600,
        duration_ms: 3_000,
        channel_count: 1,
        ..Options::default()
    };
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--help" | "-h" => {
                print_usage();
                return Ok(None);
            }
            "--serial-port" => options.serial_port = Some(next_value(&mut args, &argument)?),
            "--tcp" => options.tcp_address = Some(next_value(&mut args, &argument)?),
            "--baud" => options.baud = parse_value(&mut args, &argument)?,
            "--output" => options.output = Some(next_value(&mut args, &argument)?),
            "--duration-ms" => options.duration_ms = parse_value(&mut args, &argument)?,
            "--sample-rate" => options.sample_rate_hz = Some(parse_value(&mut args, &argument)?),
            "--batch-samples" => options.batch_samples = Some(parse_value(&mut args, &argument)?),
            "--channels" => options.channel_count = parse_value(&mut args, &argument)?,
            _ => return Err(format!("unknown argument {argument}")),
        }
    }
    Ok(Some(options))
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .filter(|value| !value.starts_with('-'))
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_value<T>(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    next_value(args, flag)?
        .parse::<T>()
        .map_err(|error| format!("{flag} has an invalid value: {error}"))
}

fn error_code(error: &HardwareSmokeError) -> &'static str {
    match error {
        HardwareSmokeError::InvalidConfig(_) => "invalid_config",
        HardwareSmokeError::Session(_) => "session_error",
        HardwareSmokeError::Recording(_) => "recording_error",
        HardwareSmokeError::Timeout(_) => "timeout",
        HardwareSmokeError::Device(_) => "device_error",
    }
}

fn print_usage() {
    eprintln!(
        "scope-hardware-smoke --serial-port <port> [--baud <baud>] --output <scope> [options]\n\
         scope-hardware-smoke --tcp <host:port> --output <scope> [options]\n\
         options: --duration-ms <ms> --sample-rate <hz> --batch-samples <count> --channels <count>"
    );
}
