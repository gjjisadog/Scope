use std::sync::Arc;

use super::{
    ChannelMeta, DataError, DataResult, DataSource, DatasetMeta, RangeSummary, SampleBlock,
};

pub const CHANNEL_UNIT_ANALOG: &str = "__scope_kind_analog";
pub const CHANNEL_UNIT_DIGITAL: &str = "__scope_kind_digital";

struct Segment {
    source: Arc<dyn DataSource>,
    start: usize,
    len: usize,
}

pub struct CombinedDataSource {
    meta: DatasetMeta,
    segments: Vec<Segment>,
}

impl CombinedDataSource {
    pub fn new(source_name: String, parts: Vec<(Arc<dyn DataSource>, bool)>) -> DataResult<Self> {
        if parts.is_empty() {
            return Err(DataError::NoChannels);
        }

        let mut channels = Vec::new();
        let mut segments = Vec::new();
        let mut start_time = f64::INFINITY;
        let mut end_time = f64::NEG_INFINITY;
        let mut sample_count = 0_u64;
        let mut sample_rate = 0.0_f64;

        for (source, is_digital) in parts {
            let meta = source.metadata();
            let start = channels.len();
            let len = meta.channels.len();
            let part_start_time = meta.start_time;
            let part_end_time = meta.end_time;
            let part_sample_count = meta.sample_count;
            let part_sample_rate = meta.nominal_sample_rate_hz;
            let unit = if is_digital {
                CHANNEL_UNIT_DIGITAL
            } else {
                CHANNEL_UNIT_ANALOG
            };
            for channel in &meta.channels {
                channels.push(ChannelMeta {
                    index: channels.len(),
                    name: channel.name.clone(),
                    unit: unit.to_owned(),
                    sample_rate_hz: channel.sample_rate_hz,
                    scale: channel.scale,
                    default_visible: channel.default_visible && !is_digital,
                });
            }
            segments.push(Segment { source, start, len });
            start_time = start_time.min(part_start_time);
            end_time = end_time.max(part_end_time);
            sample_count = sample_count.max(part_sample_count);
            if sample_rate <= 0.0 {
                sample_rate = part_sample_rate;
            }
        }

        if channels.is_empty() {
            return Err(DataError::NoChannels);
        }

        Ok(Self {
            meta: DatasetMeta {
                source_name,
                channels,
                start_time: if start_time.is_finite() {
                    start_time
                } else {
                    0.0
                },
                end_time: if end_time.is_finite() { end_time } else { 0.0 },
                sample_count,
                nominal_sample_rate_hz: sample_rate.max(1.0),
            },
            segments,
        })
    }

    fn segment_channel(&self, channel: usize) -> Option<(usize, usize)> {
        self.segments
            .iter()
            .enumerate()
            .find_map(|(segment_index, segment)| {
                (channel >= segment.start && channel < segment.start + segment.len)
                    .then_some((segment_index, channel - segment.start))
            })
    }

    fn common_summary_from_samples(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        target_bins: usize,
    ) -> DataResult<RangeSummary> {
        let block = self.read_range(
            start_time,
            end_time,
            channels,
            target_bins.saturating_mul(16).max(target_bins).max(1),
        )?;
        Ok(RangeSummary::from_samples(
            &block,
            channels.len(),
            start_time,
            end_time,
            target_bins,
        ))
    }

    fn validate_channels(&self, channels: &[usize]) -> DataResult<()> {
        if channels
            .iter()
            .any(|channel| *channel >= self.meta.channels.len())
        {
            return Err(DataError::BadChannel);
        }
        Ok(())
    }

    fn same_times(left: &[f64], right: &[f64]) -> bool {
        left.len() == right.len() && left.iter().zip(right).all(|(a, b)| (*a - *b).abs() <= 1e-9)
    }

    fn interpolate_to_times(
        source_times: &[f64],
        values: &[f32],
        target_times: &[f64],
    ) -> Vec<f32> {
        if source_times.is_empty() || values.is_empty() {
            return vec![f32::NAN; target_times.len()];
        }

        let mut out = Vec::with_capacity(target_times.len());
        let mut index = 0usize;
        for &target in target_times {
            while index + 1 < source_times.len() && source_times[index + 1] < target {
                index += 1;
            }
            if index + 1 >= source_times.len() {
                out.push(*values.get(index).unwrap_or(&f32::NAN));
                continue;
            }
            let t0 = source_times[index];
            let t1 = source_times[index + 1];
            let y0 = *values.get(index).unwrap_or(&f32::NAN);
            let y1 = *values.get(index + 1).unwrap_or(&f32::NAN);
            if !t0.is_finite() || !t1.is_finite() || t1 <= t0 || !y0.is_finite() || !y1.is_finite()
            {
                out.push(y0);
            } else {
                let ratio = ((target - t0) / (t1 - t0)).clamp(0.0, 1.0) as f32;
                out.push(y0 + (y1 - y0) * ratio);
            }
        }
        out
    }
}

impl DataSource for CombinedDataSource {
    fn open(_path: &std::path::Path) -> DataResult<Self>
    where
        Self: Sized,
    {
        Err(DataError::UnsupportedFormat(
            "CombinedDataSource must be created from existing sources".to_owned(),
        ))
    }

    fn metadata(&self) -> &DatasetMeta {
        &self.meta
    }

    fn read_range(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        max_points: usize,
    ) -> DataResult<SampleBlock> {
        self.validate_channels(channels)?;
        let mut times = Vec::new();
        let mut output = vec![Vec::new(); channels.len()];

        for (out_index, &channel) in channels.iter().enumerate() {
            let Some((segment_index, local_channel)) = self.segment_channel(channel) else {
                return Err(DataError::BadChannel);
            };
            let block = self.segments[segment_index].source.read_range(
                start_time,
                end_time,
                &[local_channel],
                max_points,
            )?;
            if times.is_empty() {
                times = block.times;
                output[out_index] = block.channels.into_iter().next().unwrap_or_default();
            } else if Self::same_times(&times, &block.times) {
                output[out_index] = block.channels.into_iter().next().unwrap_or_default();
            } else {
                let values = block.channels.into_iter().next().unwrap_or_default();
                output[out_index] = Self::interpolate_to_times(&block.times, &values, &times);
            }
        }

        Ok(SampleBlock {
            times,
            channels: output,
        })
    }

    fn summarize_range(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        target_bins: usize,
    ) -> DataResult<RangeSummary> {
        self.validate_channels(channels)?;
        if channels.is_empty() || end_time <= start_time {
            return Ok(RangeSummary {
                bin_start: Vec::new(),
                bin_end: Vec::new(),
                min: vec![Vec::new(); channels.len()],
                max: vec![Vec::new(); channels.len()],
            });
        }

        let mut mapped = Vec::with_capacity(channels.len());
        for &channel in channels {
            let Some((segment_index, local_channel)) = self.segment_channel(channel) else {
                return Err(DataError::BadChannel);
            };
            mapped.push((segment_index, local_channel));
        }

        let first_segment = mapped[0].0;
        if mapped
            .iter()
            .all(|(segment_index, _)| *segment_index == first_segment)
        {
            let local_channels = mapped
                .iter()
                .map(|(_, local_channel)| *local_channel)
                .collect::<Vec<_>>();
            return self.segments[first_segment].source.summarize_range(
                start_time,
                end_time,
                &local_channels,
                target_bins,
            );
        }

        self.common_summary_from_samples(start_time, end_time, channels, target_bins)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedSource {
        meta: DatasetMeta,
        times: Vec<f64>,
        channels: Vec<Vec<f32>>,
    }

    impl FixedSource {
        fn new(name: &str, values: Vec<f32>) -> Self {
            Self {
                meta: DatasetMeta {
                    source_name: name.to_owned(),
                    channels: vec![ChannelMeta {
                        index: 0,
                        name: name.to_owned(),
                        unit: String::new(),
                        sample_rate_hz: 1.0,
                        scale: 1.0,
                        default_visible: true,
                    }],
                    start_time: 0.0,
                    end_time: 3.0,
                    sample_count: 4,
                    nominal_sample_rate_hz: 1.0,
                },
                times: vec![0.0, 1.0, 2.0, 3.0],
                channels: vec![values],
            }
        }
    }

    impl DataSource for FixedSource {
        fn open(_path: &std::path::Path) -> DataResult<Self>
        where
            Self: Sized,
        {
            Err(DataError::UnsupportedFormat("test only".to_owned()))
        }

        fn metadata(&self) -> &DatasetMeta {
            &self.meta
        }

        fn read_range(
            &self,
            _start_time: f64,
            _end_time: f64,
            channels: &[usize],
            _max_points: usize,
        ) -> DataResult<SampleBlock> {
            Ok(SampleBlock {
                times: self.times.clone(),
                channels: channels
                    .iter()
                    .map(|channel| self.channels[*channel].clone())
                    .collect(),
            })
        }

        fn summarize_range(
            &self,
            _start_time: f64,
            _end_time: f64,
            channels: &[usize],
            _target_bins: usize,
        ) -> DataResult<RangeSummary> {
            Ok(RangeSummary {
                bin_start: vec![99.0],
                bin_end: vec![100.0],
                min: vec![vec![0.0]; channels.len()],
                max: vec![vec![0.0]; channels.len()],
            })
        }
    }

    #[test]
    fn mixed_segment_summary_uses_common_time_bins() {
        let first = Arc::new(FixedSource::new("A", vec![1.0, 2.0, 3.0, 4.0]));
        let second = Arc::new(FixedSource::new("B", vec![10.0, 20.0, 30.0, 40.0]));
        let source =
            CombinedDataSource::new("combined".to_owned(), vec![(first, false), (second, true)])
                .unwrap();

        let summary = source.summarize_range(0.0, 3.0, &[0, 1], 2).unwrap();
        assert_eq!(summary.bin_start, vec![0.0, 1.5]);
        assert_eq!(summary.bin_end, vec![1.5, 3.0]);
        assert_eq!(summary.min[0], vec![1.0, 3.0]);
        assert_eq!(summary.max[0], vec![2.0, 4.0]);
        assert_eq!(summary.min[1], vec![10.0, 30.0]);
        assert_eq!(summary.max[1], vec![20.0, 40.0]);
    }
}
