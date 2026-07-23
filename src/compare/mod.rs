use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CompareError {
    #[error("series segment times and values have different lengths: {times} != {values}")]
    LengthMismatch { times: usize, values: usize },
    #[error("series segment must contain at least one sample")]
    EmptySegment,
    #[error("series segment contains non-finite data or non-increasing timestamps")]
    InvalidSegment,
    #[error("alignment values must be finite")]
    InvalidAlignment,
    #[error("comparison tolerance must contain a finite non-negative absolute or relative limit")]
    InvalidTolerance,
    #[error("relative error floor must be finite and positive")]
    InvalidRelativeFloor,
    #[error("reference and test series do not overlap")]
    NoOverlap,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeriesSegment {
    pub times: Vec<f64>,
    pub values: Vec<f64>,
}

impl SeriesSegment {
    pub fn new(times: Vec<f64>, values: Vec<f64>) -> Result<Self, CompareError> {
        if times.len() != values.len() {
            return Err(CompareError::LengthMismatch {
                times: times.len(),
                values: values.len(),
            });
        }
        if times.is_empty() {
            return Err(CompareError::EmptySegment);
        }
        if times.iter().any(|time| !time.is_finite())
            || values.iter().any(|value| !value.is_finite())
            || times.windows(2).any(|pair| pair[1] <= pair[0])
        {
            return Err(CompareError::InvalidSegment);
        }
        Ok(Self { times, values })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Series {
    segments: Vec<SeriesSegment>,
}

impl Series {
    pub fn new(segments: Vec<SeriesSegment>) -> Result<Self, CompareError> {
        let mut previous_end = None;
        for segment in &segments {
            if segment.times.is_empty() {
                return Err(CompareError::EmptySegment);
            }
            if segment.times.len() != segment.values.len()
                || segment.times.iter().any(|time| !time.is_finite())
                || segment.values.iter().any(|value| !value.is_finite())
                || segment.times.windows(2).any(|pair| pair[1] <= pair[0])
            {
                return Err(CompareError::InvalidSegment);
            }
            if previous_end.is_some_and(|end| segment.times[0] <= end) {
                return Err(CompareError::InvalidSegment);
            }
            previous_end = segment.times.last().copied();
        }
        Ok(Self { segments })
    }

    pub fn segments(&self) -> &[SeriesSegment] {
        &self.segments
    }

    pub fn sample_at(&self, time: f64) -> Option<f64> {
        if !time.is_finite() {
            return None;
        }
        self.segments.iter().find_map(|segment| {
            let first = *segment.times.first()?;
            let last = *segment.times.last()?;
            if time < first || time > last {
                return None;
            }
            match segment
                .times
                .binary_search_by(|candidate| candidate.total_cmp(&time))
            {
                Ok(index) => segment.values.get(index).copied(),
                Err(index) if index > 0 && index < segment.times.len() => {
                    let left_time = segment.times[index - 1];
                    let right_time = segment.times[index];
                    let left_value = segment.values[index - 1];
                    let right_value = segment.values[index];
                    let ratio = (time - left_time) / (right_time - left_time);
                    Some(left_value + (right_value - left_value) * ratio)
                }
                _ => None,
            }
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AlignmentSpec {
    ManualOffset {
        seconds: f64,
    },
    Anchor {
        reference_time: f64,
        test_time: f64,
    },
    TriggerPoint {
        reference_time: f64,
        test_time: f64,
        confidence: f64,
    },
    ThresholdEvent {
        reference_time: f64,
        test_time: f64,
        confidence: f64,
    },
    FundamentalPhase {
        reference_phase_radians: f64,
        test_phase_radians: f64,
        period_seconds: f64,
        confidence: f64,
    },
}

impl Default for AlignmentSpec {
    fn default() -> Self {
        Self::ManualOffset { seconds: 0.0 }
    }
}

impl AlignmentSpec {
    pub fn offset_seconds(self) -> Result<f64, CompareError> {
        let offset = match self {
            Self::ManualOffset { seconds } => seconds,
            Self::Anchor {
                reference_time,
                test_time,
            } => reference_time - test_time,
            Self::TriggerPoint {
                reference_time,
                test_time,
                confidence,
            }
            | Self::ThresholdEvent {
                reference_time,
                test_time,
                confidence,
            } => {
                validate_confidence(confidence)?;
                reference_time - test_time
            }
            Self::FundamentalPhase {
                reference_phase_radians,
                test_phase_radians,
                period_seconds,
                confidence,
            } => {
                validate_confidence(confidence)?;
                if !period_seconds.is_finite() || period_seconds <= 0.0 {
                    return Err(CompareError::InvalidAlignment);
                }
                let phase_delta = wrap_phase(reference_phase_radians - test_phase_radians);
                phase_delta / std::f64::consts::TAU * period_seconds
            }
        };
        offset
            .is_finite()
            .then_some(offset)
            .ok_or(CompareError::InvalidAlignment)
    }

    pub fn confidence(self) -> Result<f64, CompareError> {
        let confidence = match self {
            Self::ManualOffset { .. } | Self::Anchor { .. } => 1.0,
            Self::TriggerPoint { confidence, .. }
            | Self::ThresholdEvent { confidence, .. }
            | Self::FundamentalPhase { confidence, .. } => confidence,
        };
        validate_confidence(confidence)?;
        Ok(confidence)
    }
}

fn validate_confidence(confidence: f64) -> Result<(), CompareError> {
    if confidence.is_finite() && (0.0..=1.0).contains(&confidence) {
        Ok(())
    } else {
        Err(CompareError::InvalidAlignment)
    }
}

fn wrap_phase(value: f64) -> f64 {
    let wrapped =
        (value + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
    if wrapped == -std::f64::consts::PI && value > 0.0 {
        std::f64::consts::PI
    } else {
        wrapped
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tolerance {
    pub absolute: Option<f64>,
    pub relative: Option<f64>,
}

impl Tolerance {
    pub fn absolute(value: f64) -> Self {
        Self {
            absolute: Some(value),
            relative: None,
        }
    }

    fn validate(&self) -> Result<(), CompareError> {
        let valid = self
            .absolute
            .into_iter()
            .chain(self.relative)
            .any(|value| value.is_finite() && value >= 0.0);
        if valid
            && self
                .absolute
                .into_iter()
                .chain(self.relative)
                .all(|value| value.is_finite() && value >= 0.0)
        {
            Ok(())
        } else {
            Err(CompareError::InvalidTolerance)
        }
    }

    fn exceeded(&self, absolute_error: f64, relative_error: Option<f64>) -> bool {
        self.absolute.is_some_and(|limit| absolute_error > limit)
            || self
                .relative
                .zip(relative_error)
                .is_some_and(|(limit, error)| error > limit)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompareRequest {
    pub reference: Series,
    pub test: Series,
    pub alignment: AlignmentSpec,
    pub tolerance: Option<Tolerance>,
    pub relative_floor: f64,
}

impl CompareRequest {
    pub fn new(reference: Series, test: Series) -> Self {
        Self {
            reference,
            test,
            alignment: AlignmentSpec::default(),
            tolerance: None,
            relative_floor: 1.0e-12,
        }
    }

    fn validate(&self) -> Result<f64, CompareError> {
        if !self.relative_floor.is_finite() || self.relative_floor <= 0.0 {
            return Err(CompareError::InvalidRelativeFloor);
        }
        if let Some(tolerance) = &self.tolerance {
            tolerance.validate()?;
        }
        self.alignment.offset_seconds()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComparePoint {
    pub time: f64,
    pub reference: f64,
    pub test: Option<f64>,
    pub error: Option<f64>,
    pub absolute_error: Option<f64>,
    pub relative_error: Option<f64>,
    pub valid: bool,
    pub exceeds_tolerance: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompareInterval {
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompareSummary {
    pub valid_points: usize,
    pub invalid_points: usize,
    pub rms_error: f64,
    pub max_absolute_error: f64,
    pub max_relative_error: f64,
    pub exceedance_intervals: Vec<CompareInterval>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompareResult {
    pub alignment_offset_seconds: f64,
    #[serde(default = "default_alignment_confidence")]
    pub alignment_confidence: f64,
    pub points: Vec<ComparePoint>,
    pub summary: CompareSummary,
}

/// The stable comparison evidence payload shared by GUI and exported reports.
///
/// Keeping this projection separate from the point-by-point result makes it
/// possible for every output format to carry the same bounded, deterministic
/// summary without duplicating formatting or silently dropping gap quality.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareEvidence {
    pub alignment_offset_seconds: f64,
    pub alignment_confidence: f64,
    pub valid_points: usize,
    pub invalid_points: usize,
    pub rms_error: f64,
    pub max_absolute_error: f64,
    pub max_relative_error: f64,
    pub exceedance_intervals: Vec<CompareInterval>,
}

impl CompareResult {
    pub fn evidence(&self) -> CompareEvidence {
        CompareEvidence {
            alignment_offset_seconds: self.alignment_offset_seconds,
            alignment_confidence: self.alignment_confidence,
            valid_points: self.summary.valid_points,
            invalid_points: self.summary.invalid_points,
            rms_error: self.summary.rms_error,
            max_absolute_error: self.summary.max_absolute_error,
            max_relative_error: self.summary.max_relative_error,
            exceedance_intervals: self.summary.exceedance_intervals.clone(),
        }
    }

    /// A deterministic, human-readable line used by GUI and binary exports.
    pub fn evidence_line(&self) -> String {
        let evidence = self.evidence();
        format!(
            "Compare: offset={:.6}s confidence={:.3} valid={} invalid={} rms={:.6} maxAbs={:.6} maxRel={:.3}% exceedances={}",
            evidence.alignment_offset_seconds,
            evidence.alignment_confidence,
            evidence.valid_points,
            evidence.invalid_points,
            evidence.rms_error,
            evidence.max_absolute_error,
            evidence.max_relative_error * 100.0,
            evidence.exceedance_intervals.len(),
        )
    }
}

fn default_alignment_confidence() -> f64 {
    1.0
}

pub fn compare(request: CompareRequest) -> Result<CompareResult, CompareError> {
    let offset = request.validate()?;
    let mut points = Vec::new();
    let mut sum_squared_error = 0.0;
    let mut max_absolute_error = 0.0_f64;
    let mut max_relative_error = 0.0_f64;
    let mut valid_points = 0;
    let mut invalid_points = 0;
    let mut exceedance_intervals = Vec::new();
    let mut open_exceedance: Option<CompareInterval> = None;

    for segment in request.reference.segments() {
        for (&time, &reference) in segment.times.iter().zip(&segment.values) {
            let test = request.test.sample_at(time - offset);
            let (error, absolute_error, relative_error, exceeds_tolerance) =
                if let Some(test) = test {
                    let error = test - reference;
                    let absolute_error = error.abs();
                    let denominator = reference.abs().max(request.relative_floor);
                    let relative_error = (denominator.is_finite() && denominator > 0.0)
                        .then_some(absolute_error / denominator)
                        .filter(|value| value.is_finite());
                    let exceeds_tolerance = request.tolerance.as_ref().is_some_and(|tolerance| {
                        tolerance.exceeded(absolute_error, relative_error)
                    });
                    valid_points += 1;
                    sum_squared_error += error * error;
                    max_absolute_error = max_absolute_error.max(absolute_error);
                    if let Some(relative_error) = relative_error {
                        max_relative_error = max_relative_error.max(relative_error);
                    }
                    (
                        Some(error),
                        Some(absolute_error),
                        relative_error,
                        exceeds_tolerance,
                    )
                } else {
                    invalid_points += 1;
                    (None, None, None, false)
                };

            if exceeds_tolerance {
                match &mut open_exceedance {
                    Some(interval) => interval.end = time,
                    None => {
                        open_exceedance = Some(CompareInterval {
                            start: time,
                            end: time,
                        })
                    }
                }
            } else if let Some(interval) = open_exceedance.take() {
                exceedance_intervals.push(interval);
            }

            points.push(ComparePoint {
                time,
                reference,
                test,
                error,
                absolute_error,
                relative_error,
                valid: test.is_some(),
                exceeds_tolerance,
            });
        }
        if let Some(interval) = open_exceedance.take() {
            exceedance_intervals.push(interval);
        }
    }

    if valid_points == 0 {
        return Err(CompareError::NoOverlap);
    }
    Ok(CompareResult {
        alignment_offset_seconds: offset,
        alignment_confidence: request.alignment.confidence()?,
        points,
        summary: CompareSummary {
            valid_points,
            invalid_points,
            rms_error: (sum_squared_error / valid_points as f64).sqrt(),
            max_absolute_error,
            max_relative_error,
            exceedance_intervals,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_rejects_mismatched_time_and_value_lengths() {
        let result = SeriesSegment::new(vec![0.0, 1.0], vec![1.0]);
        assert!(matches!(result, Err(CompareError::LengthMismatch { .. })));
    }

    #[test]
    fn series_preserves_empty_gap_free_segments_only() {
        let series = Series::new(vec![
            SeriesSegment::new(vec![0.0, 1.0], vec![1.0, 2.0]).unwrap()
        ])
        .unwrap();
        assert_eq!(series.segments().len(), 1);
    }

    #[test]
    fn series_rejects_deserialized_empty_or_invalid_segments() {
        assert_eq!(
            Series::new(vec![SeriesSegment {
                times: Vec::new(),
                values: Vec::new(),
            }]),
            Err(CompareError::EmptySegment)
        );
        assert_eq!(
            Series::new(vec![SeriesSegment {
                times: vec![0.0, 0.0],
                values: vec![1.0, 2.0],
            }]),
            Err(CompareError::InvalidSegment)
        );
        assert_eq!(
            Series::new(vec![
                segment(&[(0.0, 1.0), (1.0, 2.0)]),
                segment(&[(0.5, 3.0), (2.0, 4.0)]),
            ]),
            Err(CompareError::InvalidSegment)
        );
    }

    fn segment(points: &[(f64, f64)]) -> SeriesSegment {
        SeriesSegment::new(
            points.iter().map(|(time, _)| *time).collect(),
            points.iter().map(|(_, value)| *value).collect(),
        )
        .unwrap()
    }

    fn test_series(points: &[(f64, f64)]) -> Series {
        Series::new(vec![segment(points)]).unwrap()
    }

    #[test]
    fn interpolation_uses_linear_value_inside_a_segment() {
        let series = test_series(&[(0.0, 0.0), (1.0, 10.0)]);
        assert_eq!(series.sample_at(0.25), Some(2.5));
    }

    #[test]
    fn interpolation_does_not_bridge_a_gap() {
        let series = Series::new(vec![
            segment(&[(0.0, 0.0), (1.0, 1.0)]),
            segment(&[(3.0, 3.0), (4.0, 4.0)]),
        ])
        .unwrap();
        assert_eq!(series.sample_at(2.0), None);
    }

    #[test]
    fn different_sample_rates_use_explicit_timestamp_resampling() {
        let result = compare(CompareRequest::new(
            test_series(&[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]),
            test_series(&[
                (0.0, 0.0),
                (0.25, 0.25),
                (0.5, 0.5),
                (0.75, 0.75),
                (1.0, 1.0),
            ]),
        ))
        .unwrap();
        assert_eq!(result.summary.valid_points, 3);
        assert_eq!(result.summary.invalid_points, 0);
        assert!(result.summary.rms_error.abs() < 1.0e-12);
    }

    #[test]
    fn anchor_alignment_shifts_test_into_reference_time() {
        let alignment = AlignmentSpec::Anchor {
            reference_time: 5.0,
            test_time: 3.0,
        };
        assert_eq!(alignment.offset_seconds().unwrap(), 2.0);
    }

    #[test]
    fn trigger_and_threshold_alignment_report_confidence() {
        let trigger = AlignmentSpec::TriggerPoint {
            reference_time: 5.0,
            test_time: 3.0,
            confidence: 0.8,
        };
        assert_eq!(trigger.offset_seconds().unwrap(), 2.0);
        assert_eq!(trigger.confidence().unwrap(), 0.8);

        let threshold = AlignmentSpec::ThresholdEvent {
            reference_time: 5.0,
            test_time: 3.0,
            confidence: 0.7,
        };
        assert_eq!(threshold.offset_seconds().unwrap(), 2.0);
        assert_eq!(threshold.confidence().unwrap(), 0.7);
    }

    #[test]
    fn fundamental_phase_alignment_uses_shortest_phase_delta() {
        let alignment = AlignmentSpec::FundamentalPhase {
            reference_phase_radians: 0.25 * std::f64::consts::PI,
            test_phase_radians: -0.25 * std::f64::consts::PI,
            period_seconds: 0.02,
            confidence: 0.9,
        };
        assert!((alignment.offset_seconds().unwrap() - 0.005).abs() < 1.0e-12);
        assert_eq!(alignment.confidence().unwrap(), 0.9);
    }

    #[test]
    fn alignment_rejects_invalid_confidence_and_period() {
        assert!(matches!(
            AlignmentSpec::TriggerPoint {
                reference_time: 0.0,
                test_time: 0.0,
                confidence: 1.1,
            }
            .offset_seconds(),
            Err(CompareError::InvalidAlignment)
        ));
        assert!(matches!(
            AlignmentSpec::FundamentalPhase {
                reference_phase_radians: 0.0,
                test_phase_radians: 0.0,
                period_seconds: 0.0,
                confidence: 1.0,
            }
            .offset_seconds(),
            Err(CompareError::InvalidAlignment)
        ));
    }

    #[test]
    fn compare_reports_absolute_and_relative_error() {
        let result = compare(CompareRequest::new(
            test_series(&[(0.0, 10.0), (1.0, 10.0)]),
            test_series(&[(0.0, 12.0), (1.0, 12.0)]),
        ))
        .unwrap();
        assert_eq!(result.summary.valid_points, 2);
        assert!((result.summary.rms_error - 2.0).abs() < 1.0e-12);
        assert!((result.summary.max_relative_error - 0.2).abs() < 1.0e-12);
    }

    #[test]
    fn compare_marks_gap_as_invalid_and_does_not_create_exceedance() {
        let request = CompareRequest::new(
            test_series(&[(0.0, 1.0), (2.0, 1.0), (4.0, 1.0)]),
            Series::new(vec![
                segment(&[(0.0, 1.0), (1.0, 1.0)]),
                segment(&[(3.0, 1.0), (4.0, 1.0)]),
            ])
            .unwrap(),
        );
        let result = compare(request).unwrap();
        assert!(result.points.iter().any(|point| !point.valid));
    }

    #[test]
    fn tolerance_exceedance_is_closed_into_intervals() {
        let mut request = CompareRequest::new(
            test_series(&[(0.0, 0.0), (1.0, 0.0), (2.0, 0.0)]),
            test_series(&[(0.0, 0.0), (1.0, 2.0), (2.0, 0.0)]),
        );
        request.tolerance = Some(Tolerance::absolute(1.0));
        let result = compare(request).unwrap();
        assert_eq!(
            result.summary.exceedance_intervals,
            vec![CompareInterval {
                start: 1.0,
                end: 1.0
            }]
        );
    }

    #[test]
    fn evidence_projection_and_line_preserve_gap_quality() {
        let request = CompareRequest::new(
            test_series(&[(0.0, 1.0), (1.0, 1.0), (2.0, 1.0)]),
            Series::new(vec![segment(&[(0.0, 1.0)]), segment(&[(2.0, 1.0)])]).unwrap(),
        );
        let result = compare(request).unwrap();
        let evidence = result.evidence();
        assert_eq!(evidence.valid_points, 2);
        assert_eq!(evidence.invalid_points, 1);
        assert!(result.evidence_line().contains("invalid=1"));
        assert_eq!(
            serde_json::to_value(evidence)
                .unwrap()
                .get("invalidPoints")
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
    }

    #[test]
    fn evidence_line_uses_each_summary_metric_once() {
        let result = compare(CompareRequest {
            reference: test_series(&[(0.0, 10.0), (1.0, 10.0)]),
            test: test_series(&[(0.0, 10.1), (1.0, 10.1)]),
            alignment: AlignmentSpec::default(),
            tolerance: Some(Tolerance::absolute(0.05)),
            relative_floor: 1.0e-12,
        })
        .unwrap();
        assert_eq!(
            result.evidence_line(),
            "Compare: offset=0.000000s confidence=1.000 valid=2 invalid=0 rms=0.100000 maxAbs=0.100000 maxRel=1.000% exceedances=1"
        );
    }
}
