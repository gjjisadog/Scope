use std::collections::VecDeque;

use thiserror::Error;

use super::protocol::DecodedSampleBatch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerMode {
    Auto,
    Normal,
    Single,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerEdge {
    Rising,
    Falling,
    Either,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TriggerConfig {
    pub mode: TriggerMode,
    pub edge: TriggerEdge,
    pub source_channel: u16,
    pub level: f32,
    pub hysteresis: f32,
    pub pre_samples: usize,
    pub post_samples: usize,
    pub auto_timeout_samples: usize,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            mode: TriggerMode::Auto,
            edge: TriggerEdge::Rising,
            source_channel: 0,
            level: 0.0,
            hysteresis: 0.0,
            pre_samples: 100,
            post_samples: 300,
            auto_timeout_samples: 5_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TriggerCapture {
    pub channel_ids: Vec<u16>,
    pub sample_indices: Vec<u64>,
    pub timestamps: Vec<u64>,
    pub channels: Vec<Vec<f32>>,
    pub trigger_position: usize,
    pub auto_timeout: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TriggerError {
    #[error("invalid trigger configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid trigger sample batch: {0}")]
    InvalidBatch(String),
}

#[derive(Clone)]
struct SamplePoint {
    sample_index: u64,
    timestamp: u64,
    values: Vec<f32>,
}

struct CaptureBuilder {
    channel_ids: Vec<u16>,
    points: Vec<SamplePoint>,
    trigger_position: usize,
    remaining_post: usize,
    auto_timeout: bool,
}

pub struct TriggerEngine {
    config: TriggerConfig,
    armed: bool,
    previous_source: Option<f32>,
    pre_history: VecDeque<SamplePoint>,
    active_capture: Option<CaptureBuilder>,
    samples_since_arm: usize,
}

impl TriggerEngine {
    pub fn new(config: TriggerConfig) -> Result<Self, TriggerError> {
        validate_config(&config)?;
        Ok(Self {
            config,
            armed: true,
            previous_source: None,
            pre_history: VecDeque::new(),
            active_capture: None,
            samples_since_arm: 0,
        })
    }

    pub fn config(&self) -> &TriggerConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: TriggerConfig) -> Result<(), TriggerError> {
        validate_config(&config)?;
        self.config = config;
        self.arm();
        Ok(())
    }

    pub fn arm(&mut self) {
        self.armed = true;
        self.reset_history();
    }

    pub fn disarm(&mut self) {
        self.armed = false;
        self.reset_history();
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }

    pub fn on_gap(&mut self) {
        self.reset_history();
    }

    pub fn feed(
        &mut self,
        batch: &DecodedSampleBatch,
    ) -> Result<Option<TriggerCapture>, TriggerError> {
        if !self.armed {
            return Ok(None);
        }
        let source_position = batch
            .channel_ids
            .iter()
            .position(|channel_id| *channel_id == self.config.source_channel)
            .ok_or_else(|| {
                TriggerError::InvalidBatch(format!(
                    "trigger source channel {} is not present",
                    self.config.source_channel
                ))
            })?;
        if batch.channels.len() != batch.channel_ids.len() {
            return Err(TriggerError::InvalidBatch(
                "channel id and column counts differ".to_owned(),
            ));
        }
        let sample_count = batch.channels.first().map(Vec::len).unwrap_or(0);
        if sample_count == 0
            || batch
                .channels
                .iter()
                .any(|values| values.len() != sample_count)
        {
            return Err(TriggerError::InvalidBatch(
                "channel columns must be non-empty and aligned".to_owned(),
            ));
        }
        let last_offset = u64::try_from(sample_count - 1)
            .map_err(|_| TriggerError::InvalidBatch("sample count overflow".to_owned()))?;
        batch
            .first_sample_index
            .checked_add(last_offset)
            .ok_or_else(|| TriggerError::InvalidBatch("sample index overflow".to_owned()))?;
        batch
            .timestamp_ticks
            .checked_add(
                u64::from(batch.sample_period_ticks)
                    .checked_mul(last_offset)
                    .ok_or_else(|| {
                        TriggerError::InvalidBatch("timestamp offset overflow".to_owned())
                    })?,
            )
            .ok_or_else(|| TriggerError::InvalidBatch("timestamp overflow".to_owned()))?;

        let mut first_capture = None;
        for sample_offset in 0..sample_count {
            let offset = u64::try_from(sample_offset)
                .map_err(|_| TriggerError::InvalidBatch("sample offset overflow".to_owned()))?;
            let point = SamplePoint {
                sample_index: batch.first_sample_index + offset,
                timestamp: batch.timestamp_ticks + u64::from(batch.sample_period_ticks) * offset,
                values: batch
                    .channels
                    .iter()
                    .map(|channel| channel[sample_offset])
                    .collect(),
            };
            if let Some(capture) = self.push_point(point, source_position, &batch.channel_ids)? {
                if first_capture.is_none() {
                    first_capture = Some(capture);
                }
                if !self.armed {
                    break;
                }
            }
        }
        Ok(first_capture)
    }

    fn push_point(
        &mut self,
        point: SamplePoint,
        source_position: usize,
        channel_ids: &[u16],
    ) -> Result<Option<TriggerCapture>, TriggerError> {
        if let Some(builder) = &mut self.active_capture {
            builder.points.push(point);
            builder.remaining_post = builder.remaining_post.saturating_sub(1);
            if builder.remaining_post == 0 {
                return Ok(Some(self.finish_capture()));
            }
            return Ok(None);
        }

        let source = point.values[source_position];
        if !source.is_finite() {
            self.previous_source = None;
            self.push_pre_history(point);
            return Ok(None);
        }
        self.samples_since_arm = self.samples_since_arm.saturating_add(1);
        let edge_hit = self
            .previous_source
            .is_some_and(|previous| self.crossed(previous, source));
        let auto_timeout = self.config.mode == TriggerMode::Auto
            && self.samples_since_arm >= self.config.auto_timeout_samples;
        self.previous_source = Some(source);
        if edge_hit || auto_timeout {
            let mut points = self.pre_history.drain(..).collect::<Vec<_>>();
            let trigger_position = points.len();
            points.push(point);
            self.active_capture = Some(CaptureBuilder {
                channel_ids: channel_ids.to_vec(),
                points,
                trigger_position,
                remaining_post: self.config.post_samples,
                auto_timeout: !edge_hit && auto_timeout,
            });
            if self.config.post_samples == 0 {
                return Ok(Some(self.finish_capture()));
            }
            return Ok(None);
        }
        self.push_pre_history(point);
        Ok(None)
    }

    fn crossed(&self, previous: f32, current: f32) -> bool {
        let half_hysteresis = self.config.hysteresis * 0.5;
        let low = self.config.level - half_hysteresis;
        let high = self.config.level + half_hysteresis;
        let rising = previous <= low && current >= high;
        let falling = previous >= high && current <= low;
        match self.config.edge {
            TriggerEdge::Rising => rising,
            TriggerEdge::Falling => falling,
            TriggerEdge::Either => rising || falling,
        }
    }

    fn push_pre_history(&mut self, point: SamplePoint) {
        self.pre_history.push_back(point);
        while self.pre_history.len() > self.config.pre_samples {
            self.pre_history.pop_front();
        }
    }

    fn finish_capture(&mut self) -> TriggerCapture {
        let builder = self
            .active_capture
            .take()
            .expect("finish_capture requires an active capture");
        let channel_count = builder.channel_ids.len();
        let mut channels = (0..channel_count)
            .map(|_| Vec::with_capacity(builder.points.len()))
            .collect::<Vec<_>>();
        let mut sample_indices = Vec::with_capacity(builder.points.len());
        let mut timestamps = Vec::with_capacity(builder.points.len());
        for point in builder.points {
            sample_indices.push(point.sample_index);
            timestamps.push(point.timestamp);
            for (channel, value) in channels.iter_mut().zip(point.values) {
                channel.push(value);
            }
        }
        if self.config.mode == TriggerMode::Single {
            self.armed = false;
        }
        self.reset_history();
        TriggerCapture {
            channel_ids: builder.channel_ids,
            sample_indices,
            timestamps,
            channels,
            trigger_position: builder.trigger_position,
            auto_timeout: builder.auto_timeout,
        }
    }

    fn reset_history(&mut self) {
        self.previous_source = None;
        self.pre_history.clear();
        self.active_capture = None;
        self.samples_since_arm = 0;
    }
}

fn validate_config(config: &TriggerConfig) -> Result<(), TriggerError> {
    if !config.level.is_finite() {
        return Err(TriggerError::InvalidConfig(
            "level must be finite".to_owned(),
        ));
    }
    if !config.hysteresis.is_finite() || config.hysteresis < 0.0 {
        return Err(TriggerError::InvalidConfig(
            "hysteresis must be finite and non-negative".to_owned(),
        ));
    }
    config
        .pre_samples
        .checked_add(config.post_samples)
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| TriggerError::InvalidConfig("capture length overflow".to_owned()))?;
    if config.mode == TriggerMode::Auto && config.auto_timeout_samples == 0 {
        return Err(TriggerError::InvalidConfig(
            "auto timeout must be greater than zero".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::protocol::DecodedSampleBatch;

    fn batch(first: u64, values: &[f32]) -> DecodedSampleBatch {
        DecodedSampleBatch {
            revision: 1,
            first_sample_index: first,
            sample_period_ticks: 1,
            timestamp_ticks: first,
            channel_ids: vec![0],
            channels: vec![values.to_vec()],
            raw_frame: Vec::new(),
        }
    }

    fn config(mode: TriggerMode) -> TriggerConfig {
        TriggerConfig {
            mode,
            edge: TriggerEdge::Rising,
            source_channel: 0,
            level: 0.0,
            hysteresis: 0.2,
            pre_samples: 2,
            post_samples: 1,
            auto_timeout_samples: 100,
        }
    }

    #[test]
    fn single_rising_trigger_captures_pre_and_post_samples_then_disarms() {
        let mut trigger = TriggerEngine::new(config(TriggerMode::Single)).unwrap();

        let capture = trigger
            .feed(&batch(0, &[-1.0, -0.2, 0.2, 1.0]))
            .unwrap()
            .unwrap();

        assert_eq!(capture.sample_indices, vec![0, 1, 2, 3]);
        assert_eq!(capture.trigger_position, 2);
        assert_eq!(capture.channels[0], vec![-1.0, -0.2, 0.2, 1.0]);
        assert!(!capture.auto_timeout);
        assert!(!trigger.is_armed());
    }

    #[test]
    fn gap_resets_edge_history() {
        let mut trigger = TriggerEngine::new(config(TriggerMode::Normal)).unwrap();
        assert!(trigger.feed(&batch(0, &[-1.0])).unwrap().is_none());

        trigger.on_gap();

        assert!(trigger.feed(&batch(2, &[0.2, 1.0])).unwrap().is_none());
    }

    #[test]
    fn auto_mode_emits_timeout_capture() {
        let mut trigger = TriggerEngine::new(TriggerConfig {
            post_samples: 0,
            pre_samples: 1,
            auto_timeout_samples: 3,
            ..config(TriggerMode::Auto)
        })
        .unwrap();

        let capture = trigger.feed(&batch(0, &[1.0, 1.0, 1.0])).unwrap().unwrap();

        assert!(capture.auto_timeout);
        assert_eq!(capture.trigger_position, 1);
    }

    #[test]
    fn falling_edge_honors_hysteresis_and_normal_mode_stays_armed() {
        let mut trigger = TriggerEngine::new(TriggerConfig {
            mode: TriggerMode::Normal,
            edge: TriggerEdge::Falling,
            pre_samples: 1,
            post_samples: 0,
            ..config(TriggerMode::Normal)
        })
        .unwrap();

        let capture = trigger
            .feed(&batch(0, &[1.0, 0.05, -0.05, 0.2, -0.2]))
            .unwrap()
            .unwrap();

        assert_eq!(capture.sample_indices, vec![3, 4]);
        assert_eq!(capture.trigger_position, 1);
        assert!(trigger.is_armed());
    }

    #[test]
    fn either_edge_accepts_a_falling_crossing() {
        let mut trigger = TriggerEngine::new(TriggerConfig {
            edge: TriggerEdge::Either,
            pre_samples: 0,
            post_samples: 0,
            ..config(TriggerMode::Single)
        })
        .unwrap();

        let capture = trigger.feed(&batch(0, &[1.0, -1.0])).unwrap().unwrap();

        assert_eq!(capture.sample_indices, vec![1]);
    }

    #[test]
    fn normal_trigger_keeps_remainder_of_a_batch_as_next_pretrigger_history() {
        let mut trigger = TriggerEngine::new(TriggerConfig {
            mode: TriggerMode::Normal,
            pre_samples: 2,
            post_samples: 0,
            ..config(TriggerMode::Normal)
        })
        .unwrap();

        let first = trigger.feed(&batch(0, &[-1.0, 1.0, 5.0, 6.0])).unwrap();
        let second = trigger.feed(&batch(4, &[-1.0, 1.0])).unwrap().unwrap();

        assert!(first.is_some());
        assert_eq!(second.sample_indices, vec![3, 4, 5]);
        assert_eq!(second.channels[0], vec![6.0, -1.0, 1.0]);
        assert_eq!(second.trigger_position, 2);
    }
}
