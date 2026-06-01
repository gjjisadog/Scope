use std::sync::Arc;

use super::{
    ChannelMeta, DataError, DataResult, DataSource, DatasetMeta, RangeSummary, SampleBlock,
};

pub struct BitfieldDigitalDataSource {
    source: Arc<dyn DataSource>,
    bitfield_channels: Vec<usize>,
    meta: DatasetMeta,
}

impl BitfieldDigitalDataSource {
    pub fn new(
        source: Arc<dyn DataSource>,
        source_name: String,
        bitfield_channels: Vec<usize>,
    ) -> Self {
        let source_meta = source.metadata();
        let start_time = source_meta.start_time;
        let end_time = source_meta.end_time;
        let sample_count = source_meta.sample_count;
        let nominal_sample_rate_hz = source_meta.nominal_sample_rate_hz;
        let channels = (0..32)
            .map(|index| ChannelMeta {
                index,
                name: format!("DDATA{}", index),
                unit: super::combined_source::CHANNEL_UNIT_DIGITAL.to_owned(),
                sample_rate_hz: nominal_sample_rate_hz,
                scale: 1.0,
                default_visible: false,
            })
            .collect::<Vec<_>>();

        Self {
            source,
            bitfield_channels,
            meta: DatasetMeta {
                source_name,
                channels,
                start_time,
                end_time,
                sample_count,
                nominal_sample_rate_hz,
            },
        }
    }

    fn validate_channels(&self, channels: &[usize]) -> DataResult<()> {
        if channels.iter().any(|channel| *channel >= 32) {
            return Err(DataError::BadChannel);
        }
        Ok(())
    }

    fn expand_words(words: &[Vec<f32>], row: usize, channels: &[usize]) -> Vec<f32> {
        let mut packed = 0_u64;
        for (word_index, values) in words.iter().take(3).enumerate() {
            let word = values
                .get(row)
                .copied()
                .filter(|value| value.is_finite())
                .unwrap_or(0.0)
                .round()
                .max(0.0) as u64;
            packed |= word << (word_index * 16);
        }
        channels
            .iter()
            .map(|channel| ((packed >> channel) & 1) as f32)
            .collect()
    }
}

impl DataSource for BitfieldDigitalDataSource {
    fn open(_path: &std::path::Path) -> DataResult<Self>
    where
        Self: Sized,
    {
        Err(DataError::UnsupportedFormat(
            "BitfieldDigitalDataSource must be created from an existing source".to_owned(),
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
        let block =
            self.source
                .read_range(start_time, end_time, &self.bitfield_channels, max_points)?;
        let mut expanded = vec![Vec::with_capacity(block.times.len()); channels.len()];
        for row in 0..block.times.len() {
            for (out_index, value) in Self::expand_words(&block.channels, row, channels)
                .into_iter()
                .enumerate()
            {
                expanded[out_index].push(value);
            }
        }
        Ok(SampleBlock {
            times: block.times,
            channels: expanded,
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
        let block = self.read_range(
            start_time,
            end_time,
            channels,
            target_bins.saturating_mul(8).max(target_bins).max(1),
        )?;
        if block.times.is_empty() {
            return Ok(RangeSummary {
                bin_start: Vec::new(),
                bin_end: Vec::new(),
                min: vec![Vec::new(); channels.len()],
                max: vec![Vec::new(); channels.len()],
            });
        }

        let bin_count = target_bins.max(1).min(block.times.len());
        let time_span = (end_time - start_time).max(f64::EPSILON);
        let mut counts = vec![0_u32; bin_count];
        let mut min = vec![vec![f32::INFINITY; bin_count]; channels.len()];
        let mut max = vec![vec![f32::NEG_INFINITY; bin_count]; channels.len()];
        for (row, time) in block.times.iter().enumerate() {
            let relative = ((*time - start_time) / time_span).clamp(0.0, 1.0);
            let bin = ((relative * bin_count as f64).floor() as usize).min(bin_count - 1);
            counts[bin] += 1;
            for channel_index in 0..channels.len() {
                let value = block.channels[channel_index][row];
                min[channel_index][bin] = min[channel_index][bin].min(value);
                max[channel_index][bin] = max[channel_index][bin].max(value);
            }
        }

        let mut bin_start = Vec::new();
        let mut bin_end = Vec::new();
        let mut compact_min = vec![Vec::new(); channels.len()];
        let mut compact_max = vec![Vec::new(); channels.len()];
        for (bin, count) in counts.iter().enumerate() {
            if *count == 0 {
                continue;
            }
            bin_start.push(start_time + time_span * bin as f64 / bin_count as f64);
            bin_end.push(start_time + time_span * (bin + 1) as f64 / bin_count as f64);
            for channel_index in 0..channels.len() {
                compact_min[channel_index].push(min[channel_index][bin]);
                compact_max[channel_index].push(max[channel_index][bin]);
            }
        }

        Ok(RangeSummary {
            bin_start,
            bin_end,
            min: compact_min,
            max: compact_max,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSource {
        meta: DatasetMeta,
    }

    impl MockSource {
        fn new() -> Self {
            Self {
                meta: DatasetMeta {
                    source_name: "DDATA.csv".to_owned(),
                    channels: (0..3)
                        .map(|index| ChannelMeta {
                            index,
                            name: format!("DDATA_WORD{index}"),
                            unit: String::new(),
                            sample_rate_hz: 1000.0,
                            scale: 1.0,
                            default_visible: false,
                        })
                        .collect(),
                    start_time: 0.0,
                    end_time: 0.001,
                    sample_count: 2,
                    nominal_sample_rate_hz: 1000.0,
                },
            }
        }
    }

    impl DataSource for MockSource {
        fn open(_path: &std::path::Path) -> DataResult<Self>
        where
            Self: Sized,
        {
            Ok(Self::new())
        }

        fn metadata(&self) -> &DatasetMeta {
            &self.meta
        }

        fn read_range(
            &self,
            _start_time: f64,
            _end_time: f64,
            _channels: &[usize],
            _max_points: usize,
        ) -> DataResult<SampleBlock> {
            Ok(SampleBlock {
                times: vec![0.0, 0.001],
                channels: vec![vec![5.0, 0.0], vec![1.0, 2.0], vec![0.0, 0.0]],
            })
        }

        fn summarize_range(
            &self,
            _start_time: f64,
            _end_time: f64,
            _channels: &[usize],
            _target_bins: usize,
        ) -> DataResult<RangeSummary> {
            unreachable!()
        }
    }

    #[test]
    fn expands_first_three_ddata_words_to_32_bits() {
        let source = BitfieldDigitalDataSource::new(
            Arc::new(MockSource::new()),
            "DDATA.csv".to_owned(),
            vec![0, 1, 2],
        );
        assert_eq!(source.metadata().channels.len(), 32);

        let block = source
            .read_range(0.0, 0.001, &[0, 1, 2, 16, 17], 10)
            .unwrap();
        assert_eq!(block.channels[0], vec![1.0, 0.0]);
        assert_eq!(block.channels[1], vec![0.0, 0.0]);
        assert_eq!(block.channels[2], vec![1.0, 0.0]);
        assert_eq!(block.channels[3], vec![1.0, 0.0]);
        assert_eq!(block.channels[4], vec![0.0, 1.0]);
    }
}
