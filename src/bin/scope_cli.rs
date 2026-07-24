use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::ExitCode,
    time::{Duration, Instant},
};

use scope_analyzer::{
    compare::{compare, AlignmentSpec, CompareRequest, Series, SeriesSegment, Tolerance},
    data::{CsvDataSource, DataSource},
    live::{
        protocol_v2::{
            ArmCapture, CaptureEdge, CaptureTrigger, CaptureTriggerKind, ConfigureStream,
            ManualTrigger,
        },
        session::{LiveSession, SessionEvent},
        transport::TransportConfig,
    },
    measurements::{analyze_segments, ChannelMeasurementSpec},
    rules::{evaluate, evaluate_series, MetricSample, RuleEvaluation, RuleSpec},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

const CLI_SCHEMA_VERSION: u32 = 1;
const MAX_CLI_SAMPLES: u64 = 10_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Inspect,
    Analyze,
    ValidateRecording,
    Project,
    Compare,
    Test,
    Report,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AlignmentMode {
    Manual,
    Anchor,
    Trigger,
    Threshold,
    Phase,
}

#[derive(Debug, PartialEq)]
struct CliArgs {
    command: Command,
    input: String,
    channel: usize,
    sample_rate: f64,
    migrate_output: Option<String>,
    reference: String,
    test: String,
    reference_channel: usize,
    test_channel: usize,
    offset: f64,
    alignment: AlignmentMode,
    reference_time: Option<f64>,
    test_time: Option<f64>,
    confidence: Option<f64>,
    reference_phase: Option<f64>,
    test_phase: Option<f64>,
    period: Option<f64>,
    absolute_tolerance: Option<f64>,
    relative_tolerance: Option<f64>,
    relative_floor: f64,
    metrics: String,
    rules: String,
    report_compare: String,
    report_output: Option<String>,
    report_evidence_output: Option<String>,
    report_sources: Vec<String>,
}

#[derive(Debug, Error)]
enum CliError {
    #[error("usage error: {0}")]
    Usage(String),
    #[error("input error: {0}")]
    Input(String),
    #[error("compare error: {0}")]
    Compare(String),
    #[error("rule error: {0}")]
    Rule(String),
    #[error("analysis error: {0}")]
    Analysis(String),
    #[error("project error: {0}")]
    Project(String),
}

#[derive(Serialize)]
struct SuccessEnvelope<'a, T: Serialize> {
    schema_version: u32,
    command: &'a str,
    ok: bool,
    result: T,
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    schema_version: u32,
    command: &'a str,
    ok: bool,
    error: CliErrorPayload,
}

#[derive(Serialize)]
struct CliErrorPayload {
    code: &'static str,
    message: String,
}

fn main() -> ExitCode {
    let raw_args = std::env::args().skip(1).collect::<Vec<_>>();
    if matches!(
        raw_args.first().map(String::as_str),
        Some("live-inspect" | "capture-inspect")
    ) {
        return run_v2_diagnostic_command(raw_args);
    }
    let command_hint = command_hint(raw_args.first().map(String::as_str));
    let args = match parse_args(raw_args) {
        Ok(args) => args,
        Err(CliError::Usage(message)) => {
            eprintln!("scope-cli: {message}");
            print_help();
            print_error_envelope(command_hint, "usage_error", CliError::Usage(message));
            return ExitCode::from(2);
        }
        Err(error) => {
            eprintln!("scope-cli: {error}");
            let (code, exit_code) = error_code_and_exit(&error);
            print_error_envelope(command_hint, code, error);
            return ExitCode::from(exit_code);
        }
    };

    let result = match args.command {
        Command::Inspect => run_inspect(&args).map(|result| {
            (
                "inspect",
                serde_json::to_value(result).expect("inspect result is serializable"),
            )
        }),
        Command::Analyze => run_analyze(&args).map(|result| {
            (
                "analyze",
                serde_json::to_value(result).expect("analysis result is serializable"),
            )
        }),
        Command::ValidateRecording => run_validate_recording(&args).map(|result| {
            (
                "validate-recording",
                serde_json::to_value(result).expect("recording result is serializable"),
            )
        }),
        Command::Project => run_project(&args).map(|result| {
            (
                "project",
                serde_json::to_value(result).expect("project result is serializable"),
            )
        }),
        Command::Compare => run_compare(&args).map(|result| {
            (
                "compare",
                serde_json::to_value(result).expect("compare result is serializable"),
            )
        }),
        Command::Test => run_rule_test(&args).map(|result| {
            (
                "test",
                serde_json::to_value(result).expect("rule result is serializable"),
            )
        }),
        Command::Report => run_report(&args).map(|result| {
            (
                "report",
                serde_json::to_value(result).expect("report result is serializable"),
            )
        }),
    };
    match result {
        Ok((command, result)) => {
            let envelope = SuccessEnvelope {
                schema_version: CLI_SCHEMA_VERSION,
                command,
                ok: true,
                result,
            };
            let exit_code = success_exit_code(command, &envelope.result);
            println!(
                "{}",
                serde_json::to_string(&envelope).expect("compare result is serializable")
            );
            ExitCode::from(exit_code)
        }
        Err(error) => {
            let (code, exit_code) = error_code_and_exit(&error);
            let envelope = ErrorEnvelope {
                schema_version: CLI_SCHEMA_VERSION,
                command: match args.command {
                    Command::Inspect => "inspect",
                    Command::Analyze => "analyze",
                    Command::ValidateRecording => "validate-recording",
                    Command::Project => "project",
                    Command::Compare => "compare",
                    Command::Test => "test",
                    Command::Report => "report",
                },
                ok: false,
                error: CliErrorPayload {
                    code,
                    message: error.to_string(),
                },
            };
            println!(
                "{}",
                serde_json::to_string(&envelope).expect("error result is serializable")
            );
            ExitCode::from(exit_code)
        }
    }
}

fn success_exit_code(command: &str, result: &Value) -> u8 {
    let failed_rules =
        command == "test" && result.get("passed").and_then(Value::as_bool) == Some(false);
    let invalid_recording = command == "validate-recording"
        && result.get("valid").and_then(Value::as_bool) == Some(false);
    if failed_rules || invalid_recording {
        5
    } else {
        0
    }
}

fn command_hint(command: Option<&str>) -> &'static str {
    match command {
        Some("inspect") => "inspect",
        Some("analyze") => "analyze",
        Some("validate-recording") => "validate-recording",
        Some("project") => "project",
        Some("compare") => "compare",
        Some("test") => "test",
        Some("report") => "report",
        _ => "unknown",
    }
}

fn error_code_and_exit(error: &CliError) -> (&'static str, u8) {
    match error {
        CliError::Usage(_) => ("usage_error", 2),
        CliError::Input(_) => ("input_error", 3),
        CliError::Compare(_) => ("compare_error", 4),
        CliError::Rule(_) => ("rule_error", 5),
        CliError::Analysis(_) => ("analysis_error", 6),
        CliError::Project(_) => ("project_error", 7),
    }
}

fn print_error_envelope(command: &'static str, code: &'static str, error: CliError) {
    let envelope = ErrorEnvelope {
        schema_version: CLI_SCHEMA_VERSION,
        command,
        ok: false,
        error: CliErrorPayload {
            code,
            message: error.to_string(),
        },
    };
    println!(
        "{}",
        serde_json::to_string(&envelope).expect("error result is serializable")
    );
}

#[derive(Serialize)]
struct V2DiagnosticReport {
    protocol_version: u8,
    stream_id: u16,
    domain: String,
    capture_phase: String,
    consistency_group: u16,
    row_count: u64,
    row_sequence_gaps: u64,
    row_sequence_reorders: u64,
    source_sequence_faults: u64,
    applied_sequence_faults: u64,
    invalid_snapshot_rows: u64,
    missing_causal_source: u64,
    causal_source_mismatch: u64,
    causal_application_mismatch: u64,
    causal_sequence_reorder: u64,
    causal_group_mismatch: u64,
    causal_cache_evictions: u64,
    capture_complete: bool,
    capture_missing_chunks: u32,
    capture_duplicate_chunks: u32,
    capture_reordered_chunks: u32,
}

fn run_v2_diagnostic_command(raw_args: Vec<String>) -> ExitCode {
    let command = raw_args.first().map(String::as_str).unwrap_or("unknown");
    let parsed = parse_v2_diagnostic_args(&raw_args[1..]);
    let result = parsed.and_then(|(address, stream_id, rows)| {
        if command == "live-inspect" {
            inspect_v2_live(&address, stream_id, rows)
        } else {
            inspect_v2_capture(&address, stream_id, rows)
        }
    });
    match result {
        Ok(report) => {
            let envelope = SuccessEnvelope {
                schema_version: CLI_SCHEMA_VERSION,
                command,
                ok: true,
                result: report,
            };
            println!(
                "{}",
                serde_json::to_string(&envelope).expect("diagnostic JSON")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("scope-cli: {error}");
            let (code, exit_code) = error_code_and_exit(&error);
            print_error_envelope(
                match command {
                    "live-inspect" => "live-inspect",
                    _ => "capture-inspect",
                },
                code,
                error,
            );
            ExitCode::from(exit_code)
        }
    }
}

fn parse_v2_diagnostic_args(args: &[String]) -> Result<(String, u16, u16), CliError> {
    let mut address = None;
    let mut stream_id = 1_u16;
    let mut rows = 16_u16;
    let mut values = args.iter();
    while let Some(argument) = values.next() {
        match argument.as_str() {
            "--address" => address = Some(next_v2_argument(&mut values, "--address")?),
            "--stream-id" => {
                stream_id = next_v2_argument(&mut values, "--stream-id")?
                    .parse()
                    .map_err(|_| CliError::Usage("--stream-id must be a u16".to_owned()))?
            }
            "--rows" => {
                rows = next_v2_argument(&mut values, "--rows")?
                    .parse()
                    .map_err(|_| CliError::Usage("--rows must be a u16".to_owned()))?;
                if rows == 0 {
                    return Err(CliError::Usage("--rows must be non-zero".to_owned()));
                }
            }
            value => {
                return Err(CliError::Usage(format!(
                    "unknown diagnostic argument {value}"
                )))
            }
        }
    }
    Ok((
        address.ok_or_else(|| {
            CliError::Usage("diagnostic command requires --address <host:port>".to_owned())
        })?,
        stream_id,
        rows,
    ))
}

fn next_v2_argument<'a>(
    values: &mut impl Iterator<Item = &'a String>,
    flag: &str,
) -> Result<String, CliError> {
    values
        .next()
        .cloned()
        .ok_or_else(|| CliError::Usage(format!("{flag} needs a value")))
}

fn inspect_v2_live(
    address: &str,
    stream_id: u16,
    rows: u16,
) -> Result<V2DiagnosticReport, CliError> {
    let session = LiveSession::connect_v2(TransportConfig::Tcp {
        address: address.to_owned(),
    })
    .map_err(|error| CliError::Input(error.to_string()))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut configured = false;
    let mut report = empty_v2_report(stream_id);
    while Instant::now() < deadline {
        let event = session
            .recv_timeout(Duration::from_millis(100))
            .map_err(|error| CliError::Input(error.to_string()))?;
        match event {
            SessionEvent::StreamTable(table) => {
                let descriptor = table.stream(stream_id).ok_or_else(|| {
                    CliError::Input(format!("stream {stream_id} is absent from STREAM_TABLE"))
                })?;
                report.domain = format!("{:?}", descriptor.domain);
                report.capture_phase = format!("{:?}", descriptor.capture_phase);
                report.consistency_group = descriptor.consistency_group;
                let mask = descriptor
                    .channel_ids
                    .iter()
                    .fold(0_u64, |mask, id| mask | (1_u64 << id));
                session
                    .configure_stream(ConfigureStream {
                        stream_id,
                        batch_samples: rows,
                        channel_mask: mask,
                    })
                    .map_err(|error| CliError::Input(error.to_string()))?;
                configured = true;
            }
            SessionEvent::ConfiguredV2(_) if configured => {
                session
                    .start()
                    .map_err(|error| CliError::Input(error.to_string()))?;
            }
            SessionEvent::SnapshotV2(batch, diagnostics) => {
                report.row_count = report
                    .row_count
                    .saturating_add(batch.row_metadata.len() as u64);
                copy_snapshot_diagnostics(&mut report, diagnostics);
                if v2_report_has_failure(&report) {
                    let _ = session.disconnect();
                    return Err(CliError::Input(
                        "V2 snapshot diagnostics reported a protocol or causal contract failure"
                            .to_owned(),
                    ));
                }
                if report.row_count >= u64::from(rows) {
                    let _ = session.disconnect();
                    return Ok(report);
                }
            }
            SessionEvent::Error(error) => return Err(CliError::Input(error)),
            _ => {}
        }
    }
    let _ = session.disconnect();
    Err(CliError::Input(
        "timed out waiting for a V2 sample batch".to_owned(),
    ))
}

fn inspect_v2_capture(
    address: &str,
    stream_id: u16,
    rows: u16,
) -> Result<V2DiagnosticReport, CliError> {
    let session = LiveSession::connect_v2(TransportConfig::Tcp {
        address: address.to_owned(),
    })
    .map_err(|error| CliError::Input(error.to_string()))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut armed = false;
    let mut report = empty_v2_report(stream_id);
    while Instant::now() < deadline {
        let event = session
            .recv_timeout(Duration::from_millis(100))
            .map_err(|error| CliError::Input(error.to_string()))?;
        match event {
            SessionEvent::StreamTable(table) => {
                let descriptor = table.stream(stream_id).ok_or_else(|| {
                    CliError::Input(format!("stream {stream_id} is absent from STREAM_TABLE"))
                })?;
                report.domain = format!("{:?}", descriptor.domain);
                report.capture_phase = format!("{:?}", descriptor.capture_phase);
                report.consistency_group = descriptor.consistency_group;
                let mask = descriptor
                    .channel_ids
                    .iter()
                    .fold(0_u64, |mask, id| mask | (1_u64 << id));
                session
                    .configure_stream(ConfigureStream {
                        stream_id,
                        batch_samples: rows,
                        channel_mask: mask,
                    })
                    .map_err(|error| CliError::Input(error.to_string()))?;
            }
            SessionEvent::ConfiguredV2(_) if !armed => {
                session
                    .arm_capture(ArmCapture {
                        capture_id: 1,
                        stream_id,
                        pretrigger_rows: 1,
                        posttrigger_rows: 1,
                        timeout_samples: u32::from(rows).saturating_mul(10),
                        trigger: CaptureTrigger {
                            kind: CaptureTriggerKind::Manual,
                            channel_id: 0,
                            level: 0.0,
                            edge: CaptureEdge::Rising,
                            hysteresis: 0.0,
                            flag_mask: 0,
                            flag_value: 0,
                        },
                    })
                    .map_err(|error| CliError::Input(error.to_string()))?;
                session
                    .manual_trigger(ManualTrigger { capture_id: 1 })
                    .map_err(|error| CliError::Input(error.to_string()))?;
                armed = true;
            }
            SessionEvent::CaptureComplete(capture) => {
                report.capture_complete = capture.diagnostics.capture_complete;
                report.capture_missing_chunks = capture.diagnostics.capture_missing_chunks;
                report.capture_duplicate_chunks = capture.diagnostics.capture_duplicate_chunks;
                report.capture_reordered_chunks = capture.diagnostics.capture_reordered_chunks;
                report.row_count = capture
                    .blocks
                    .iter()
                    .map(|batch| u64::from(batch.row_count))
                    .sum();
                let _ = session.disconnect();
                return Ok(report);
            }
            SessionEvent::CaptureFailure(error) => return Err(CliError::Input(error)),
            SessionEvent::CaptureStatus(status)
                if !matches!(
                    status.state,
                    scope_analyzer::live::protocol_v2::CaptureState::Armed
                        | scope_analyzer::live::protocol_v2::CaptureState::Triggered
                        | scope_analyzer::live::protocol_v2::CaptureState::PostCapture
                        | scope_analyzer::live::protocol_v2::CaptureState::Uploading
                ) =>
            {
                return Err(CliError::Input(format!(
                    "V2 capture ended in device state {:?}",
                    status.state
                )))
            }
            SessionEvent::Error(error) => return Err(CliError::Input(error)),
            _ => {}
        }
    }
    let _ = session.disconnect();
    Err(CliError::Input(
        "timed out waiting for a V2 hardware capture".to_owned(),
    ))
}

fn empty_v2_report(stream_id: u16) -> V2DiagnosticReport {
    V2DiagnosticReport {
        protocol_version: 2,
        stream_id,
        domain: String::new(),
        capture_phase: String::new(),
        consistency_group: 0,
        row_count: 0,
        row_sequence_gaps: 0,
        row_sequence_reorders: 0,
        source_sequence_faults: 0,
        applied_sequence_faults: 0,
        invalid_snapshot_rows: 0,
        missing_causal_source: 0,
        causal_source_mismatch: 0,
        causal_application_mismatch: 0,
        causal_sequence_reorder: 0,
        causal_group_mismatch: 0,
        causal_cache_evictions: 0,
        capture_complete: false,
        capture_missing_chunks: 0,
        capture_duplicate_chunks: 0,
        capture_reordered_chunks: 0,
    }
}

fn copy_snapshot_diagnostics(
    report: &mut V2DiagnosticReport,
    diagnostics: scope_analyzer::live::snapshot::SnapshotDiagnostics,
) {
    report.row_sequence_gaps = diagnostics.row_sequence_gaps;
    report.row_sequence_reorders = diagnostics.row_sequence_reorders;
    report.source_sequence_faults = diagnostics.source_sequence_faults;
    report.applied_sequence_faults = diagnostics.applied_sequence_faults;
    report.invalid_snapshot_rows = diagnostics.invalid_snapshot_rows;
    report.missing_causal_source = diagnostics.missing_causal_source;
    report.causal_source_mismatch = diagnostics.causal_source_mismatch;
    report.causal_application_mismatch = diagnostics.causal_application_mismatch;
    report.causal_sequence_reorder = diagnostics.causal_sequence_reorder;
    report.causal_group_mismatch = diagnostics.causal_group_mismatch;
    report.causal_cache_evictions = diagnostics.causal_cache_evictions;
}

fn v2_report_has_failure(report: &V2DiagnosticReport) -> bool {
    report.row_sequence_gaps != 0
        || report.row_sequence_reorders != 0
        || report.source_sequence_faults != 0
        || report.applied_sequence_faults != 0
        || report.invalid_snapshot_rows != 0
        || report.missing_causal_source != 0
        || report.causal_source_mismatch != 0
        || report.causal_application_mismatch != 0
        || report.causal_sequence_reorder != 0
        || report.causal_group_mismatch != 0
        || report.causal_cache_evictions != 0
}

fn parse_args<I, S>(args: I) -> Result<CliArgs, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut args = args.into_iter().map(Into::into);
    let command = match args.next().as_deref() {
        Some("inspect") => Command::Inspect,
        Some("analyze") => Command::Analyze,
        Some("validate-recording") => Command::ValidateRecording,
        Some("project") => Command::Project,
        Some("compare") => Command::Compare,
        Some("test") => Command::Test,
        Some("report") => Command::Report,
        Some("--help" | "-h") | None => {
            return Err(CliError::Usage("a command is required".to_owned()))
        }
        Some(value) => return Err(CliError::Usage(format!("unknown command {value}"))),
    };
    let mut input = None;
    let mut channel = 0;
    let mut sample_rate = 1000.0;
    let mut migrate_output = None;
    let mut reference = None;
    let mut test = None;
    let mut reference_channel = 0;
    let mut test_channel = 0;
    let mut offset = 0.0;
    let mut alignment = AlignmentMode::Manual;
    let mut reference_time = None;
    let mut test_time = None;
    let mut confidence = None;
    let mut reference_phase = None;
    let mut test_phase = None;
    let mut period = None;
    let mut absolute_tolerance = None;
    let mut relative_tolerance = None;
    let mut relative_floor = 1.0e-12;
    let mut metrics = None;
    let mut rules = None;
    let mut report_compare = None;
    let mut report_output = None;
    let mut report_evidence_output = None;
    let mut report_sources = Vec::new();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--input" | "--file" | "--csv" | "--recording" | "--project" | "--path" => {
                input = Some(next_value(&mut args, &argument)?)
            }
            "--channel" => channel = parse_value(&mut args, "--channel")?,
            "--sample-rate" => sample_rate = parse_positive(&mut args, "--sample-rate")?,
            "--migrate-output" => migrate_output = Some(next_value(&mut args, "--migrate-output")?),
            "--reference" => reference = Some(next_value(&mut args, "--reference")?),
            "--test" => test = Some(next_value(&mut args, "--test")?),
            "--reference-channel" => {
                reference_channel = parse_value(&mut args, "--reference-channel")?
            }
            "--test-channel" => test_channel = parse_value(&mut args, "--test-channel")?,
            "--offset" => offset = parse_finite(&mut args, "--offset")?,
            "--alignment" => alignment = parse_alignment(&mut args)?,
            "--reference-time" => {
                reference_time = Some(parse_finite(&mut args, "--reference-time")?)
            }
            "--test-time" => test_time = Some(parse_finite(&mut args, "--test-time")?),
            "--confidence" => confidence = Some(parse_unit_interval(&mut args, "--confidence")?),
            "--reference-phase" => {
                reference_phase = Some(parse_finite(&mut args, "--reference-phase")?)
            }
            "--test-phase" => test_phase = Some(parse_finite(&mut args, "--test-phase")?),
            "--period" => period = Some(parse_positive(&mut args, "--period")?),
            "--absolute-tolerance" => {
                absolute_tolerance = Some(parse_non_negative(&mut args, "--absolute-tolerance")?)
            }
            "--relative-tolerance" => {
                relative_tolerance = Some(parse_non_negative(&mut args, "--relative-tolerance")?)
            }
            "--relative-floor" => relative_floor = parse_positive(&mut args, "--relative-floor")?,
            "--metrics" => metrics = Some(next_value(&mut args, "--metrics")?),
            "--rules" => rules = Some(next_value(&mut args, "--rules")?),
            "--compare" => report_compare = Some(next_value(&mut args, "--compare")?),
            "--output" => report_output = Some(next_value(&mut args, "--output")?),
            "--evidence-output" => {
                report_evidence_output = Some(next_value(&mut args, "--evidence-output")?)
            }
            "--source" => report_sources.push(next_value(&mut args, "--source")?),
            _ => return Err(CliError::Usage(format!("unknown argument {argument}"))),
        }
    }

    let (reference, test, metrics, rules) = match command {
        Command::Inspect | Command::Analyze | Command::ValidateRecording | Command::Project => {
            (String::new(), String::new(), String::new(), String::new())
        }
        Command::Compare => (
            reference
                .ok_or_else(|| CliError::Usage("compare requires --reference <csv>".to_owned()))?,
            test.ok_or_else(|| CliError::Usage("compare requires --test <csv>".to_owned()))?,
            String::new(),
            String::new(),
        ),
        Command::Test => (
            String::new(),
            String::new(),
            metrics.ok_or_else(|| CliError::Usage("test requires --metrics <json>".to_owned()))?,
            rules.ok_or_else(|| CliError::Usage("test requires --rules <json>".to_owned()))?,
        ),
        Command::Report => {
            if metrics.is_some() != rules.is_some() {
                return Err(CliError::Usage(
                    "report requires both --metrics and --rules when rule results are included"
                        .to_owned(),
                ));
            }
            (
                String::new(),
                String::new(),
                metrics.unwrap_or_default(),
                rules.unwrap_or_default(),
            )
        }
    };
    Ok(CliArgs {
        command,
        input: match command {
            Command::Inspect | Command::Analyze | Command::ValidateRecording | Command::Project => {
                input.ok_or_else(|| {
                    CliError::Usage(format!("{} requires --input <path>", command_name(command)))
                })?
            }
            _ => String::new(),
        },
        channel,
        sample_rate,
        migrate_output: if command == Command::Project {
            migrate_output
        } else {
            None
        },
        reference,
        test,
        reference_channel,
        test_channel,
        offset,
        alignment,
        reference_time,
        test_time,
        confidence,
        reference_phase,
        test_phase,
        period,
        absolute_tolerance,
        relative_tolerance,
        relative_floor,
        metrics,
        rules,
        report_compare: match command {
            Command::Report => report_compare
                .ok_or_else(|| CliError::Usage("report requires --compare <json>".to_owned()))?,
            _ => String::new(),
        },
        report_output: if command == Command::Report {
            report_output
        } else {
            None
        },
        report_evidence_output: if command == Command::Report {
            report_evidence_output
        } else {
            None
        },
        report_sources: if command == Command::Report {
            report_sources
        } else {
            Vec::new()
        },
    })
}

fn parse_alignment<I>(args: &mut I) -> Result<AlignmentMode, CliError>
where
    I: Iterator<Item = String>,
{
    match next_value(args, "--alignment")?.as_str() {
        "manual" | "offset" => Ok(AlignmentMode::Manual),
        "anchor" => Ok(AlignmentMode::Anchor),
        "trigger" | "trigger-point" => Ok(AlignmentMode::Trigger),
        "threshold" | "threshold-event" => Ok(AlignmentMode::Threshold),
        "phase" | "fundamental-phase" => Ok(AlignmentMode::Phase),
        value => Err(CliError::Usage(format!(
            "invalid alignment {value}; expected manual, anchor, trigger, threshold or phase"
        ))),
    }
}

fn command_name(command: Command) -> &'static str {
    match command {
        Command::Inspect => "inspect",
        Command::Analyze => "analyze",
        Command::ValidateRecording => "validate-recording",
        Command::Project => "project",
        Command::Compare => "compare",
        Command::Test => "test",
        Command::Report => "report",
    }
}

fn next_value<I>(args: &mut I, flag: &str) -> Result<String, CliError>
where
    I: Iterator<Item = String>,
{
    args.next()
        .ok_or_else(|| CliError::Usage(format!("{flag} needs a value")))
}

fn parse_value<T, I>(args: &mut I, flag: &str) -> Result<T, CliError>
where
    T: std::str::FromStr,
    I: Iterator<Item = String>,
{
    let value = next_value(args, flag)?;
    value
        .parse()
        .map_err(|_| CliError::Usage(format!("invalid value for {flag}: {value}")))
}

fn parse_finite<I>(args: &mut I, flag: &str) -> Result<f64, CliError>
where
    I: Iterator<Item = String>,
{
    let value: f64 = parse_value(args, flag)?;
    value
        .is_finite()
        .then_some(value)
        .ok_or_else(|| CliError::Usage(format!("{flag} must be finite")))
}

fn parse_non_negative<I>(args: &mut I, flag: &str) -> Result<f64, CliError>
where
    I: Iterator<Item = String>,
{
    let value = parse_finite(args, flag)?;
    (value >= 0.0)
        .then_some(value)
        .ok_or_else(|| CliError::Usage(format!("{flag} must be non-negative")))
}

fn parse_positive<I>(args: &mut I, flag: &str) -> Result<f64, CliError>
where
    I: Iterator<Item = String>,
{
    let value = parse_finite(args, flag)?;
    (value > 0.0)
        .then_some(value)
        .ok_or_else(|| CliError::Usage(format!("{flag} must be positive")))
}

fn parse_unit_interval<I>(args: &mut I, flag: &str) -> Result<f64, CliError>
where
    I: Iterator<Item = String>,
{
    let value = parse_finite(args, flag)?;
    (0.0..=1.0)
        .contains(&value)
        .then_some(value)
        .ok_or_else(|| CliError::Usage(format!("{flag} must be between 0 and 1")))
}

fn run_inspect(args: &CliArgs) -> Result<Value, CliError> {
    let path = Path::new(&args.input);
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("scope"))
    {
        let recording = scope_analyzer::live::recording::ScopeRecording::open(path)
            .map_err(|error| CliError::Input(format!("{}: {error}", path.display())))?;
        return Ok(json!({
            "kind": "scope",
            "path": path.display().to_string(),
            "cleanEnd": recording.clean_end(),
            "recoveredTail": recording.recovered_tail(),
            "metadata": recording_metadata_value(recording.metadata()),
            "sampleFrames": recording.sample_records().len(),
            "gapRecords": recording.gaps().len(),
            "triggerRecords": recording.triggers().len(),
        }));
    }

    let source = CsvDataSource::open_with_sample_rate(path, args.sample_rate)
        .map_err(|error| CliError::Input(format!("{}: {error}", path.display())))?;
    Ok(json!({
        "kind": "csv",
        "path": path.display().to_string(),
        "metadata": dataset_metadata_value(source.metadata()),
    }))
}

fn run_analyze(args: &CliArgs) -> Result<Value, CliError> {
    let path = Path::new(&args.input);
    let source = CsvDataSource::open_with_sample_rate(path, args.sample_rate)
        .map_err(|error| CliError::Input(format!("{}: {error}", path.display())))?;
    let meta = source.metadata();
    if meta.sample_count == 0 || meta.sample_count > MAX_CLI_SAMPLES {
        return Err(CliError::Input(format!(
            "{} has an unsupported sample count: {}",
            path.display(),
            meta.sample_count
        )));
    }
    let channel = meta.channels.get(args.channel).ok_or_else(|| {
        CliError::Input(format!(
            "{} has no channel {} (available: {})",
            path.display(),
            args.channel,
            meta.channels.len()
        ))
    })?;
    let max_points = usize::try_from(MAX_CLI_SAMPLES).unwrap_or(usize::MAX);
    let segments = source
        .read_range_segments(meta.start_time, meta.end_time, &[args.channel], max_points)
        .map_err(|error| CliError::Input(format!("{}: {error}", path.display())))?;
    let mut spec = ChannelMeasurementSpec::new(args.channel, 0, channel.name.clone());
    spec.unit = channel.unit.clone();
    let result = analyze_segments(&segments, &[spec], None)
        .map_err(|error| CliError::Analysis(error.to_string()))?;
    let statistics = result
        .channels
        .first()
        .ok_or_else(|| CliError::Analysis("measurement returned no channel".to_owned()))?;
    Ok(json!({
        "kind": "csv",
        "path": path.display().to_string(),
        "metadata": dataset_metadata_value(meta),
        "segments": segments.len(),
        "channel": {
            "index": statistics.channel_index,
            "name": statistics.name,
            "unit": statistics.unit,
            "validSamples": statistics.valid_samples,
            "duration": statistics.duration,
            "mean": optional_number(statistics.mean),
            "rms": optional_number(statistics.rms),
            "min": optional_number(statistics.min),
            "max": optional_number(statistics.max),
            "positivePeak": optional_number(statistics.positive_peak),
            "negativePeak": optional_number(statistics.negative_peak),
            "absolutePeak": optional_number(statistics.absolute_peak),
            "peakToPeak": optional_number(statistics.peak_to_peak),
            "frequency": statistics.frequency.as_ref().map(|frequency| json!({
                "hz": optional_number(Some(frequency.hz)),
                "acceptedPeriods": frequency.accepted_periods,
                "jitterPercent": optional_number(Some(frequency.jitter_percent)),
            })).unwrap_or(Value::Null),
            "quality": quality_value(&statistics.quality),
        },
        "quality": quality_value(&result.quality),
    }))
}

fn run_validate_recording(args: &CliArgs) -> Result<Value, CliError> {
    let path = Path::new(&args.input);
    let recording = scope_analyzer::live::recording::ScopeRecording::open(path)
        .map_err(|error| CliError::Input(format!("{}: {error}", path.display())))?;
    let clean_end = recording.clean_end();
    let recovered_tail = recording.recovered_tail();
    Ok(json!({
        "path": path.display().to_string(),
        "valid": recording_is_valid(clean_end, recovered_tail),
        "cleanEnd": clean_end,
        "recoveredTail": recovered_tail,
        "metadata": recording_metadata_value(recording.metadata()),
        "sampleFrames": recording.sample_records().len(),
        "sampleCount": recording.sample_records().iter().map(|record| u64::from(record.sample_count)).sum::<u64>(),
        "gapRecords": recording.gaps().len(),
        "triggerRecords": recording.triggers().len(),
    }))
}

fn recording_is_valid(clean_end: bool, recovered_tail: bool) -> bool {
    clean_end && !recovered_tail
}

fn run_project(args: &CliArgs) -> Result<Value, CliError> {
    let path = Path::new(&args.input);
    let document = scope_analyzer::project::load_project(path)
        .map_err(|error| CliError::Project(format!("{}: {error}", path.display())))?;
    if let Some(output) = &args.migrate_output {
        scope_analyzer::project::save_project_atomic(Path::new(output), &document)
            .map_err(|error| CliError::Project(format!("{}: {error}", output)))?;
    }
    Ok(json!({
        "path": path.display().to_string(),
        "schemaVersion": document.schema_version,
        "projectId": document.project_id.0,
        "sourceCount": document.sources.len(),
        "datasetCount": document.datasets.len(),
        "captureCount": document.captures.len(),
        "compareEnabled": document.compare.enabled,
        "compareMappings": document.compare.channel_mappings.len(),
        "migratedOutput": args.migrate_output,
    }))
}

fn dataset_metadata_value(meta: &scope_analyzer::data::DatasetMeta) -> Value {
    json!({
        "sourceName": meta.source_name,
        "startTime": optional_number(Some(meta.start_time)),
        "endTime": optional_number(Some(meta.end_time)),
        "duration": optional_number(Some(meta.duration())),
        "sampleCount": meta.sample_count,
        "nominalSampleRateHz": optional_number(Some(meta.nominal_sample_rate_hz)),
        "channels": meta.channels.iter().map(|channel| json!({
            "index": channel.index,
            "name": channel.name,
            "unit": channel.unit,
            "sampleRateHz": optional_number(Some(channel.sample_rate_hz)),
            "scale": optional_number(Some(f64::from(channel.scale))),
            "defaultVisible": channel.default_visible,
        })).collect::<Vec<_>>(),
    })
}

fn recording_metadata_value(
    metadata: &scope_analyzer::live::recording::RecordingMetadata,
) -> Value {
    json!({
        "deviceId": metadata.device_id,
        "firmwareName": metadata.firmware_name,
        "tickHz": metadata.tick_hz,
        "sampleRateHz": metadata.sample_rate_hz,
        "batchSamples": metadata.batch_samples,
        "channelMask": metadata.channel_mask,
        "clientVersion": metadata.client_version,
        "channelTableRevision": metadata.channel_table.revision,
        "channels": metadata.channel_table.channels.iter().map(|channel| json!({
            "id": channel.channel_id,
            "name": channel.name,
            "unit": channel.unit,
            "kind": channel.kind,
            "wireFormat": channel.wire_format,
            "scale": optional_number(Some(f64::from(channel.scale))),
            "offset": optional_number(Some(f64::from(channel.offset))),
        })).collect::<Vec<_>>(),
    })
}

fn optional_number(value: Option<f64>) -> Value {
    value
        .filter(|value| value.is_finite())
        .map(Value::from)
        .unwrap_or(Value::Null)
}

fn quality_value(quality: &scope_analyzer::measurements::MeasurementQuality) -> Value {
    json!({
        "containsGap": quality.contains_gap,
        "insufficientSamples": quality.insufficient_samples,
        "lowAmplitude": quality.low_amplitude,
        "invalidTimebase": quality.invalid_timebase,
        "incompleteChannels": quality.incomplete_channels,
        "valid": quality.is_valid(),
    })
}

fn run_compare(args: &CliArgs) -> Result<scope_analyzer::compare::CompareResult, CliError> {
    let reference = load_series(Path::new(&args.reference), args.reference_channel)?;
    let test = load_series(Path::new(&args.test), args.test_channel)?;
    let mut request = CompareRequest::new(reference, test);
    request.alignment = match args.alignment {
        AlignmentMode::Manual => AlignmentSpec::ManualOffset {
            seconds: args.offset,
        },
        AlignmentMode::Anchor => AlignmentSpec::Anchor {
            reference_time: required_alignment_value(args.reference_time, "--reference-time")?,
            test_time: required_alignment_value(args.test_time, "--test-time")?,
        },
        AlignmentMode::Trigger => AlignmentSpec::TriggerPoint {
            reference_time: required_alignment_value(args.reference_time, "--reference-time")?,
            test_time: required_alignment_value(args.test_time, "--test-time")?,
            confidence: args.confidence.unwrap_or(1.0),
        },
        AlignmentMode::Threshold => AlignmentSpec::ThresholdEvent {
            reference_time: required_alignment_value(args.reference_time, "--reference-time")?,
            test_time: required_alignment_value(args.test_time, "--test-time")?,
            confidence: args.confidence.unwrap_or(1.0),
        },
        AlignmentMode::Phase => AlignmentSpec::FundamentalPhase {
            reference_phase_radians: required_alignment_value(
                args.reference_phase,
                "--reference-phase",
            )?,
            test_phase_radians: required_alignment_value(args.test_phase, "--test-phase")?,
            period_seconds: required_alignment_value(args.period, "--period")?,
            confidence: args.confidence.unwrap_or(1.0),
        },
    };
    request.relative_floor = args.relative_floor;
    if args.absolute_tolerance.is_some() || args.relative_tolerance.is_some() {
        request.tolerance = Some(Tolerance {
            absolute: args.absolute_tolerance,
            relative: args.relative_tolerance,
        });
    }
    compare(request).map_err(|error| CliError::Compare(error.to_string()))
}

fn required_alignment_value(value: Option<f64>, flag: &str) -> Result<f64, CliError> {
    value.ok_or_else(|| CliError::Usage(format!("selected alignment requires {flag}")))
}

#[derive(Deserialize)]
struct RuleDocument {
    rules: Vec<RuleSpec>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuleMetricSeriesDocument {
    metrics: BTreeMap<String, Vec<MetricSample>>,
    #[serde(default)]
    events: BTreeMap<String, f64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RuleMetricsInput {
    Scalars(BTreeMap<String, f64>),
    Series(RuleMetricSeriesDocument),
}

impl RuleMetricsInput {
    fn evaluate(&self, rules: &[RuleSpec]) -> Result<RuleEvaluation, CliError> {
        match self {
            Self::Scalars(metrics) => {
                evaluate(rules, metrics).map_err(|error| CliError::Rule(error.to_string()))
            }
            Self::Series(document) => evaluate_series(rules, &document.metrics, &document.events)
                .map_err(|error| CliError::Rule(error.to_string())),
        }
    }

    fn key_measurements(&self) -> BTreeMap<String, f64> {
        match self {
            Self::Scalars(metrics) => metrics.clone(),
            Self::Series(document) => document
                .metrics
                .iter()
                .filter_map(|(name, samples)| {
                    samples
                        .iter()
                        .rev()
                        .find(|sample| sample.value.is_finite())
                        .map(|sample| (name.clone(), sample.value))
                })
                .collect(),
        }
    }
}

fn load_rule_metrics(path: &str) -> Result<RuleMetricsInput, CliError> {
    let text =
        fs::read_to_string(path).map_err(|error| CliError::Input(format!("{path}: {error}")))?;
    serde_json::from_str(&text).map_err(|error| CliError::Input(format!("{path}: {error}")))
}

fn load_rule_specs(path: &str) -> Result<Vec<RuleSpec>, CliError> {
    let text =
        fs::read_to_string(path).map_err(|error| CliError::Input(format!("{path}: {error}")))?;
    let value = serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|error| CliError::Input(format!("{path}: {error}")))?;
    if value.is_array() {
        serde_json::from_value::<Vec<RuleSpec>>(value)
    } else {
        serde_json::from_value::<RuleDocument>(value).map(|document| document.rules)
    }
    .map_err(|error| CliError::Input(format!("{path}: {error}")))
}

fn run_rule_test(args: &CliArgs) -> Result<RuleEvaluation, CliError> {
    let metrics = load_rule_metrics(&args.metrics)?;
    let rules = load_rule_specs(&args.rules)?;
    metrics.evaluate(&rules)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportArtifact {
    schema_version: u32,
    application_version: &'static str,
    markdown: String,
    output: Option<String>,
    rules: Option<RuleEvaluation>,
    compare_evidence: scope_analyzer::compare::CompareEvidence,
    source_hashes: BTreeMap<String, String>,
    key_measurements: BTreeMap<String, f64>,
    data_quality: Value,
    trigger_quality: Vec<Value>,
    evidence_svg: String,
}

fn run_report(args: &CliArgs) -> Result<ReportArtifact, CliError> {
    let text = fs::read_to_string(&args.report_compare)
        .map_err(|error| CliError::Input(format!("{}: {error}", args.report_compare)))?;
    let value = serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|error| CliError::Input(format!("{}: {error}", args.report_compare)))?;
    let result_value = value.get("result").cloned().unwrap_or(value);
    let result = serde_json::from_value::<scope_analyzer::compare::CompareResult>(result_value)
        .map_err(|error| CliError::Input(format!("{}: {error}", args.report_compare)))?;
    let compare_evidence = result.evidence();
    let mut key_measurements = BTreeMap::new();
    let rule_evaluation = if args.metrics.is_empty() {
        None
    } else {
        let metrics = load_rule_metrics(&args.metrics)?;
        key_measurements = metrics.key_measurements();
        let rules = load_rule_specs(&args.rules)?;
        Some(metrics.evaluate(&rules)?)
    };
    let mut source_hashes = BTreeMap::new();
    let trigger_quality = collect_trigger_quality(&args.report_sources)?;
    if args.report_sources.is_empty() {
        source_hashes.insert(
            "compare".to_owned(),
            content_hash(Path::new(&args.report_compare))?,
        );
    } else {
        for source in &args.report_sources {
            source_hashes.insert(source.clone(), content_hash(Path::new(source))?);
        }
    }
    if !args.metrics.is_empty() {
        source_hashes.insert(
            "metrics".to_owned(),
            content_hash(Path::new(&args.metrics))?,
        );
        source_hashes.insert("rules".to_owned(), content_hash(Path::new(&args.rules))?);
    }
    let total_points = result.summary.valid_points + result.summary.invalid_points;
    let data_quality = json!({
        "validPoints": result.summary.valid_points,
        "invalidPoints": result.summary.invalid_points,
        "invalidFraction": (total_points > 0).then_some(result.summary.invalid_points as f64 / total_points as f64),
        "alignmentConfidence": result.alignment_confidence,
        "gapAware": true,
        "triggerQuality": trigger_quality.clone(),
        "keyMeasurements": key_measurements.clone(),
    });
    let evidence_svg = render_evidence_svg(&result);
    let mut markdown = String::new();
    markdown.push_str("# Scope Compare Report\n\n");
    markdown.push_str(&format!(
        "{}\n\n- Alignment offset: `{:.9}` s\n- Alignment confidence: `{:.3}`\n- Valid points: `{}`\n- Invalid points: `{}`\n- RMS error: `{:.9}`\n- Maximum absolute error: `{:.9}`\n- Maximum relative error: `{:.6}%`\n\n",
        result.evidence_line(),
        result.alignment_offset_seconds,
        result.alignment_confidence,
        result.summary.valid_points,
        result.summary.invalid_points,
        result.summary.rms_error,
        result.summary.max_absolute_error,
        result.summary.max_relative_error * 100.0,
    ));
    markdown.push_str("## Provenance\n\n");
    markdown.push_str(&format!(
        "- Application version: `{}`\n- Compare schema: `{}`\n- Source hashes (CRC32C): `{}`\n- Key measurements: `{}`\n- Trigger/gap quality: `{}`\n- Data quality: `{}`\n\n",
        env!("CARGO_PKG_VERSION"),
        CLI_SCHEMA_VERSION,
        serde_json::to_string(&source_hashes).expect("hash map is serializable"),
        serde_json::to_string(&key_measurements).expect("measurements are serializable"),
        serde_json::to_string(&trigger_quality).expect("trigger quality is serializable"),
        serde_json::to_string(&data_quality).expect("quality is serializable"),
    ));
    markdown.push_str("## Evidence figure\n\nThe deterministic SVG evidence figure is returned in `evidenceSvg`.\n\n");
    markdown.push_str("## Exceedance intervals\n\n");
    if result.summary.exceedance_intervals.is_empty() {
        markdown.push_str("None\n");
    } else {
        markdown.push_str("| Start (s) | End (s) |\n| ---: | ---: |\n");
        for interval in result.summary.exceedance_intervals {
            markdown.push_str(&format!(
                "| {:.9} | {:.9} |\n",
                interval.start, interval.end
            ));
        }
    }
    if let Some(evaluation) = &rule_evaluation {
        markdown.push_str("\n## Rule evaluation\n\n");
        markdown.push_str(&format!(
            "Overall: `{}`\n\n",
            if evaluation.passed {
                "Passed"
            } else {
                "Failed"
            }
        ));
        markdown.push_str(
            "| Rule | Input | Metric | Observed | Severity | Status | Evidence window |\n| --- | --- | --- | ---: | --- | --- | --- |\n",
        );
        for outcome in &evaluation.outcomes {
            let observed = outcome
                .observed
                .filter(|value| value.is_finite())
                .map(|value| format!("{value:.9}"))
                .unwrap_or_else(|| "n/a".to_owned());
            let evidence = outcome
                .evidence
                .as_ref()
                .and_then(|evidence| evidence.window_start.zip(evidence.window_end))
                .map(|(start, end)| format!("{start:.6}..{end:.6}"))
                .unwrap_or_else(|| "n/a".to_owned());
            markdown.push_str(&format!(
                "| {} | {:?} | {} | {} | {:?} | {:?} | {} |\n",
                outcome.id,
                outcome.input,
                outcome.metric,
                observed,
                outcome.severity,
                outcome.status,
                evidence
            ));
        }
    }
    if let Some(output) = &args.report_output {
        fs::write(output, &markdown)
            .map_err(|error| CliError::Input(format!("{}: {error}", output)))?;
    }
    if let Some(output) = &args.report_evidence_output {
        fs::write(output, evidence_svg.as_bytes())
            .map_err(|error| CliError::Input(format!("{}: {error}", output)))?;
    }
    Ok(ReportArtifact {
        schema_version: CLI_SCHEMA_VERSION,
        application_version: env!("CARGO_PKG_VERSION"),
        markdown,
        output: args.report_output.clone(),
        rules: rule_evaluation,
        compare_evidence,
        source_hashes,
        key_measurements,
        data_quality,
        trigger_quality,
        evidence_svg,
    })
}

fn collect_trigger_quality(sources: &[String]) -> Result<Vec<Value>, CliError> {
    let mut quality = Vec::new();
    for source in sources {
        let path = Path::new(source);
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("scope"))
        {
            continue;
        }
        let recording = scope_analyzer::live::recording::ScopeRecording::open(path)
            .map_err(|error| CliError::Input(format!("{}: {error}", path.display())))?;
        quality.push(json!({
            "source": source,
            "cleanEnd": recording.clean_end(),
            "recoveredTail": recording.recovered_tail(),
            "gapRecords": recording.gaps().len(),
            "gaps": recording.gaps().iter().map(|gap| json!({
                "startSampleIndex": gap.start_sample_index,
                "missingSamples": gap.missing_samples,
                "reason": gap_reason_name(gap.reason),
            })).collect::<Vec<_>>(),
            "triggerRecords": recording.triggers().len(),
            "triggers": recording.triggers().iter().map(|trigger| json!({
                "timestampTicks": trigger.timestamp_ticks,
                "triggerSampleIndex": trigger.trigger_sample_index,
                "autoTimeout": trigger.auto_timeout,
            })).collect::<Vec<_>>(),
        }));
    }
    Ok(quality)
}

fn gap_reason_name(reason: scope_analyzer::live::buffer::GapReason) -> &'static str {
    match reason {
        scope_analyzer::live::buffer::GapReason::SequenceLoss => "sequence-loss",
        scope_analyzer::live::buffer::GapReason::SampleIndexLoss => "sample-index-loss",
        scope_analyzer::live::buffer::GapReason::HostBackpressure => "host-backpressure",
        scope_analyzer::live::buffer::GapReason::DeviceReported => "device-reported",
    }
}

fn content_hash(path: &Path) -> Result<String, CliError> {
    let bytes =
        fs::read(path).map_err(|error| CliError::Input(format!("{}: {error}", path.display())))?;
    Ok(format!(
        "crc32c:{:08x}",
        scope_analyzer::live::protocol::crc32c(&bytes)
    ))
}

fn render_evidence_svg(result: &scope_analyzer::compare::CompareResult) -> String {
    const WIDTH: f64 = 960.0;
    const HEIGHT: f64 = 320.0;
    let evidence_y = HEIGHT - 8.0;
    let valid_points = result
        .points
        .iter()
        .filter(|point| point.valid && point.test.is_some())
        .collect::<Vec<_>>();
    let (min_time, max_time) = valid_points
        .iter()
        .map(|point| point.time)
        .fold(None, |range: Option<(f64, f64)>, time| {
            Some(match range {
                Some((min, max)) => (min.min(time), max.max(time)),
                None => (time, time),
            })
        })
        .unwrap_or((0.0, 1.0));
    let (min_value, max_value) = valid_points
        .iter()
        .flat_map(|point| [Some(point.reference), point.test].into_iter().flatten())
        .fold(None, |range: Option<(f64, f64)>, value| {
            Some(match range {
                Some((min, max)) => (min.min(value), max.max(value)),
                None => (value, value),
            })
        })
        .unwrap_or((0.0, 1.0));
    let time_span = (max_time - min_time).max(f64::EPSILON);
    let value_span = (max_value - min_value).max(f64::EPSILON);
    let polyline = |test: bool, color: &str| {
        let points = valid_points
            .iter()
            .take(4_000)
            .filter_map(|point| {
                let value = if test { point.test? } else { point.reference };
                let x = 24.0 + (point.time - min_time) / time_span * (WIDTH - 48.0);
                let y = 20.0 + (max_value - value) / value_span * (HEIGHT - 40.0);
                Some(format!("{x:.3},{y:.3}"))
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "<polyline fill=\"none\" stroke=\"{color}\" stroke-width=\"1.2\" points=\"{points}\"/>\n"
        )
    };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {WIDTH} {HEIGHT}\" role=\"img\" aria-label=\"Scope compare evidence\">\n<rect width=\"100%\" height=\"100%\" fill=\"white\"/>\n{}{}<text x=\"24\" y=\"18\" font-family=\"sans-serif\" font-size=\"12\">Reference (blue) / Test (orange)</text><text x=\"24\" y=\"{evidence_y:.0}\" font-family=\"monospace\" font-size=\"10\">{}</text>\n</svg>\n",
        polyline(false, "#2563eb"),
        polyline(true, "#ea580c"),
        result.evidence_line(),
    )
}

fn load_series(path: &Path, channel: usize) -> Result<Series, CliError> {
    let source = CsvDataSource::open_with_sample_rate(path, 1000.0)
        .map_err(|error| CliError::Input(format!("{}: {error}", path.display())))?;
    let meta = source.metadata();
    if meta.sample_count == 0 || meta.sample_count > MAX_CLI_SAMPLES {
        return Err(CliError::Input(format!(
            "{} has an unsupported sample count: {}",
            path.display(),
            meta.sample_count
        )));
    }
    let block = source
        .read_range(
            meta.start_time,
            meta.end_time,
            &[channel],
            usize::try_from(meta.sample_count).unwrap_or(usize::MAX),
        )
        .map_err(|error| CliError::Input(format!("{}: {error}", path.display())))?;
    let values = block.channels.first().ok_or_else(|| {
        CliError::Input(format!(
            "{} has no readable channel {channel}",
            path.display()
        ))
    })?;
    split_segments(&block.times, values, meta.nominal_sample_rate_hz, path)
}

fn split_segments(
    times: &[f64],
    values: &[f32],
    sample_rate_hz: f64,
    path: &Path,
) -> Result<Series, CliError> {
    let mut segments = Vec::new();
    let mut segment_times = Vec::new();
    let mut segment_values = Vec::new();
    let gap_limit = (1.5 / sample_rate_hz.max(1.0)).max(f64::EPSILON);
    for (&time, &value) in times.iter().zip(values) {
        if !time.is_finite() || !f64::from(value).is_finite() {
            flush_segment(&mut segments, &mut segment_times, &mut segment_values, path)?;
            continue;
        }
        if segment_times.last().is_some_and(|last| {
            let delta = time - *last;
            delta <= 0.0 || delta > gap_limit
        }) {
            flush_segment(&mut segments, &mut segment_times, &mut segment_values, path)?;
        }
        segment_times.push(time);
        segment_values.push(f64::from(value));
    }
    flush_segment(&mut segments, &mut segment_times, &mut segment_values, path)?;
    Series::new(segments).map_err(|error| CliError::Input(format!("{}: {error}", path.display())))
}

fn flush_segment(
    segments: &mut Vec<SeriesSegment>,
    times: &mut Vec<f64>,
    values: &mut Vec<f64>,
    path: &Path,
) -> Result<(), CliError> {
    if times.is_empty() {
        return Ok(());
    }
    let segment = SeriesSegment::new(std::mem::take(times), std::mem::take(values))
        .map_err(|error| CliError::Input(format!("{}: {error}", path.display())))?;
    segments.push(segment);
    Ok(())
}

fn print_help() {
    eprintln!(
        "scope-cli inspect --input <csv|scope> [--sample-rate <hz>]\n\
         scope-cli analyze --input <csv> [--channel <index>] [--sample-rate <hz>]\n\
         scope-cli validate-recording --input <scope>\n\
         scope-cli project --input <scopeproj> [--migrate-output <scopeproj>]\n\
         scope-cli compare --reference <csv> --test <csv> [options]\n\
         --reference-channel <index>  Zero-based reference channel (default 0)\n\
         --test-channel <index>       Zero-based test channel (default 0)\n\
         --offset <seconds>           Test time offset relative to reference\n\
         --alignment <mode>           manual|anchor|trigger|threshold|phase\n\
         --reference-time <seconds>   Reference event time (event/anchor modes)\n\
         --test-time <seconds>        Test event time (event/anchor modes)\n\
         --confidence <0..1>          Alignment confidence (event/phase modes)\n\
         --reference-phase <radians>  Reference phase (phase mode)\n\
         --test-phase <radians>       Test phase (phase mode)\n\
         --period <seconds>            Fundamental period (phase mode)\n\
         --absolute-tolerance <value> Absolute error tolerance\n\
         --relative-tolerance <value> Relative error tolerance\n\
         --relative-floor <value>     Positive denominator floor (default 1e-12)\n\
         scope-cli test --metrics <json> --rules <json>\n\
         scope-cli report --compare <json> [--source <file>]... [--metrics <json> --rules <json>] [--output <markdown>] [--evidence-output <svg>]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compare_cli_defaults_and_overrides() {
        let args = parse_args([
            "compare",
            "--reference",
            "reference.csv",
            "--test",
            "test.csv",
            "--reference-channel",
            "2",
            "--test-channel",
            "3",
            "--offset",
            "0.25",
            "--absolute-tolerance",
            "0.1",
            "--relative-tolerance",
            "0.05",
        ])
        .unwrap();

        assert_eq!(args.command, Command::Compare);
        assert_eq!(args.reference, "reference.csv");
        assert_eq!(args.test, "test.csv");
        assert_eq!(args.reference_channel, 2);
        assert_eq!(args.test_channel, 3);
        assert_eq!(args.offset, 0.25);
        assert_eq!(args.alignment, AlignmentMode::Manual);
        assert_eq!(args.absolute_tolerance, Some(0.1));
        assert_eq!(args.relative_tolerance, Some(0.05));
    }

    #[test]
    fn parses_inspect_and_analyze_inputs() {
        let inspect =
            parse_args(["inspect", "--input", "data.csv", "--sample-rate", "2000"]).unwrap();
        assert_eq!(inspect.command, Command::Inspect);
        assert_eq!(inspect.input, "data.csv");
        assert_eq!(inspect.sample_rate, 2000.0);

        let analyze = parse_args(["analyze", "--csv", "data.csv", "--channel", "2"]).unwrap();
        assert_eq!(analyze.command, Command::Analyze);
        assert_eq!(analyze.input, "data.csv");
        assert_eq!(analyze.channel, 2);
    }

    #[test]
    fn parses_recording_and_project_commands() {
        let recording = parse_args(["validate-recording", "--recording", "capture.scope"]).unwrap();
        assert_eq!(recording.command, Command::ValidateRecording);
        assert_eq!(recording.input, "capture.scope");

        let project = parse_args([
            "project",
            "--project",
            "analysis.scopeproj",
            "--migrate-output",
            "migrated.scopeproj",
        ])
        .unwrap();
        assert_eq!(project.command, Command::Project);
        assert_eq!(project.input, "analysis.scopeproj");
        assert_eq!(
            project.migrate_output,
            Some("migrated.scopeproj".to_owned())
        );
    }

    #[test]
    fn rejects_compare_without_required_paths() {
        assert!(matches!(
            parse_args(["compare", "--reference", "reference.csv"]),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn parses_phase_alignment_options() {
        let args = parse_args([
            "compare",
            "--reference",
            "reference.csv",
            "--test",
            "test.csv",
            "--alignment",
            "phase",
            "--reference-phase",
            "0.25",
            "--test-phase",
            "-0.25",
            "--period",
            "0.02",
            "--confidence",
            "0.8",
        ])
        .unwrap();
        assert_eq!(args.alignment, AlignmentMode::Phase);
        assert_eq!(args.reference_phase, Some(0.25));
        assert_eq!(args.test_phase, Some(-0.25));
        assert_eq!(args.period, Some(0.02));
        assert_eq!(args.confidence, Some(0.8));
    }

    #[test]
    fn parses_rule_test_command() {
        let args =
            parse_args(["test", "--metrics", "metrics.json", "--rules", "rules.json"]).unwrap();
        assert_eq!(args.command, Command::Test);
        assert_eq!(args.metrics, "metrics.json");
        assert_eq!(args.rules, "rules.json");
    }

    #[test]
    fn timestamped_metrics_enable_windowed_rules() {
        let metrics: RuleMetricsInput = serde_json::from_str(
            r#"{
                "metrics": {
                    "rms": [
                        {"time": 0.0, "value": 0.2},
                        {"time": 1.0, "value": 0.4},
                        {"time": 2.0, "value": 0.3}
                    ]
                },
                "events": {"trigger": 0.0}
            }"#,
        )
        .unwrap();
        let mut rule = RuleSpec::less_equal("rms", 0.5);
        rule.window = Some(scope_analyzer::rules::RuleWindow::EventRelative {
            event: "trigger".to_owned(),
            start: 0.5,
            end: 2.0,
        });
        rule.duration_seconds = Some(1.0);

        let result = metrics.evaluate(&[rule]).unwrap();

        assert!(result.passed);
        assert_eq!(
            result.outcomes[0].evidence.as_ref().unwrap().sample_count,
            2
        );
        assert_eq!(metrics.key_measurements().get("rms"), Some(&0.3));
    }

    #[test]
    fn semantic_validation_failures_use_ci_failure_exit_code() {
        assert_eq!(
            success_exit_code("test", &serde_json::json!({"passed": false})),
            5
        );
        assert_eq!(
            success_exit_code("test", &serde_json::json!({"passed": true})),
            0
        );
        assert_eq!(
            success_exit_code("report", &serde_json::json!({"passed": false})),
            0
        );
        assert_eq!(
            success_exit_code("validate-recording", &serde_json::json!({"valid": false})),
            5
        );
        assert_eq!(
            success_exit_code("validate-recording", &serde_json::json!({"valid": true})),
            0
        );
    }

    #[test]
    fn incomplete_recordings_are_not_reported_as_valid() {
        assert!(recording_is_valid(true, false));
        assert!(!recording_is_valid(false, true));
        assert!(!recording_is_valid(false, false));
        assert!(!recording_is_valid(true, true));
    }

    #[test]
    fn parses_report_command() {
        let args = parse_args([
            "report",
            "--compare",
            "compare.json",
            "--metrics",
            "metrics.json",
            "--rules",
            "rules.json",
            "--source",
            "reference.csv",
            "--output",
            "report.md",
        ])
        .unwrap();
        assert_eq!(args.command, Command::Report);
        assert_eq!(args.report_compare, "compare.json");
        assert_eq!(args.metrics, "metrics.json");
        assert_eq!(args.rules, "rules.json");
        assert_eq!(args.report_sources, vec!["reference.csv"]);
        assert_eq!(args.report_output, Some("report.md".to_owned()));
    }

    #[test]
    fn splits_non_finite_samples_without_bridging_them() {
        let series = split_segments(
            &[0.0, 1.0, 2.0],
            &[1.0, f32::NAN, 3.0],
            1.0,
            Path::new("fixture.csv"),
        )
        .unwrap();
        assert_eq!(series.segments().len(), 2);
    }
}
