use std::collections::{BTreeSet, VecDeque};

use thiserror::Error;

use super::protocol::DecodedSampleBatch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GapReason {
    SequenceLoss,
    SampleIndexLoss,
    HostBackpressure,
    DeviceReported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveGap {
    pub start_sample_index: u64,
    pub missing_samples: u64,
    pub reason: GapReason,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapshotSegment {
    pub times: Vec<f64>,
    pub channels: Vec<Vec<f32>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveSnapshot {
    pub channel_ids: Vec<u16>,
    pub segments: Vec<SnapshotSegment>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LiveBufferError {
    #[error("invalid live buffer configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid live sample batch: {0}")]
    InvalidBatch(String),
}

pub struct LiveBuffer {
    channel_ids: Vec<u16>,
    capacity: usize,
    tick_hz: u64,
    sample_indices: VecDeque<u64>,
    timestamps: VecDeque<u64>,
    channels: Vec<VecDeque<f32>>,
    gaps: VecDeque<LiveGap>,
}

impl LiveBuffer {
    pub fn new(
        channel_ids: Vec<u16>,
        capacity: usize,
        tick_hz: u64,
    ) -> Result<Self, LiveBufferError> {
        if channel_ids.is_empty() {
            return Err(LiveBufferError::InvalidConfig(
                "at least one channel is required".to_owned(),
            ));
        }
        if capacity == 0 {
            return Err(LiveBufferError::InvalidConfig(
                "capacity must be greater than zero".to_owned(),
            ));
        }
        if tick_hz == 0 {
            return Err(LiveBufferError::InvalidConfig(
                "tick_hz must be greater than zero".to_owned(),
            ));
        }
        let mut seen = BTreeSet::new();
        if channel_ids
            .iter()
            .any(|channel_id| !seen.insert(*channel_id))
        {
            return Err(LiveBufferError::InvalidConfig(
                "channel ids must be unique".to_owned(),
            ));
        }
        let channels = channel_ids
            .iter()
            .map(|_| VecDeque::with_capacity(capacity))
            .collect();
        Ok(Self {
            channel_ids,
            capacity,
            tick_hz,
            sample_indices: VecDeque::with_capacity(capacity),
            timestamps: VecDeque::with_capacity(capacity),
            channels,
            gaps: VecDeque::new(),
        })
    }

    pub fn len(&self) -> usize {
        self.sample_indices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sample_indices.is_empty()
    }

    pub fn channel_ids(&self) -> &[u16] {
        &self.channel_ids
    }

    pub fn sample_indices(&self) -> Vec<u64> {
        self.sample_indices.iter().copied().collect()
    }

    pub fn gaps(&self) -> Vec<LiveGap> {
        self.gaps.iter().copied().collect()
    }

    pub fn clear(&mut self) {
        self.sample_indices.clear();
        self.timestamps.clear();
        for channel in &mut self.channels {
            channel.clear();
        }
        self.gaps.clear();
    }

    pub fn push_gap(&mut self, start_sample_index: u64, missing_samples: u64, reason: GapReason) {
        if missing_samples == 0 {
            return;
        }
        if self.gaps.back().is_some_and(|gap| {
            gap.start_sample_index == start_sample_index && gap.missing_samples == missing_samples
        }) {
            return;
        }
        self.gaps.push_back(LiveGap {
            start_sample_index,
            missing_samples,
            reason,
        });
    }

    pub fn push_batch(&mut self, batch: DecodedSampleBatch) -> Result<(), LiveBufferError> {
        if batch.channel_ids != self.channel_ids {
            return Err(LiveBufferError::InvalidBatch(
                "channel ids do not match the live buffer".to_owned(),
            ));
        }
        if batch.channels.len() != self.channels.len() {
            return Err(LiveBufferError::InvalidBatch(
                "channel column count does not match".to_owned(),
            ));
        }
        let sample_count = batch.channels.first().map(Vec::len).unwrap_or(0);
        if sample_count == 0
            || batch
                .channels
                .iter()
                .any(|values| values.len() != sample_count)
        {
            return Err(LiveBufferError::InvalidBatch(
                "channel columns must be non-empty and aligned".to_owned(),
            ));
        }
        let last_sample_offset = u64::try_from(sample_count - 1).map_err(|_| {
            LiveBufferError::InvalidBatch("sample count does not fit u64".to_owned())
        })?;
        batch
            .first_sample_index
            .checked_add(last_sample_offset)
            .ok_or_else(|| LiveBufferError::InvalidBatch("sample index overflow".to_owned()))?;
        let last_tick_offset = u64::from(batch.sample_period_ticks)
            .checked_mul(last_sample_offset)
            .ok_or_else(|| LiveBufferError::InvalidBatch("timestamp offset overflow".to_owned()))?;
        batch
            .timestamp_ticks
            .checked_add(last_tick_offset)
            .ok_or_else(|| LiveBufferError::InvalidBatch("timestamp overflow".to_owned()))?;

        if let Some(previous) = self.sample_indices.back().copied() {
            let expected = previous.checked_add(1).ok_or_else(|| {
                LiveBufferError::InvalidBatch("previous sample index overflow".to_owned())
            })?;
            if batch.first_sample_index < expected {
                return Err(LiveBufferError::InvalidBatch(format!(
                    "out-of-order sample index {}, expected at least {expected}",
                    batch.first_sample_index
                )));
            }
            if batch.first_sample_index > expected {
                self.push_gap(
                    expected,
                    batch.first_sample_index - expected,
                    GapReason::SampleIndexLoss,
                );
            }
        }

        for sample_offset in 0..sample_count {
            self.sample_indices.push_back(
                batch.first_sample_index
                    + u64::try_from(sample_offset).expect("offset bounded by checked sample count"),
            );
            self.timestamps.push_back(
                batch.timestamp_ticks
                    + u64::from(batch.sample_period_ticks)
                        * u64::try_from(sample_offset)
                            .expect("offset bounded by checked sample count"),
            );
            for (target, source) in self.channels.iter_mut().zip(&batch.channels) {
                target.push_back(source[sample_offset]);
            }
        }
        self.evict_excess();
        Ok(())
    }

    pub fn snapshot(&self, max_points: usize) -> LiveSnapshot {
        if self.is_empty() || max_points == 0 {
            return LiveSnapshot {
                channel_ids: self.channel_ids.clone(),
                segments: Vec::new(),
            };
        }
        let mut ranges = Vec::new();
        let mut start = 0;
        for index in 1..self.len() {
            if self.sample_indices[index] != self.sample_indices[index - 1].saturating_add(1) {
                ranges.push(start..index);
                start = index;
            }
        }
        ranges.push(start..self.len());
        if ranges.len() > max_points {
            ranges = ranges.split_off(ranges.len() - max_points);
        }
        let mut remaining_budget = max_points;
        let mut remaining_samples = ranges.iter().map(std::ops::Range::len).sum::<usize>();
        let range_count = ranges.len();
        let mut segments = Vec::with_capacity(range_count);
        for (index, range) in ranges.into_iter().enumerate() {
            let remaining_ranges = range_count - index;
            let reserved_for_others = remaining_ranges.saturating_sub(1);
            let proportional = range
                .len()
                .saturating_mul(remaining_budget)
                .div_ceil(remaining_samples.max(1));
            let budget = proportional
                .max(1)
                .min(remaining_budget.saturating_sub(reserved_for_others).max(1));
            remaining_budget = remaining_budget.saturating_sub(budget);
            remaining_samples = remaining_samples.saturating_sub(range.len());
            segments.push(self.snapshot_range(range, budget));
        }
        LiveSnapshot {
            channel_ids: self.channel_ids.clone(),
            segments,
        }
    }

    /// Returns the newest samples at full resolution while preserving gaps.
    pub fn snapshot_recent(&self, max_samples: usize) -> LiveSnapshot {
        if self.is_empty() || max_samples == 0 {
            return LiveSnapshot {
                channel_ids: self.channel_ids.clone(),
                segments: Vec::new(),
            };
        }
        let start = self.len().saturating_sub(max_samples);
        let mut ranges = Vec::new();
        let mut range_start = start;
        for index in start.saturating_add(1)..self.len() {
            if self.sample_indices[index] != self.sample_indices[index - 1].saturating_add(1) {
                ranges.push(range_start..index);
                range_start = index;
            }
        }
        ranges.push(range_start..self.len());
        LiveSnapshot {
            channel_ids: self.channel_ids.clone(),
            segments: ranges
                .into_iter()
                .map(|range| {
                    let count = range.len();
                    self.snapshot_range(range, count)
                })
                .collect(),
        }
    }

    fn evict_excess(&mut self) {
        while self.len() > self.capacity {
            self.sample_indices.pop_front();
            self.timestamps.pop_front();
            for channel in &mut self.channels {
                channel.pop_front();
            }
        }
        if let Some(&oldest) = self.sample_indices.front() {
            while self.gaps.front().is_some_and(|gap| {
                gap.start_sample_index.saturating_add(gap.missing_samples) <= oldest
            }) {
                self.gaps.pop_front();
            }
        }
    }

    fn snapshot_range(&self, range: std::ops::Range<usize>, max_points: usize) -> SnapshotSegment {
        let selected = if range.len() <= max_points {
            range.clone().collect::<Vec<_>>()
        } else {
            let bin_count = (max_points / 2).max(1);
            let bin_size = range.len().div_ceil(bin_count);
            let mut selected = BTreeSet::new();
            for bin_start in (range.start..range.end).step_by(bin_size) {
                let bin_end = (bin_start + bin_size).min(range.end);
                selected.insert(bin_start);
                selected.insert(bin_end - 1);
                for channel in &self.channels {
                    let mut minimum = bin_start;
                    let mut maximum = bin_start;
                    for index in bin_start + 1..bin_end {
                        if channel[index].total_cmp(&channel[minimum]).is_lt() {
                            minimum = index;
                        }
                        if channel[index].total_cmp(&channel[maximum]).is_gt() {
                            maximum = index;
                        }
                    }
                    selected.insert(minimum);
                    selected.insert(maximum);
                }
            }
            selected.into_iter().collect()
        };
        let selected = self.limit_selected_to_budget(selected, &range, max_points);
        SnapshotSegment {
            times: selected
                .iter()
                .map(|&index| self.timestamps[index] as f64 / self.tick_hz as f64)
                .collect(),
            channels: self
                .channels
                .iter()
                .map(|channel| selected.iter().map(|&index| channel[index]).collect())
                .collect(),
        }
    }

    fn limit_selected_to_budget(
        &self,
        selected: Vec<usize>,
        range: &std::ops::Range<usize>,
        max_points: usize,
    ) -> Vec<usize> {
        if selected.len() <= max_points {
            return selected;
        }
        let mut kept = BTreeSet::new();
        if max_points == 1 {
            return vec![range.end - 1];
        }
        kept.insert(range.start);
        kept.insert(range.end - 1);
        for channel in &self.channels {
            if kept.len() >= max_points {
                break;
            }
            let mut minimum = range.start;
            let mut maximum = range.start;
            for index in range.start + 1..range.end {
                if channel[index].total_cmp(&channel[minimum]).is_lt() {
                    minimum = index;
                }
                if channel[index].total_cmp(&channel[maximum]).is_gt() {
                    maximum = index;
                }
            }
            kept.insert(minimum);
            if kept.len() < max_points {
                kept.insert(maximum);
            }
        }
        if kept.len() < max_points {
            let remaining = max_points - kept.len();
            for offset in 0..remaining {
                let position = (offset + 1) * (selected.len() - 1) / (remaining + 1);
                kept.insert(selected[position]);
            }
        }
        kept.into_iter().take(max_points).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::protocol::DecodedSampleBatch;

    fn batch(first: u64, tick: u64, values: &[f32]) -> DecodedSampleBatch {
        DecodedSampleBatch {
            revision: 1,
            first_sample_index: first,
            sample_period_ticks: 10,
            timestamp_ticks: tick,
            channel_ids: vec![0],
            channels: vec![values.to_vec()],
            raw_frame: Vec::new(),
        }
    }

    #[test]
    fn buffer_keeps_aligned_capacity_and_gap() {
        let mut buffer = LiveBuffer::new(vec![0], 5, 1_000_000).unwrap();

        buffer.push_batch(batch(10, 100, &[1.0, 2.0, 3.0])).unwrap();
        buffer.push_gap(13, 2, GapReason::SequenceLoss);
        buffer.push_batch(batch(15, 150, &[4.0, 5.0])).unwrap();

        assert_eq!(buffer.len(), 5);
        assert_eq!(buffer.sample_indices(), vec![10, 11, 12, 15, 16]);
        assert_eq!(buffer.gaps()[0].start_sample_index, 13);
        assert_eq!(buffer.gaps()[0].missing_samples, 2);
    }

    #[test]
    fn buffer_evicts_oldest_samples_and_snapshot_preserves_spike() {
        let mut buffer = LiveBuffer::new(vec![0], 5, 1_000_000).unwrap();
        buffer
            .push_batch(batch(0, 0, &[0.0, 0.0, 10.0, 0.0, 0.0, 0.0]))
            .unwrap();

        assert_eq!(buffer.sample_indices(), vec![1, 2, 3, 4, 5]);
        let snapshot = buffer.snapshot(4);
        assert!(snapshot.segments.iter().any(|segment| {
            segment
                .channels
                .first()
                .is_some_and(|values| values.contains(&10.0))
        }));
    }

    #[test]
    fn buffer_rejects_misaligned_batch_without_partial_mutation() {
        let mut buffer = LiveBuffer::new(vec![0, 1], 5, 1_000_000).unwrap();
        let malformed = DecodedSampleBatch {
            revision: 1,
            first_sample_index: 0,
            sample_period_ticks: 1,
            timestamp_ticks: 0,
            channel_ids: vec![0, 1],
            channels: vec![vec![1.0, 2.0], vec![3.0]],
            raw_frame: Vec::new(),
        };

        assert!(matches!(
            buffer.push_batch(malformed),
            Err(LiveBufferError::InvalidBatch(_))
        ));
        assert!(buffer.is_empty());
    }

    #[test]
    fn snapshot_budget_is_shared_across_gap_segments() {
        let mut buffer = LiveBuffer::new(vec![0], 20, 1_000_000).unwrap();
        buffer
            .push_batch(batch(0, 0, &[0.0, 1.0, 2.0, 3.0, 4.0]))
            .unwrap();
        buffer.push_gap(5, 5, GapReason::SampleIndexLoss);
        buffer
            .push_batch(batch(10, 100, &[5.0, 6.0, 7.0, 8.0, 9.0]))
            .unwrap();

        let snapshot = buffer.snapshot(4);

        assert_eq!(snapshot.segments.len(), 2);
        assert!(snapshot
            .segments
            .iter()
            .all(|segment| !segment.times.is_empty()));
        assert!(
            snapshot
                .segments
                .iter()
                .map(|segment| segment.times.len())
                .sum::<usize>()
                <= 4
        );
    }

    #[test]
    fn recent_snapshot_keeps_exact_tail_and_gap_boundaries() {
        let mut buffer = LiveBuffer::new(vec![0], 20, 1_000_000).unwrap();
        buffer
            .push_batch(batch(0, 0, &[0.0, 1.0, 2.0, 3.0, 4.0]))
            .unwrap();
        buffer.push_gap(5, 5, GapReason::SampleIndexLoss);
        buffer
            .push_batch(batch(10, 100, &[5.0, 6.0, 7.0, 8.0, 9.0]))
            .unwrap();

        let snapshot = buffer.snapshot_recent(7);

        assert_eq!(snapshot.segments.len(), 2);
        assert_eq!(snapshot.segments[0].channels[0], vec![3.0, 4.0]);
        assert_eq!(
            snapshot.segments[1].channels[0],
            vec![5.0, 6.0, 7.0, 8.0, 9.0]
        );
    }
}
