use std::{process::ExitCode, thread, time::Duration};

use scope_analyzer::live::simulator::{SimulatorConfig, SimulatorHandle};

fn main() -> ExitCode {
    let config = match parse_args(std::env::args().skip(1)) {
        Ok(Some(config)) => config,
        Ok(None) => {
            print_help();
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("scope_dsp_simulator: {error}");
            eprintln!("Use --help for usage.");
            return ExitCode::from(2);
        }
    };
    match SimulatorHandle::spawn(config) {
        Ok(simulator) => {
            println!("SCP1 DSP simulator listening on {}", simulator.address());
            loop {
                thread::sleep(Duration::from_secs(60));
            }
        }
        Err(error) => {
            eprintln!("failed to start simulator: {error}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<SimulatorConfig>, String> {
    let mut config = SimulatorConfig::default();
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "-h" | "--help" => return Ok(None),
            "--accelerated" => config.accelerated = true,
            "--listen" => config.listen = parse_value(&mut args, "--listen")?,
            "--sample-rate" => config.sample_rate_hz = parse_value(&mut args, "--sample-rate")?,
            "--batch-samples" => config.batch_samples = parse_value(&mut args, "--batch-samples")?,
            "--seed" => config.seed = parse_value(&mut args, "--seed")?,
            "--drop-every" => config.drop_every = Some(parse_value(&mut args, "--drop-every")?),
            "--corrupt-every" => {
                config.corrupt_every = Some(parse_value(&mut args, "--corrupt-every")?)
            }
            "--disconnect-after" => {
                config.disconnect_after = Some(parse_value(&mut args, "--disconnect-after")?)
            }
            _ => return Err(format!("unknown argument {argument}")),
        }
    }
    config.validate().map_err(|error| error.to_string())?;
    Ok(Some(config))
}

fn parse_value<T: std::str::FromStr>(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<T, String> {
    let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
    value
        .parse()
        .map_err(|_| format!("invalid value for {flag}: {value}"))
}

fn print_help() {
    println!(
        "SCP1 DSP simulator\n\
         Usage: scope_dsp_simulator [options]\n\
         --listen <host:port>       Listen address (default 127.0.0.1:19090)\n\
         --sample-rate <hz>         Default sample rate (default 10000)\n\
         --batch-samples <count>    Samples per frame (default 100)\n\
         --accelerated              Run faster than wall clock\n\
         --seed <integer>           Deterministic signal seed\n\
         --drop-every <n>           Drop every nth sample frame\n\
         --corrupt-every <n>        Corrupt every nth sample frame\n\
         --disconnect-after <n>     Disconnect after n sample frames"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simulator_cli_options() {
        let config = parse_args([
            "--listen".to_owned(),
            "127.0.0.1:0".to_owned(),
            "--sample-rate".to_owned(),
            "20000".to_owned(),
            "--batch-samples".to_owned(),
            "50".to_owned(),
            "--accelerated".to_owned(),
        ])
        .unwrap()
        .unwrap();

        assert_eq!(
            config.listen,
            "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap()
        );
        assert_eq!(config.sample_rate_hz, 20_000);
        assert_eq!(config.batch_samples, 50);
        assert!(config.accelerated);
    }
}
