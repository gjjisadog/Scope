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

struct CaptureBuilder {
    channel_ids: Vec<u16>,
    sample_indices: Vec<u64>,
    timestamps: Vec<u64>,
    channels: Vec<Vec<f32>>,
    trigger_position: usize,
    remaining_post: usize,
    auto_timeout: bool,
}

/// Fixed-column pre-trigger storage. Values are appended directly from the
/// decoded batch, so arming a trigger does not allocate one `Vec<f32>` per
/// sample.
struct SampleHistory {
    channel_ids: Vec<u16>,
    capacity: usize,
    sample_indices: VecDeque<u64>,
    timestamps: VecDeque<u64>,
    channels: Vec<VecDeque<f32>>,
}

impl SampleHistory {
    fn clear(&mut self) {
        self.sample_indices.clear();
        self.timestamps.clear();
        for channel in &mut self.channels {
            channel.clear();
        }
    }

    fn prepare(&mut self, channel_ids: &[u16], capacity: usize) {
        if self.channel_ids == channel_ids && self.capacity == capacity {
            return;
        }
        self.channel_ids = channel_ids.to_vec();
        self.capacity = capacity;
        self.sample_indices = VecDeque::with_capacity(capacity);
        self.timestamps = VecDeque::with_capacity(capacity);
        self.channels = channel_ids
            .iter()
            .map(|_| VecDeque::with_capacity(capacity))
            .collect();
    }

    fn push(&mut self, batch: &DecodedSampleBatch, sample_offset: usize, capacity: usize) {
        self.sample_indices.push_back(
            batch.first_sample_index
                + u64::try_from(sample_offset).expect("sample offset was validated"),
        );
        self.timestamps.push_back(
            batch.timestamp_ticks
                + u64::from(batch.sample_period_ticks)
                    * u64::try_from(sample_offset).expect("sample offset was validated"),
        );
        for (history, source) in self.channels.iter_mut().zip(&batch.channels) {
            history.push_back(source[sample_offset]);
        }
        while self.sample_indices.len() > capacity {
            self.sample_indices.pop_front();
            self.timestamps.pop_front();
            for channel in &mut self.channels {
                channel.pop_front();
            }
        }
    }

    fn drain_capture_columns(&mut self) -> (Vec<u64>, Vec<u64>, Vec<Vec<f32>>) {
        (
            self.sample_indices.drain(..).collect(),
            self.timestamps.drain(..).collect(),
            self.channels
                .iter_mut()
                .map(|channel| channel.drain(..).collect())
                .collect(),
        )
    }
}

pub struct TriggerEngine {
    config: TriggerConfig,
    armed: bool,
    edge_latch: Option<TriggerEdge>,
    pre_history: SampleHistory,
    active_capture: Option<CaptureBuilder>,
    samples_since_arm: usize,
}

impl TriggerEngine {
    pub fn new(config: TriggerConfig) -> Result<Self, TriggerError> {
        validate_config(&config)?;
        Ok(Self {
            config,
            armed: true,
            edge_latch: None,
            pre_history: SampleHistory {
                channel_ids: Vec::new(),
                capacity: 0,
                sample_indices: VecDeque::new(),
                timestamps: VecDeque::new(),
                channels: Vec::new(),
            },
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
        Ok(self.feed_all(batch)?.into_iter().next())
    }

    pub fn feed_all(
        &mut self,
        batch: &DecodedSampleBatch,
    ) -> Result<Vec<TriggerCapture>, TriggerError> {
        if !self.armed {
            return Ok(Vec::new());
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

        self.pre_history
            .prepare(&batch.channel_ids, self.config.pre_samples);
        let mut captures = Vec::new();
        for sample_offset in 0..sample_count {
            if let Some(capture) = self.push_sample(batch, sample_offset, source_position)? {
                captures.push(capture);
                if !self.armed {
                    break;
                }
            }
        }
        Ok(captures)
    }

    fn push_sample(
        &mut self,
        batch: &DecodedSampleBatch,
        sample_offset: usize,
        source_position: usize,
    ) -> Result<Option<TriggerCapture>, TriggerError> {
        if let Some(builder) = &mut self.active_capture {
            append_sample(builder, batch, sample_offset);
            builder.remaining_post = builder.remaining_post.saturating_sub(1);
            if builder.remaining_post == 0 {
                return Ok(Some(self.finish_capture()));
            }
            return Ok(None);
        }

        let source = batch.channels[source_position][sample_offset];
        if !source.is_finite() {
            self.edge_latch = None;
            self.push_pre_history(batch, sample_offset);
            return Ok(None);
        }
        self.samples_since_arm = self.samples_since_arm.saturating_add(1);
        let edge_hit = self.detect_edge(source);
        let auto_timeout = self.config.mode == TriggerMode::Auto
            && self.samples_since_arm >= self.config.auto_timeout_samples;
        if edge_hit || auto_timeout {
            let trigger_position = self.pre_history.sample_indices.len();
            let (sample_indices, timestamps, channels) = self.pre_history.drain_capture_columns();
            let mut builder = CaptureBuilder {
                channel_ids: batch.channel_ids.clone(),
                sample_indices,
                timestamps,
                channels,
                trigger_position,
                remaining_post: self.config.post_samples,
                auto_timeout: !edge_hit && auto_timeout,
            };
            append_sample(&mut builder, batch, sample_offset);
            self.active_capture = Some(builder);
            if self.config.post_samples == 0 {
                return Ok(Some(self.finish_capture()));
            }
            return Ok(None);
        }
        self.push_pre_history(batch, sample_offset);
        Ok(None)
    }

    fn detect_edge(&mut self, current: f32) -> bool {
        let half_hysteresis = self.config.hysteresis * 0.5;
        let low = self.config.level - half_hysteresis;
        let high = self.config.level + half_hysteresis;
        match self.config.edge {
            TriggerEdge::Rising => {
                if current <= low {
                    self.edge_latch = Some(TriggerEdge::Rising);
                    false
                } else if current >= high && self.edge_latch == Some(TriggerEdge::Rising) {
                    self.edge_latch = None;
                    true
                } else {
                    false
                }
            }
            TriggerEdge::Falling => {
                if current >= high {
                    self.edge_latch = Some(TriggerEdge::Falling);
                    false
                } else if current <= low && self.edge_latch == Some(TriggerEdge::Falling) {
                    self.edge_latch = None;
                    true
                } else {
                    false
                }
            }
            TriggerEdge::Either => {
                if current <= low {
                    let hit = self.edge_latch == Some(TriggerEdge::Falling);
                    self.edge_latch = Some(TriggerEdge::Rising);
                    hit
                } else if current >= high {
                    let hit = self.edge_latch == Some(TriggerEdge::Rising);
                    self.edge_latch = Some(TriggerEdge::Falling);
                    hit
                } else {
                    false
                }
            }
        }
    }

    fn push_pre_history(&mut self, batch: &DecodedSampleBatch, sample_offset: usize) {
        self.pre_history
            .push(batch, sample_offset, self.config.pre_samples);
    }

    fn finish_capture(&mut self) -> TriggerCapture {
        let builder = self
            .active_capture
            .take()
            .expect("finish_capture requires an active capture");
        if self.config.mode == TriggerMode::Single {
            self.armed = false;
        }
        self.reset_history();
        TriggerCapture {
            channel_ids: builder.channel_ids,
            sample_indices: builder.sample_indices,
            timestamps: builder.timestamps,
            channels: builder.channels,
            trigger_position: builder.trigger_position,
            auto_timeout: builder.auto_timeout,
        }
    }

    fn reset_history(&mut self) {
        self.edge_latch = None;
        self.pre_history.clear();
        self.active_capture = None;
        self.samples_since_arm = 0;
    }
}

fn append_sample(builder: &mut CaptureBuilder, batch: &DecodedSampleBatch, sample_offset: usize) {
    let offset = u64::try_from(sample_offset).expect("sample offset was validated");
    builder
        .sample_indices
        .push(batch.first_sample_index.saturating_add(offset));
    builder.timestamps.push(
        batch
            .timestamp_ticks
            .saturating_add(u64::from(batch.sample_period_ticks).saturating_mul(offset)),
    );
    for (target, source) in builder.channels.iter_mut().zip(&batch.channels) {
        target.push(source[sample_offset]);
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
    fn feed_all_returns_every_normal_capture_completed_in_one_batch() {
        let mut trigger = TriggerEngine::new(TriggerConfig {
            mode: TriggerMode::Normal,
            pre_samples: 0,
            post_samples: 0,
            ..config(TriggerMode::Normal)
        })
        .unwrap();

        let captures = trigger
            .feed_all(&batch(0, &[-1.0, 1.0, -1.0, 1.0]))
            .unwrap();

        assert_eq!(captures.len(), 2);
        assert_eq!(captures[0].sample_indices, vec![1]);
        assert_eq!(captures[1].sample_indices, vec![3]);
    }

    #[test]
    fn gradual_rising_signal_crosses_a_nonzero_hysteresis_band() {
        let mut trigger = TriggerEngine::new(TriggerConfig {
            mode: TriggerMode::Single,
            pre_samples: 4,
            post_samples: 0,
            ..config(TriggerMode::Single)
        })
        .unwrap();

        let capture = trigger
            .feed(&batch(0, &[-1.0, -0.05, 0.0, 0.05, 0.2]))
            .unwrap()
            .unwrap();

        assert_eq!(capture.sample_indices, vec![0, 1, 2, 3, 4]);
        assert_eq!(capture.trigger_position, 4);
        assert!(!capture.auto_timeout);
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
