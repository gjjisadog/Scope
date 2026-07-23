use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleOperator {
    LessEqual,
    GreaterEqual,
    BetweenInclusive,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleSeverity {
    Info,
    #[default]
    Warning,
    Error,
    Critical,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleInputKind {
    Source,
    Derived,
    #[default]
    Measurement,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RuleWindow {
    Absolute { start: f64, end: f64 },
    EventRelative { event: String, start: f64, end: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleSpec {
    pub id: String,
    pub metric: String,
    pub operator: RuleOperator,
    pub lower: f64,
    pub upper: Option<f64>,
    #[serde(default)]
    pub tolerance: f64,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub severity: RuleSeverity,
    #[serde(default)]
    pub input: RuleInputKind,
    #[serde(default)]
    pub window: Option<RuleWindow>,
}

impl RuleSpec {
    pub fn less_equal(metric: impl Into<String>, limit: f64) -> Self {
        let metric = metric.into();
        Self {
            id: metric.clone(),
            metric,
            operator: RuleOperator::LessEqual,
            lower: limit,
            upper: None,
            tolerance: 0.0,
            duration_seconds: None,
            severity: RuleSeverity::default(),
            input: RuleInputKind::default(),
            window: None,
        }
    }

    pub fn greater_equal(metric: impl Into<String>, limit: f64) -> Self {
        let metric = metric.into();
        Self {
            id: metric.clone(),
            metric,
            operator: RuleOperator::GreaterEqual,
            lower: limit,
            upper: None,
            tolerance: 0.0,
            duration_seconds: None,
            severity: RuleSeverity::default(),
            input: RuleInputKind::default(),
            window: None,
        }
    }

    pub fn between(metric: impl Into<String>, lower: f64, upper: f64) -> Self {
        let metric = metric.into();
        Self {
            id: metric.clone(),
            metric,
            operator: RuleOperator::BetweenInclusive,
            lower,
            upper: Some(upper),
            tolerance: 0.0,
            duration_seconds: None,
            severity: RuleSeverity::default(),
            input: RuleInputKind::default(),
            window: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleStatus {
    Passed,
    Failed,
    MissingMetric,
    NonFiniteMetric,
    Invalid,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleEvidence {
    pub window_start: Option<f64>,
    pub window_end: Option<f64>,
    pub sample_count: usize,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleOutcome {
    pub id: String,
    pub metric: String,
    pub observed: Option<f64>,
    pub status: RuleStatus,
    pub severity: RuleSeverity,
    pub input: RuleInputKind,
    #[serde(default)]
    pub evidence: Option<RuleEvidence>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleEvaluation {
    pub passed: bool,
    pub outcomes: Vec<RuleOutcome>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RuleError {
    #[error("rule id and metric must not be empty")]
    EmptyName,
    #[error("rule {0} contains non-finite limits")]
    NonFiniteLimit(String),
    #[error("rule {0} has an invalid inclusive range")]
    InvalidRange(String),
}

pub fn evaluate(
    rules: &[RuleSpec],
    metrics: &BTreeMap<String, f64>,
) -> Result<RuleEvaluation, RuleError> {
    let mut outcomes = Vec::with_capacity(rules.len());
    for rule in rules {
        validate_rule(rule)?;
        let observed = metrics.get(&rule.metric).copied();
        let status = match observed {
            None => RuleStatus::MissingMetric,
            Some(value) if !value.is_finite() => RuleStatus::NonFiniteMetric,
            Some(_value) if rule.window.is_some() || rule.duration_seconds.is_some() => {
                RuleStatus::Invalid
            }
            Some(value) => status_for_value(rule, value),
        };
        outcomes.push(RuleOutcome {
            id: rule.id.clone(),
            metric: rule.metric.clone(),
            observed,
            status,
            severity: rule.severity,
            input: rule.input,
            evidence: observed.map(|value| RuleEvidence {
                window_start: None,
                window_end: None,
                sample_count: 1,
                min: value.is_finite().then_some(value),
                max: value.is_finite().then_some(value),
            }),
        });
    }
    Ok(RuleEvaluation {
        passed: outcomes
            .iter()
            .all(|outcome| outcome.status == RuleStatus::Passed),
        outcomes,
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricSample {
    pub time: f64,
    pub value: f64,
}

/// Evaluates rules against timestamped metric samples and preserves the evidence window.
pub fn evaluate_series(
    rules: &[RuleSpec],
    metrics: &BTreeMap<String, Vec<MetricSample>>,
    events: &BTreeMap<String, f64>,
) -> Result<RuleEvaluation, RuleError> {
    let mut outcomes = Vec::with_capacity(rules.len());
    for rule in rules {
        validate_rule(rule)?;
        let samples = match metrics.get(&rule.metric) {
            None => {
                outcomes.push(RuleOutcome {
                    id: rule.id.clone(),
                    metric: rule.metric.clone(),
                    observed: None,
                    status: RuleStatus::MissingMetric,
                    severity: rule.severity,
                    input: rule.input,
                    evidence: None,
                });
                continue;
            }
            Some(samples) => samples,
        };
        let (window_start, window_end) = match &rule.window {
            None => (None, None),
            Some(RuleWindow::Absolute { start, end }) => (Some(*start), Some(*end)),
            Some(RuleWindow::EventRelative { event, start, end }) => {
                let Some(event_time) = events.get(event).copied() else {
                    outcomes.push(RuleOutcome {
                        id: rule.id.clone(),
                        metric: rule.metric.clone(),
                        observed: None,
                        status: RuleStatus::Invalid,
                        severity: rule.severity,
                        input: rule.input,
                        evidence: None,
                    });
                    continue;
                };
                (Some(event_time + start), Some(event_time + end))
            }
        };
        let sample_is_in_window = |time: f64| {
            time.is_finite()
                && window_start.is_none_or(|start| time >= start)
                && window_end.is_none_or(|end| time <= end)
        };
        let invalid_timestamp_order = samples.windows(2).any(|pair| {
            pair[0].time.is_finite() && pair[1].time.is_finite() && pair[1].time <= pair[0].time
        });
        let invalid_sample_in_window = samples.iter().any(|sample| {
            !sample.time.is_finite()
                || (sample_is_in_window(sample.time) && !sample.value.is_finite())
        });
        if invalid_timestamp_order || invalid_sample_in_window {
            outcomes.push(RuleOutcome {
                id: rule.id.clone(),
                metric: rule.metric.clone(),
                observed: None,
                status: RuleStatus::Invalid,
                severity: rule.severity,
                input: rule.input,
                evidence: None,
            });
            continue;
        }
        let selected = samples
            .iter()
            .filter(|sample| sample_is_in_window(sample.time) && sample.value.is_finite())
            .collect::<Vec<_>>();
        if selected.is_empty() {
            outcomes.push(RuleOutcome {
                id: rule.id.clone(),
                metric: rule.metric.clone(),
                observed: None,
                status: RuleStatus::Invalid,
                severity: rule.severity,
                input: rule.input,
                evidence: None,
            });
            continue;
        }
        let passing = selected
            .iter()
            .all(|sample| status_for_value(rule, sample.value) == RuleStatus::Passed);
        let duration_ok = rule.duration_seconds.is_none_or(|duration| {
            selected
                .last()
                .zip(selected.first())
                .is_some_and(|(last, first)| last.time - first.time >= duration)
        });
        let min = selected
            .iter()
            .map(|sample| sample.value)
            .fold(f64::INFINITY, f64::min);
        let max = selected
            .iter()
            .map(|sample| sample.value)
            .fold(f64::NEG_INFINITY, f64::max);
        let observed = selected.last().map(|sample| sample.value);
        outcomes.push(RuleOutcome {
            id: rule.id.clone(),
            metric: rule.metric.clone(),
            observed,
            status: if passing && duration_ok {
                RuleStatus::Passed
            } else {
                RuleStatus::Failed
            },
            severity: rule.severity,
            input: rule.input,
            evidence: Some(RuleEvidence {
                window_start,
                window_end,
                sample_count: selected.len(),
                min: Some(min),
                max: Some(max),
            }),
        });
    }
    Ok(RuleEvaluation {
        passed: outcomes
            .iter()
            .all(|outcome| outcome.status == RuleStatus::Passed),
        outcomes,
    })
}

fn status_for_value(rule: &RuleSpec, value: f64) -> RuleStatus {
    let pass = match rule.operator {
        RuleOperator::LessEqual => value <= rule.lower + rule.tolerance,
        RuleOperator::GreaterEqual => value >= rule.lower - rule.tolerance,
        RuleOperator::BetweenInclusive => {
            value >= rule.lower - rule.tolerance
                && value <= rule.upper.expect("validated range") + rule.tolerance
        }
    };
    if pass {
        RuleStatus::Passed
    } else {
        RuleStatus::Failed
    }
}

fn validate_rule(rule: &RuleSpec) -> Result<(), RuleError> {
    if rule.id.trim().is_empty() || rule.metric.trim().is_empty() {
        return Err(RuleError::EmptyName);
    }
    if !rule.lower.is_finite() || rule.upper.is_some_and(|upper| !upper.is_finite()) {
        return Err(RuleError::NonFiniteLimit(rule.id.clone()));
    }
    if !rule.tolerance.is_finite() || rule.tolerance < 0.0 {
        return Err(RuleError::NonFiniteLimit(rule.id.clone()));
    }
    if rule
        .duration_seconds
        .is_some_and(|duration| !duration.is_finite() || duration < 0.0)
    {
        return Err(RuleError::InvalidRange(rule.id.clone()));
    }
    if let Some(window) = &rule.window {
        let (start, end) = match window {
            RuleWindow::Absolute { start, end } | RuleWindow::EventRelative { start, end, .. } => {
                (*start, *end)
            }
        };
        if !start.is_finite() || !end.is_finite() || end < start {
            return Err(RuleError::InvalidRange(rule.id.clone()));
        }
        if let RuleWindow::EventRelative { event, .. } = window {
            if event.trim().is_empty() {
                return Err(RuleError::EmptyName);
            }
        }
    }
    if rule.operator == RuleOperator::BetweenInclusive
        && rule.upper.is_none_or(|upper| upper < rule.lower)
    {
        return Err(RuleError::InvalidRange(rule.id.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_all_rules_deterministically() {
        let rules = vec![
            RuleSpec::less_equal("rms", 0.1),
            RuleSpec::greater_equal("frequency", 49.0),
            RuleSpec::between("frequency", 49.0, 51.0),
        ];
        let metrics = BTreeMap::from([("frequency".to_owned(), 50.0), ("rms".to_owned(), 0.05)]);
        let result = evaluate(&rules, &metrics).unwrap();
        assert!(result.passed);
        assert!(result
            .outcomes
            .iter()
            .all(|outcome| outcome.status == RuleStatus::Passed));
    }

    #[test]
    fn reports_missing_and_non_finite_metrics_as_failures() {
        let rules = vec![RuleSpec::less_equal("missing", 1.0)];
        let result = evaluate(&rules, &BTreeMap::new()).unwrap();
        assert!(!result.passed);
        assert_eq!(result.outcomes[0].status, RuleStatus::MissingMetric);

        let metrics = BTreeMap::from([("rms".to_owned(), f64::NAN)]);
        let rules = vec![RuleSpec::less_equal("rms", 1.0)];
        let result = evaluate(&rules, &metrics).unwrap();
        assert_eq!(result.outcomes[0].status, RuleStatus::NonFiniteMetric);
    }

    #[test]
    fn evaluates_window_duration_and_preserves_evidence() {
        let mut rule = RuleSpec::less_equal("rms", 0.1);
        rule.window = Some(RuleWindow::Absolute {
            start: 1.0,
            end: 3.0,
        });
        rule.duration_seconds = Some(1.0);
        rule.severity = RuleSeverity::Error;
        let metrics = BTreeMap::from([(
            "rms".to_owned(),
            vec![
                MetricSample {
                    time: 0.0,
                    value: 0.2,
                },
                MetricSample {
                    time: 1.0,
                    value: 0.05,
                },
                MetricSample {
                    time: 2.0,
                    value: 0.08,
                },
            ],
        )]);
        let result = evaluate_series(&[rule], &metrics, &BTreeMap::new()).unwrap();
        assert!(result.passed);
        assert_eq!(result.outcomes[0].severity, RuleSeverity::Error);
        assert_eq!(
            result.outcomes[0].evidence.as_ref().unwrap().sample_count,
            2
        );
    }

    #[test]
    fn scalar_window_evaluation_is_explicitly_invalid() {
        let mut rule = RuleSpec::less_equal("rms", 0.1);
        rule.window = Some(RuleWindow::EventRelative {
            event: "trigger".to_owned(),
            start: 0.0,
            end: 1.0,
        });
        let metrics = BTreeMap::from([("rms".to_owned(), 0.05)]);
        let result = evaluate(&[rule], &metrics).unwrap();
        assert_eq!(result.outcomes[0].status, RuleStatus::Invalid);
    }

    #[test]
    fn non_finite_series_sample_cannot_be_filtered_into_pass() {
        let rule = RuleSpec::less_equal("rms", 0.1);
        let metrics = BTreeMap::from([(
            "rms".to_owned(),
            vec![
                MetricSample {
                    time: 0.0,
                    value: 0.05,
                },
                MetricSample {
                    time: 1.0,
                    value: f64::NAN,
                },
            ],
        )]);

        let result = evaluate_series(&[rule], &metrics, &BTreeMap::new()).unwrap();

        assert!(!result.passed);
        assert_eq!(result.outcomes[0].status, RuleStatus::Invalid);
    }

    #[test]
    fn non_finite_sample_outside_window_does_not_poison_valid_window() {
        let mut rule = RuleSpec::less_equal("rms", 0.1);
        rule.window = Some(RuleWindow::Absolute {
            start: 1.0,
            end: 2.0,
        });
        let metrics = BTreeMap::from([(
            "rms".to_owned(),
            vec![
                MetricSample {
                    time: 0.0,
                    value: f64::NAN,
                },
                MetricSample {
                    time: 1.0,
                    value: 0.05,
                },
            ],
        )]);

        let result = evaluate_series(&[rule], &metrics, &BTreeMap::new()).unwrap();

        assert!(result.passed);
        assert_eq!(result.outcomes[0].status, RuleStatus::Passed);
    }
}
