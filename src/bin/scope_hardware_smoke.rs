use std::{env, fs, path::Path, process::ExitCode, time::Duration};

use scope_analyzer::live::{
    hardware_smoke::{
        run, run_v2_r2, ChannelSet, HardwareSmokeConfig, HardwareSmokeError, HardwareSmokeMode,
        HardwareSmokeResult, HardwareSmokeV2R2Config, HardwareSmokeV2R2Result,
    },
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
    result: CommandResult,
}

#[derive(Serialize)]
#[serde(untagged)]
enum CommandResult {
    V1(HardwareSmokeResult),
    V2R2(Box<HardwareSmokeV2R2Result>),
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
    protocol: String,
    profile: Option<String>,
    mode: String,
    channel_set: String,
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

fn execute(options: Options) -> Result<CommandResult, HardwareSmokeError> {
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
    match options.protocol.as_str() {
        "v1" => run(&HardwareSmokeConfig {
            transport,
            output: output_path.to_path_buf(),
            duration: Duration::from_millis(options.duration_ms),
            sample_rate_hz: options.sample_rate_hz,
            batch_samples: options.batch_samples,
            channel_count: options.channel_count,
        })
        .map(CommandResult::V1),
        "v2-r2" => {
            let mode = HardwareSmokeMode::parse(&options.mode).ok_or_else(|| {
                HardwareSmokeError::InvalidConfig(format!(
                    "unsupported --mode {}; expected handshake|ctrl8k|multistream|link-stress",
                    options.mode
                ))
            })?;
            let channel_set = ChannelSet::parse(&options.channel_set).ok_or_else(|| {
                HardwareSmokeError::InvalidConfig(format!(
                    "unsupported --channel-set {}; expected required|all",
                    options.channel_set
                ))
            })?;
            let result = run_v2_r2(&HardwareSmokeV2R2Config {
                transport,
                profile: options.profile.ok_or_else(|| {
                    HardwareSmokeError::InvalidConfig(
                        "--profile is required with --protocol v2-r2".to_owned(),
                    )
                })?,
                mode,
                channel_set,
                duration: Duration::from_millis(options.duration_ms),
                batch_samples: options.batch_samples.unwrap_or(16),
            })?;
            fs::write(
                output_path,
                serde_json::to_vec_pretty(&result).expect("R2 result is serializable"),
            )
            .map_err(|error| {
                HardwareSmokeError::InvalidConfig(format!(
                    "cannot write output JSON {}: {error}",
                    output_path.display()
                ))
            })?;
            Ok(CommandResult::V2R2(Box::new(result)))
        }
        other => Err(HardwareSmokeError::InvalidConfig(format!(
            "unsupported --protocol {other}; expected v1|v2-r2"
        ))),
    }
}

fn parse_options(args: Vec<String>) -> Result<Option<Options>, String> {
    let mut options = Options {
        protocol: "v1".to_owned(),
        mode: "handshake".to_owned(),
        channel_set: "required".to_owned(),
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
            "--protocol" => options.protocol = next_value(&mut args, &argument)?,
            "--profile" => options.profile = Some(next_value(&mut args, &argument)?),
            "--mode" => options.mode = next_value(&mut args, &argument)?,
            "--channel-set" => options.channel_set = next_value(&mut args, &argument)?,
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
    error.code()
}

fn print_usage() {
    eprintln!(
        "scope-hardware-smoke [--protocol v1] --serial-port <port> [--baud <baud>] --output <scope> [options]\n\
         scope-hardware-smoke [--protocol v1] --tcp <host:port> --output <scope> [options]\n\
         scope-hardware-smoke --protocol v2-r2 --profile <name> --mode <mode> (--serial-port <port>|--tcp <host:port>) --output <json> [options]\n\
         modes: handshake | ctrl8k | multistream | link-stress\n\
         options: --channel-set required|all --duration-ms <ms> --sample-rate <hz> --batch-samples <count> --channels <count>"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_preserve_v1_hardware_smoke_behavior() {
        let options = parse_options(vec![
            "--tcp".to_owned(),
            "127.0.0.1:1".to_owned(),
            "--output".to_owned(),
            "capture.scope".to_owned(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(options.protocol, "v1");
        assert_eq!(options.baud, 921_600);
        assert_eq!(options.duration_ms, 3_000);
        assert_eq!(options.channel_count, 1);
        assert_eq!(options.channel_set, "required");
    }

    #[test]
    fn parses_v2_r2_round_one_modes_and_profile() {
        for mode in ["handshake", "ctrl8k", "multistream", "link-stress"] {
            let options = parse_options(vec![
                "--protocol".to_owned(),
                "v2-r2".to_owned(),
                "--profile".to_owned(),
                "hybrid30k".to_owned(),
                "--mode".to_owned(),
                mode.to_owned(),
            ])
            .unwrap()
            .unwrap();
            assert_eq!(options.protocol, "v2-r2");
            assert_eq!(options.profile.as_deref(), Some("hybrid30k"));
            assert_eq!(options.mode, mode);
            assert_eq!(options.channel_set, "required");
        }
    }

    #[test]
    fn parses_explicit_all_channel_set() {
        let options = parse_options(vec!["--channel-set".to_owned(), "all".to_owned()])
            .unwrap()
            .unwrap();
        assert_eq!(options.channel_set, "all");
    }
}
