use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::Serialize;

use crate::{
    data::{CloudCsvDataSource, CsvDataSource, DatDataSource, DataSource},
    fft,
};

const DEFAULT_SAMPLE_RATE_HZ: f64 = 1000.0;
const DEFAULT_HARMONIC_BASE_HZ: f64 = 50.0;
const DEFAULT_MAX_POINTS: usize = 250_000;
const MAX_FFT_POINTS: usize = 262_144;
pub const BRIDGE_PROTOCOL_VERSION: u32 = 1;

pub fn run_from_args(args: impl IntoIterator<Item = OsString>) -> Option<i32> {
    let mut args = args.into_iter();
    let command = args.next()?;
    let command = command.to_string_lossy();
    if !command.starts_with("--vscode-") {
        return None;
    }

    let result = match command.as_ref() {
        "--vscode-capabilities" => run_capabilities(args.collect()),
        "--vscode-dataset" => run_dataset(args.collect()),
        "--vscode-fft" => run_fft(args.collect()),
        _ => Err(format!("Unknown VS Code bridge command: {command}")),
    };

    match result {
        Ok(()) => Some(0),
        Err(error) => {
            eprintln!("{error}");
            Some(1)
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeCapabilities {
    protocol_version: u32,
    application_version: &'static str,
    commands: &'static [&'static str],
}

fn run_capabilities(args: Vec<OsString>) -> Result<(), String> {
    if !args.is_empty() {
        return Err("--vscode-capabilities does not accept options".to_owned());
    }
    print_json(&BridgeCapabilities {
        protocol_version: BRIDGE_PROTOCOL_VERSION,
        application_version: env!("CARGO_PKG_VERSION"),
        commands: &["dataset", "fft"],
    })
}

#[derive(Clone, Copy)]
enum SourceFormat {
    StandardCsv,
    CloudContent,
    BinaryDat,
}

impl SourceFormat {
    fn label(self) -> &'static str {
        match self {
            Self::StandardCsv => "standard",
            Self::CloudContent => "cloud-content",
            Self::BinaryDat => "binary-dat",
        }
    }
}

struct OpenedSource {
    source: Arc<dyn DataSource>,
    format: SourceFormat,
}

#[derive(Default)]
struct DatasetOptions {
    path: Option<PathBuf>,
    sample_rate_hz: f64,
    max_points: usize,
}

#[derive(Default)]
struct FftOptions {
    path: Option<PathBuf>,
    sample_rate_hz: f64,
    harmonic_base_hz: f64,
    channel: usize,
    start: Option<f64>,
    end: Option<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeDataset {
    name: String,
    path: String,
    format: String,
    sample_rate_hz: f64,
    sample_count: usize,
    duration: f64,
    skipped_rows: u64,
    truncated: bool,
    times: Vec<f64>,
    channels: Vec<Vec<Option<f32>>>,
    channel_summaries: Vec<BridgeChannelSummary>,
}

#[derive(Serialize)]
struct BridgeChannelSummary {
    index: usize,
    name: String,
    visible: bool,
    min: f32,
    max: f32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeFftResult {
    sample_count: usize,
    thd_percent: f32,
    harmonics: Vec<BridgeHarmonicRow>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeHarmonicRow {
    order: usize,
    amplitude: f32,
    phase_deg: Option<f32>,
    relative_percent: f32,
}

fn run_dataset(args: Vec<OsString>) -> Result<(), String> {
    let options = parse_dataset_options(args)?;
    let path = options
        .path
        .ok_or_else(|| "--vscode-dataset requires --path".to_owned())?;
    let opened = open_source(&path, options.sample_rate_hz)?;
    let meta = opened.source.metadata();
    let channels = (0..meta.channels.len()).collect::<Vec<_>>();
    let block = opened
        .source
        .read_range(
            meta.start_time,
            meta.end_time,
            &channels,
            options.max_points,
        )
        .map_err(|error| error.to_string())?;
    let loaded_count = block.times.len();
    let truncated = meta.sample_count > loaded_count as u64;
    let channels = block
        .channels
        .iter()
        .map(|values| {
            values
                .iter()
                .map(|value| value.is_finite().then_some(*value))
                .collect()
        })
        .collect::<Vec<Vec<Option<f32>>>>();
    let channel_summaries = meta
        .channels
        .iter()
        .enumerate()
        .map(|(index, channel)| {
            let mut min = f32::INFINITY;
            let mut max = f32::NEG_INFINITY;
            if let Some(values) = block.channels.get(index) {
                for value in values.iter().copied().filter(|value| value.is_finite()) {
                    min = min.min(value);
                    max = max.max(value);
                }
            }
            BridgeChannelSummary {
                index,
                name: channel.name.clone(),
                visible: channel.default_visible,
                min: if min.is_finite() { min } else { 0.0 },
                max: if max.is_finite() { max } else { 0.0 },
            }
        })
        .collect();

    let dataset = BridgeDataset {
        name: meta.source_name.clone(),
        path: path.to_string_lossy().into_owned(),
        format: opened.format.label().to_owned(),
        sample_rate_hz: meta.nominal_sample_rate_hz,
        sample_count: loaded_count,
        duration: block
            .times
            .last()
            .zip(block.times.first())
            .map(|(last, first)| (last - first).max(0.0))
            .unwrap_or_default(),
        skipped_rows: 0,
        truncated,
        times: block.times,
        channels,
        channel_summaries,
    };

    print_json(&dataset)
}

fn run_fft(args: Vec<OsString>) -> Result<(), String> {
    let options = parse_fft_options(args)?;
    let path = options
        .path
        .ok_or_else(|| "--vscode-fft requires --path".to_owned())?;
    let opened = open_source(&path, options.sample_rate_hz)?;
    let meta = opened.source.metadata();
    if options.channel >= meta.channels.len() {
        return Err(format!(
            "Channel {} is out of range for {} channel(s)",
            options.channel,
            meta.channels.len()
        ));
    }
    let start = options
        .start
        .unwrap_or(meta.start_time)
        .max(meta.start_time);
    let end = options.end.unwrap_or(meta.end_time).min(meta.end_time);
    if end <= start {
        return Err("FFT range is empty.".to_owned());
    }

    let block = opened
        .source
        .read_range(start, end, &[options.channel], MAX_FFT_POINTS)
        .map_err(|error| error.to_string())?;
    let samples = block.channels.first().map(Vec::as_slice).unwrap_or(&[]);
    let result = fft::analyze(
        meta.channels[options.channel].name.clone(),
        samples,
        meta.nominal_sample_rate_hz.max(1.0),
        options.harmonic_base_hz.max(0.001),
        10,
    )
    .ok_or_else(|| "FFT needs at least 16 finite samples.".to_owned())?;

    let result = BridgeFftResult {
        sample_count: result.sample_count,
        thd_percent: result.thd_percent,
        harmonics: result
            .harmonics
            .into_iter()
            .map(|row| BridgeHarmonicRow {
                order: row.order,
                amplitude: row.amplitude,
                phase_deg: row.phase_deg.is_finite().then_some(row.phase_deg),
                relative_percent: row.relative_percent,
            })
            .collect(),
    };
    print_json(&result)
}

fn open_source(path: &Path, sample_rate_hz: f64) -> Result<OpenedSource, String> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension == "dat" {
        let source = DatDataSource::open(path).map_err(|error| error.to_string())?;
        return Ok(OpenedSource {
            source: Arc::new(source),
            format: SourceFormat::BinaryDat,
        });
    }
    if extension != "csv" {
        return Err(format!(
            "Unsupported waveform file extension: {}",
            if extension.is_empty() {
                "(none)"
            } else {
                extension.as_str()
            }
        ));
    }

    match CloudCsvDataSource::open_with_sample_rate(path, sample_rate_hz) {
        Ok(source) => Ok(OpenedSource {
            source: Arc::new(source),
            format: SourceFormat::CloudContent,
        }),
        Err(cloud_error) => CsvDataSource::open_with_sample_rate(path, sample_rate_hz)
            .map(|source| OpenedSource {
                source: Arc::new(source),
                format: SourceFormat::StandardCsv,
            })
            .map_err(|csv_error| format!("{csv_error}; cloud parser also failed: {cloud_error}")),
    }
}

fn parse_dataset_options(args: Vec<OsString>) -> Result<DatasetOptions, String> {
    let mut options = DatasetOptions {
        sample_rate_hz: DEFAULT_SAMPLE_RATE_HZ,
        max_points: DEFAULT_MAX_POINTS,
        ..Default::default()
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_string_lossy().as_ref() {
            "--path" => options.path = Some(PathBuf::from(next_value(&mut args, "--path")?)),
            "--sample-rate" => {
                options.sample_rate_hz = parse_f64(next_value(&mut args, "--sample-rate")?)?;
            }
            "--max-points" => {
                options.max_points = parse_usize(next_value(&mut args, "--max-points")?)?;
            }
            flag => return Err(format!("Unknown --vscode-dataset option: {flag}")),
        }
    }
    options.max_points = options.max_points.max(1);
    options.sample_rate_hz = options.sample_rate_hz.max(1.0);
    Ok(options)
}

fn parse_fft_options(args: Vec<OsString>) -> Result<FftOptions, String> {
    let mut options = FftOptions {
        sample_rate_hz: DEFAULT_SAMPLE_RATE_HZ,
        harmonic_base_hz: DEFAULT_HARMONIC_BASE_HZ,
        ..Default::default()
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_string_lossy().as_ref() {
            "--path" => options.path = Some(PathBuf::from(next_value(&mut args, "--path")?)),
            "--sample-rate" => {
                options.sample_rate_hz = parse_f64(next_value(&mut args, "--sample-rate")?)?;
            }
            "--base" => {
                options.harmonic_base_hz = parse_f64(next_value(&mut args, "--base")?)?;
            }
            "--channel" => options.channel = parse_usize(next_value(&mut args, "--channel")?)?,
            "--start" => options.start = Some(parse_f64(next_value(&mut args, "--start")?)?),
            "--end" => options.end = Some(parse_f64(next_value(&mut args, "--end")?)?),
            flag => return Err(format!("Unknown --vscode-fft option: {flag}")),
        }
    }
    options.sample_rate_hz = options.sample_rate_hz.max(1.0);
    options.harmonic_base_hz = options.harmonic_base_hz.max(0.001);
    Ok(options)
}

fn next_value(args: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<OsString, String> {
    args.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_f64(value: OsString) -> Result<f64, String> {
    value
        .to_string_lossy()
        .parse::<f64>()
        .map_err(|_| format!("Invalid number: {}", value.to_string_lossy()))
        .and_then(|value| {
            value
                .is_finite()
                .then_some(value)
                .ok_or_else(|| "Number must be finite".to_owned())
        })
}

fn parse_usize(value: OsString) -> Result<usize, String> {
    value
        .to_string_lossy()
        .parse::<usize>()
        .map_err(|_| format!("Invalid integer: {}", value.to_string_lossy()))
}

fn print_json(value: &impl Serialize) -> Result<(), String> {
    let text = serde_json::to_string(value).map_err(|error| error.to_string())?;
    println!("{text}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "external-input fuzz gate; run explicitly in the release job"]
    fn bridge_option_parsers_survive_one_million_inputs() {
        let mut state = 0x1319_8a2e_u32;
        for index in 0..1_000_000_usize {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let token = format!("arg-{}-{:08x}", index % 17, state);
            let args = vec![OsString::from(token)];
            let _ = parse_dataset_options(args.clone());
            let _ = parse_fft_options(args);
        }
    }
}
