use std::cmp::Ordering;

use rustfft::num_complex::Complex;
use thiserror::Error;

use crate::data::SampleBlock;

const MIN_STAT_SAMPLES: usize = 2;
const MIN_FREQUENCY_PERIODS: usize = 3;
const NUMERICAL_FLOOR: f64 = 1.0e-12;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeasurementQuality {
    pub contains_gap: bool,
    pub insufficient_samples: bool,
    pub low_amplitude: bool,
    pub invalid_timebase: bool,
    pub incomplete_channels: bool,
}

impl MeasurementQuality {
    pub fn is_valid(&self) -> bool {
        !self.insufficient_samples && !self.invalid_timebase && !self.incomplete_channels
    }

    fn merge(&mut self, other: &Self) {
        self.contains_gap |= other.contains_gap;
        self.insufficient_samples |= other.insufficient_samples;
        self.low_amplitude |= other.low_amplitude;
        self.invalid_timebase |= other.invalid_timebase;
        self.incomplete_channels |= other.incomplete_channels;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrequencyEstimate {
    pub hz: f64,
    pub accepted_periods: usize,
    pub jitter_percent: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelMeasurementSpec {
    pub channel_index: usize,
    pub column: usize,
    pub name: String,
    pub unit: String,
    pub scale: f64,
}

impl ChannelMeasurementSpec {
    pub fn new(channel_index: usize, column: usize, name: impl Into<String>) -> Self {
        Self {
            channel_index,
            column,
            name: name.into(),
            unit: String::new(),
            scale: 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelStatistics {
    pub channel_index: usize,
    pub name: String,
    pub unit: String,
    pub valid_samples: usize,
    pub duration: f64,
    pub mean: Option<f64>,
    pub rms: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub positive_peak: Option<f64>,
    pub negative_peak: Option<f64>,
    pub absolute_peak: Option<f64>,
    pub peak_to_peak: Option<f64>,
    pub frequency: Option<FrequencyEstimate>,
    pub quality: MeasurementQuality,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreePhasePowerSpec {
    pub voltage_columns: [usize; 3],
    pub current_columns: [usize; 3],
    pub voltage_scales: [f64; 3],
    pub current_scales: [f64; 3],
    pub nominal_frequency_hz: f64,
    pub voltage_unit: String,
    pub current_unit: String,
}

impl ThreePhasePowerSpec {
    pub fn new(voltage_columns: [usize; 3], current_columns: [usize; 3]) -> Self {
        Self {
            voltage_columns,
            current_columns,
            voltage_scales: [1.0; 3],
            current_scales: [1.0; 3],
            nominal_frequency_hz: 50.0,
            voltage_unit: "V".to_owned(),
            current_unit: "A".to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreePhasePower {
    pub valid_samples: usize,
    pub active_power: f64,
    pub fundamental_reactive_power: f64,
    pub effective_apparent_power: f64,
    pub true_power_factor: Option<f64>,
    pub frequency_hz: f64,
    pub power_unit: String,
    pub quality: MeasurementQuality,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EngineeringMeasurementResult {
    pub start_time: Option<f64>,
    pub end_time: Option<f64>,
    pub channels: Vec<ChannelStatistics>,
    pub power: Option<ThreePhasePower>,
    pub quality: MeasurementQuality,
}

#[derive(Debug, Error, PartialEq)]
pub enum MeasurementError {
    #[error("measurement channel scale must be finite")]
    InvalidScale,
    #[error("measurement channel column {0} is unavailable")]
    MissingColumn(usize),
    #[error("measurement segment times and channel columns are not aligned")]
    UnalignedSegment,
    #[error("measurement segment timestamps must be finite and strictly increasing")]
    InvalidTimebase,
    #[error("three-phase power configuration is invalid: {0}")]
    InvalidPowerConfig(String),
}

pub fn analyze_segments(
    segments: &[SampleBlock],
    channels: &[ChannelMeasurementSpec],
    power: Option<&ThreePhasePowerSpec>,
) -> Result<EngineeringMeasurementResult, MeasurementError> {
    validate_segments(segments)?;
    if channels.iter().any(|channel| !channel.scale.is_finite()) {
        return Err(MeasurementError::InvalidScale);
    }
    for channel in channels {
        ensure_column_exists(segments, channel.column)?;
    }

    let contains_gap = segments.len() > 1;
    let mut rows = Vec::with_capacity(channels.len());
    let mut aggregate_quality = MeasurementQuality {
        contains_gap,
        ..MeasurementQuality::default()
    };
    for channel in channels {
        let row = analyze_channel(segments, channel, contains_gap);
        aggregate_quality.merge(&row.quality);
        rows.push(row);
    }

    let power_result = power
        .map(|spec| analyze_three_phase_power(segments, spec, contains_gap))
        .transpose()?;
    if let Some(power) = &power_result {
        aggregate_quality.merge(&power.quality);
    }

    let start_time = segments
        .iter()
        .filter_map(|segment| segment.times.first().copied())
        .min_by(|left, right| left.total_cmp(right));
    let end_time = segments
        .iter()
        .filter_map(|segment| segment.times.last().copied())
        .max_by(|left, right| left.total_cmp(right));

    Ok(EngineeringMeasurementResult {
        start_time,
        end_time,
        channels: rows,
        power: power_result,
        quality: aggregate_quality,
    })
}

fn validate_segments(segments: &[SampleBlock]) -> Result<(), MeasurementError> {
    for segment in segments {
        if segment
            .channels
            .iter()
            .any(|column| column.len() != segment.times.len())
        {
            return Err(MeasurementError::UnalignedSegment);
        }
        if segment.times.iter().any(|time| !time.is_finite())
            || segment.times.windows(2).any(|pair| pair[1] <= pair[0])
        {
            return Err(MeasurementError::InvalidTimebase);
        }
    }
    Ok(())
}

fn ensure_column_exists(segments: &[SampleBlock], column: usize) -> Result<(), MeasurementError> {
    if segments
        .iter()
        .any(|segment| !segment.times.is_empty() && column >= segment.channels.len())
    {
        Err(MeasurementError::MissingColumn(column))
    } else {
        Ok(())
    }
}

fn analyze_channel(
    segments: &[SampleBlock],
    spec: &ChannelMeasurementSpec,
    contains_gap: bool,
) -> ChannelStatistics {
    let mut valid_samples = 0_usize;
    let mut sum = 0.0_f64;
    let mut sum_squares = 0.0_f64;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut duration = 0.0_f64;

    for segment in segments {
        let Some(values) = segment.channels.get(spec.column) else {
            continue;
        };
        if let (Some(first), Some(last)) = (segment.times.first(), segment.times.last()) {
            duration += (last - first).max(0.0);
        }
        for value in values.iter().copied() {
            let scaled = f64::from(value) * spec.scale;
            if !scaled.is_finite() {
                continue;
            }
            valid_samples += 1;
            sum += scaled;
            sum_squares += scaled * scaled;
            min = min.min(scaled);
            max = max.max(scaled);
        }
    }

    let mut quality = MeasurementQuality {
        contains_gap,
        insufficient_samples: valid_samples < MIN_STAT_SAMPLES,
        ..MeasurementQuality::default()
    };
    let (mean, rms, min_value, max_value, absolute_peak, peak_to_peak) =
        if valid_samples >= MIN_STAT_SAMPLES {
            let count = valid_samples as f64;
            (
                Some(sum / count),
                Some((sum_squares / count).sqrt()),
                Some(min),
                Some(max),
                Some(min.abs().max(max.abs())),
                Some(max - min),
            )
        } else {
            (None, None, None, None, None, None)
        };

    let (frequency, frequency_quality) = estimate_frequency(segments, spec.column, spec.scale);
    quality.merge(&frequency_quality);

    ChannelStatistics {
        channel_index: spec.channel_index,
        name: spec.name.clone(),
        unit: spec.unit.clone(),
        valid_samples,
        duration,
        mean,
        rms,
        min: min_value,
        max: max_value,
        positive_peak: max_value,
        negative_peak: min_value,
        absolute_peak,
        peak_to_peak,
        frequency,
        quality,
    }
}

fn estimate_frequency(
    segments: &[SampleBlock],
    column: usize,
    scale: f64,
) -> (Option<FrequencyEstimate>, MeasurementQuality) {
    let mut best: Option<FrequencyEstimate> = None;
    let mut any_valid_timebase = false;
    let mut any_amplitude = false;
    for segment in segments {
        let Some(values) = segment.channels.get(column) else {
            continue;
        };
        let finite = values
            .iter()
            .copied()
            .map(|value| f64::from(value) * scale)
            .filter(|value| value.is_finite())
            .collect::<Vec<_>>();
        if finite.len() < MIN_FREQUENCY_PERIODS + 1 {
            continue;
        }
        any_valid_timebase = true;
        let mean = finite.iter().sum::<f64>() / finite.len() as f64;
        let min = finite.iter().copied().fold(f64::INFINITY, f64::min);
        let max = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let span = max - min;
        if !span.is_finite() || span <= NUMERICAL_FLOOR {
            continue;
        }
        any_amplitude = true;
        let half_hysteresis = (span * 0.05).max(NUMERICAL_FLOOR) * 0.5;
        let low = -half_hysteresis;
        let high = half_hysteresis;
        let mut armed = false;
        let mut last_nonpositive: Option<(f64, f64)> = None;
        let mut crossings = Vec::new();
        for (&time, value) in segment.times.iter().zip(values) {
            let centered = f64::from(*value) * scale - mean;
            if !centered.is_finite() {
                armed = false;
                last_nonpositive = None;
                continue;
            }
            if centered <= 0.0 {
                last_nonpositive = Some((time, centered));
            }
            if centered <= low {
                armed = true;
            }
            if armed && centered >= high {
                if let Some((left_time, left_value)) = last_nonpositive {
                    let denominator = centered - left_value;
                    if denominator > NUMERICAL_FLOOR {
                        let ratio = (-left_value / denominator).clamp(0.0, 1.0);
                        crossings.push(left_time + (time - left_time) * ratio);
                    }
                }
                armed = false;
                last_nonpositive = None;
            }
        }
        let mut periods = crossings
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .filter(|period| period.is_finite() && *period > NUMERICAL_FLOOR)
            .collect::<Vec<_>>();
        if periods.len() < MIN_FREQUENCY_PERIODS {
            continue;
        }
        let initial_median = median(&mut periods);
        periods.retain(|period| (*period - initial_median).abs() <= initial_median * 0.2);
        if periods.len() < MIN_FREQUENCY_PERIODS {
            continue;
        }
        let period = median(&mut periods);
        let mut deviations = periods
            .iter()
            .map(|value| (*value - period).abs())
            .collect::<Vec<_>>();
        let jitter_percent = median(&mut deviations) / period * 100.0;
        let estimate = FrequencyEstimate {
            hz: 1.0 / period,
            accepted_periods: periods.len(),
            jitter_percent,
        };
        if best
            .as_ref()
            .is_none_or(|current| estimate.accepted_periods > current.accepted_periods)
        {
            best = Some(estimate);
        }
    }

    let quality = MeasurementQuality {
        contains_gap: segments.len() > 1,
        insufficient_samples: best.is_none() && !any_valid_timebase,
        low_amplitude: any_valid_timebase && !any_amplitude,
        invalid_timebase: false,
        incomplete_channels: false,
    };
    (best, quality)
}

fn analyze_three_phase_power(
    segments: &[SampleBlock],
    spec: &ThreePhasePowerSpec,
    contains_gap: bool,
) -> Result<ThreePhasePower, MeasurementError> {
    validate_power_spec(segments, spec)?;
    let mut valid_samples = 0_usize;
    let mut active_sum = 0.0_f64;
    let mut voltage_square_sums = [0.0_f64; 3];
    let mut current_square_sums = [0.0_f64; 3];

    for segment in segments {
        for row in 0..segment.times.len() {
            let mut voltages = [0.0_f64; 3];
            let mut currents = [0.0_f64; 3];
            let mut finite = true;
            for phase in 0..3 {
                voltages[phase] = f64::from(segment.channels[spec.voltage_columns[phase]][row])
                    * spec.voltage_scales[phase];
                currents[phase] = f64::from(segment.channels[spec.current_columns[phase]][row])
                    * spec.current_scales[phase];
                finite &= voltages[phase].is_finite() && currents[phase].is_finite();
            }
            if !finite {
                continue;
            }
            valid_samples += 1;
            for phase in 0..3 {
                active_sum += voltages[phase] * currents[phase];
                voltage_square_sums[phase] += voltages[phase] * voltages[phase];
                current_square_sums[phase] += currents[phase] * currents[phase];
            }
        }
    }
    if valid_samples < 16 {
        return Err(MeasurementError::InvalidPowerConfig(
            "three-phase power needs at least 16 aligned finite samples".to_owned(),
        ));
    }

    let frequency = power_frequency(segments, spec).unwrap_or(spec.nominal_frequency_hz);
    if !frequency.is_finite() || frequency <= 0.0 {
        return Err(MeasurementError::InvalidPowerConfig(
            "three-phase power frequency must be positive".to_owned(),
        ));
    }
    let phasor_segment = segments
        .iter()
        .filter(|segment| segment.times.len() >= 16)
        .max_by_key(|segment| segment.times.len())
        .ok_or_else(|| {
            MeasurementError::InvalidPowerConfig(
                "three-phase power needs a contiguous segment".to_owned(),
            )
        })?;
    let mut complex_power = Complex::<f64>::new(0.0, 0.0);
    for phase in 0..3 {
        let voltage = fundamental_phasor(
            phasor_segment,
            spec.voltage_columns[phase],
            spec.voltage_scales[phase],
            frequency,
        )?;
        let current = fundamental_phasor(
            phasor_segment,
            spec.current_columns[phase],
            spec.current_scales[phase],
            frequency,
        )?;
        complex_power += voltage * current.conj();
    }

    let count = valid_samples as f64;
    let active_power = active_sum / count;
    let voltage_effective = (voltage_square_sums.iter().sum::<f64>() / count).sqrt();
    let current_effective = (current_square_sums.iter().sum::<f64>() / count).sqrt();
    let effective_apparent_power = voltage_effective * current_effective;
    let true_power_factor = (effective_apparent_power > NUMERICAL_FLOOR)
        .then_some((active_power / effective_apparent_power).clamp(-1.0, 1.0));
    let (voltage_factor, voltage_label) = engineering_unit_factor(&spec.voltage_unit, true);
    let (current_factor, current_label) = engineering_unit_factor(&spec.current_unit, false);
    let power_unit = if voltage_label.is_some() && current_label.is_some() {
        power_unit_label(voltage_factor * current_factor)
    } else {
        "engineering units".to_owned()
    };
    Ok(ThreePhasePower {
        valid_samples,
        active_power,
        fundamental_reactive_power: complex_power.im * 0.5,
        effective_apparent_power,
        true_power_factor,
        frequency_hz: frequency,
        power_unit,
        quality: MeasurementQuality {
            contains_gap,
            insufficient_samples: false,
            low_amplitude: effective_apparent_power <= NUMERICAL_FLOOR,
            invalid_timebase: false,
            incomplete_channels: false,
        },
    })
}

fn validate_power_spec(
    segments: &[SampleBlock],
    spec: &ThreePhasePowerSpec,
) -> Result<(), MeasurementError> {
    if !spec.nominal_frequency_hz.is_finite() || spec.nominal_frequency_hz <= 0.0 {
        return Err(MeasurementError::InvalidPowerConfig(
            "nominal frequency must be positive".to_owned(),
        ));
    }
    if spec
        .voltage_scales
        .iter()
        .chain(&spec.current_scales)
        .any(|scale| !scale.is_finite())
    {
        return Err(MeasurementError::InvalidScale);
    }
    let mut columns = spec.voltage_columns.to_vec();
    columns.extend(spec.current_columns);
    columns.sort_unstable();
    columns.dedup();
    if columns.len() != 6 {
        return Err(MeasurementError::InvalidPowerConfig(
            "voltage and current bindings must use six distinct columns".to_owned(),
        ));
    }
    for column in columns {
        ensure_column_exists(segments, column)?;
    }
    Ok(())
}

fn power_frequency(segments: &[SampleBlock], spec: &ThreePhasePowerSpec) -> Option<f64> {
    let mut estimates = spec
        .voltage_columns
        .iter()
        .zip(spec.voltage_scales)
        .filter_map(|(column, scale)| estimate_frequency(segments, *column, scale).0)
        .map(|estimate| estimate.hz)
        .collect::<Vec<_>>();
    if estimates.is_empty() {
        None
    } else {
        Some(median(&mut estimates))
    }
}

fn fundamental_phasor(
    segment: &SampleBlock,
    column: usize,
    scale: f64,
    frequency_hz: f64,
) -> Result<Complex<f64>, MeasurementError> {
    let values = segment
        .channels
        .get(column)
        .ok_or(MeasurementError::MissingColumn(column))?;
    let finite = values
        .iter()
        .copied()
        .map(|value| f64::from(value) * scale)
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if finite.len() < 16 {
        return Err(MeasurementError::InvalidPowerConfig(
            "fundamental phasor needs at least 16 finite samples".to_owned(),
        ));
    }
    let mean = finite.iter().sum::<f64>() / finite.len() as f64;
    let denominator = segment.times.len().saturating_sub(1).max(1) as f64;
    let mut sum = Complex::<f64>::new(0.0, 0.0);
    let mut window_sum = 0.0_f64;
    for (index, (&time, value)) in segment.times.iter().zip(values).enumerate() {
        let scaled = f64::from(*value) * scale;
        if !scaled.is_finite() {
            continue;
        }
        let window = 0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / denominator).cos();
        let angle = std::f64::consts::TAU * frequency_hz * time;
        let sample = (scaled - mean) * window;
        sum += Complex::from_polar(sample, -angle);
        window_sum += window;
    }
    if window_sum <= NUMERICAL_FLOOR {
        return Err(MeasurementError::InvalidPowerConfig(
            "fundamental phasor window is empty".to_owned(),
        ));
    }
    Ok(sum * (2.0 / window_sum))
}

fn engineering_unit_factor(unit: &str, voltage: bool) -> (f64, Option<&'static str>) {
    let normalized = unit.trim().to_ascii_lowercase();
    match (voltage, normalized.as_str()) {
        (true, "v") => (1.0, Some("V")),
        (true, "kv") => (1_000.0, Some("V")),
        (false, "a") => (1.0, Some("A")),
        (false, "ka") => (1_000.0, Some("A")),
        _ => (1.0, None),
    }
}

fn power_unit_label(factor: f64) -> String {
    if factor >= 1_000_000.0 {
        "MW / Mvar / MVA".to_owned()
    } else if factor >= 1_000.0 {
        "kW / kvar / kVA".to_owned()
    } else {
        "W / var / VA".to_owned()
    }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) * 0.5
    } else {
        values[middle]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_block(
        sample_rate: f64,
        frequency: f64,
        amplitude: f64,
        dc: f64,
        phase: f64,
        count: usize,
    ) -> SampleBlock {
        let times = (0..count)
            .map(|index| index as f64 / sample_rate)
            .collect::<Vec<_>>();
        let values = times
            .iter()
            .map(|time| {
                (dc + amplitude * (std::f64::consts::TAU * frequency * time + phase).sin()) as f32
            })
            .collect::<Vec<_>>();
        SampleBlock {
            times,
            channels: vec![values],
        }
    }

    #[test]
    fn reports_true_rms_peaks_and_frequency_for_offset_sine() {
        let block = sine_block(10_000.0, 50.0, 4.0, 3.0, 0.2, 4_000);
        let result = analyze_segments(
            &[block],
            &[ChannelMeasurementSpec::new(0, 0, "Voltage")],
            None,
        )
        .unwrap();
        let row = &result.channels[0];
        assert!((row.mean.unwrap() - 3.0).abs() < 1.0e-3);
        assert!((row.rms.unwrap() - (17.0_f64).sqrt()).abs() < 1.0e-3);
        assert!((row.peak_to_peak.unwrap() - 8.0).abs() < 1.0e-3);
        assert!((row.absolute_peak.unwrap() - 7.0).abs() < 1.0e-3);
        assert!((row.frequency.as_ref().unwrap().hz - 50.0).abs() < 0.01);
    }

    #[test]
    fn frequency_does_not_bridge_gap_segments() {
        let first = sine_block(1_000.0, 50.0, 1.0, 0.0, 0.0, 100);
        let mut second = sine_block(1_000.0, 50.0, 1.0, 0.0, 0.0, 200);
        for time in &mut second.times {
            *time += 10.0;
        }
        let result = analyze_segments(
            &[first, second],
            &[ChannelMeasurementSpec::new(0, 0, "A")],
            None,
        )
        .unwrap();
        let frequency = result.channels[0].frequency.as_ref().unwrap();
        assert!((frequency.hz - 50.0).abs() < 0.05);
        assert!(result.quality.contains_gap);
    }

    #[test]
    fn constant_signal_has_statistics_but_no_frequency() {
        let block = SampleBlock {
            times: (0..100).map(|index| index as f64 * 0.001).collect(),
            channels: vec![vec![2.0; 100]],
        };
        let result =
            analyze_segments(&[block], &[ChannelMeasurementSpec::new(0, 0, "DC")], None).unwrap();
        assert_eq!(result.channels[0].mean, Some(2.0));
        assert!(result.channels[0].frequency.is_none());
        assert!(result.channels[0].quality.low_amplitude);
    }

    #[test]
    fn rejects_unaligned_or_non_monotonic_segments() {
        let unaligned = SampleBlock {
            times: vec![0.0, 1.0],
            channels: vec![vec![1.0]],
        };
        assert_eq!(
            analyze_segments(&[unaligned], &[], None),
            Err(MeasurementError::UnalignedSegment)
        );
        let backwards = SampleBlock {
            times: vec![1.0, 0.0],
            channels: vec![vec![1.0, 2.0]],
        };
        assert_eq!(
            analyze_segments(&[backwards], &[], None),
            Err(MeasurementError::InvalidTimebase)
        );
    }

    #[test]
    fn balanced_three_phase_power_matches_known_values() {
        let sample_rate = 10_000.0;
        let frequency = 50.0;
        let count = 4_000;
        let voltage_peak = 325.269119;
        let current_peak = 14.142136;
        let lag = 30.0_f64.to_radians();
        let times = (0..count)
            .map(|index| index as f64 / sample_rate)
            .collect::<Vec<_>>();
        let phase = |offset: f64, lag: f64, peak: f64| {
            times
                .iter()
                .map(|time| {
                    (peak * (std::f64::consts::TAU * frequency * time + offset - lag).sin()) as f32
                })
                .collect::<Vec<_>>()
        };
        let channels = vec![
            phase(0.0, 0.0, voltage_peak),
            phase(-std::f64::consts::TAU / 3.0, 0.0, voltage_peak),
            phase(std::f64::consts::TAU / 3.0, 0.0, voltage_peak),
            phase(0.0, lag, current_peak),
            phase(-std::f64::consts::TAU / 3.0, lag, current_peak),
            phase(std::f64::consts::TAU / 3.0, lag, current_peak),
        ];
        let block = SampleBlock { times, channels };
        let power = ThreePhasePowerSpec::new([0, 1, 2], [3, 4, 5]);
        let result = analyze_segments(&[block], &[], Some(&power)).unwrap();
        let result = result.power.unwrap();
        let expected_s = 3.0 * 230.0 * 10.0;
        let expected_p = expected_s * lag.cos();
        let expected_q = expected_s * lag.sin();
        assert!((result.active_power - expected_p).abs() / expected_p < 0.01);
        assert!(
            (result.fundamental_reactive_power - expected_q).abs() / expected_q < 0.01,
            "Q={} expected={expected_q}",
            result.fundamental_reactive_power
        );
        assert!((result.effective_apparent_power - expected_s).abs() / expected_s < 0.01);
        assert!((result.true_power_factor.unwrap() - lag.cos()).abs() < 0.01);
    }

    #[test]
    fn power_rejects_duplicate_bindings() {
        let block = SampleBlock {
            times: (0..20).map(|index| index as f64 * 0.001).collect(),
            channels: vec![vec![1.0; 20]; 6],
        };
        let spec = ThreePhasePowerSpec::new([0, 1, 2], [2, 4, 5]);
        assert!(matches!(
            analyze_segments(&[block], &[], Some(&spec)),
            Err(MeasurementError::InvalidPowerConfig(_))
        ));
    }

    #[test]
    #[ignore = "performance gate: run explicitly with --ignored live_measurement_p95"]
    fn live_measurement_p95_stays_below_refresh_interval() {
        let sample_rate = 20_000.0;
        let sample_count = 131_072_usize;
        let times = (0..sample_count)
            .map(|index| index as f64 / sample_rate)
            .collect::<Vec<_>>();
        let channels = (0..6)
            .map(|channel| {
                let phase = std::f64::consts::TAU * (channel % 3) as f64 / 3.0;
                (0..sample_count)
                    .map(|index| {
                        let angle = std::f64::consts::TAU * 50.0 * index as f64 / sample_rate;
                        ((angle - phase).sin() * if channel < 3 { 325.0 } else { 14.14 }) as f32
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let block = SampleBlock { times, channels };
        let specs = (0..6)
            .map(|column| ChannelMeasurementSpec::new(column, column, format!("CH{column}")))
            .collect::<Vec<_>>();
        let power = ThreePhasePowerSpec::new([0, 1, 2], [3, 4, 5]);

        let mut durations = Vec::new();
        for _ in 0..6 {
            let started = std::time::Instant::now();
            let result = analyze_segments(std::slice::from_ref(&block), &specs, Some(&power))
                .expect("representative Live measurement succeeds");
            assert!(result.power.is_some());
            durations.push(started.elapsed());
        }
        durations.remove(0);
        durations.sort_unstable();
        let p95 = durations[durations.len() - 1];
        eprintln!("live measurement p95: {:.2} ms", p95.as_secs_f64() * 1000.0);
        assert!(
            p95 < std::time::Duration::from_millis(250),
            "Live measurement p95 {p95:?} exceeds the 250 ms refresh interval"
        );
    }
}
