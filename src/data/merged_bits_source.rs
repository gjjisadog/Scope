use std::sync::Arc;

use super::{
    ChannelMeta, DataError, DataResult, DataSource, DatasetMeta, RangeSummary, SampleBlock,
};

pub struct MergedLeadingBitsDataSource {
    source: Arc<dyn DataSource>,
    meta: DatasetMeta,
}

impl MergedLeadingBitsDataSource {
    pub fn new(source: Arc<dyn DataSource>, source_name: String) -> DataResult<Self> {
        let source_meta = source.metadata();
        if source_meta.channels.len() < 3 {
            return Err(DataError::NoChannels);
        }
        let start_time = source_meta.start_time;
        let end_time = source_meta.end_time;
        let sample_count = source_meta.sample_count;
        let nominal_sample_rate_hz = source_meta.nominal_sample_rate_hz;

        let mut channels = Vec::with_capacity(source_meta.channels.len().saturating_sub(2));
        channels.push(ChannelMeta {
            index: 0,
            name: "DCH1_DCH3".to_owned(),
            unit: super::combined_source::CHANNEL_UNIT_DIGITAL.to_owned(),
            sample_rate_hz: nominal_sample_rate_hz,
            scale: 1.0,
            default_visible: false,
        });
        for channel in source_meta.channels.iter().skip(3) {
            channels.push(ChannelMeta {
                index: channels.len(),
                name: channel.name.clone(),
                unit: super::combined_source::CHANNEL_UNIT_DIGITAL.to_owned(),
                sample_rate_hz: channel.sample_rate_hz,
                scale: channel.scale,
                default_visible: false,
            });
        }

        Ok(Self {
            source,
            meta: DatasetMeta {
                source_name,
                channels,
                start_time,
                end_time,
                sample_count,
                nominal_sample_rate_hz,
            },
        })
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

    fn source_channels(channels: &[usize]) -> Vec<usize> {
        let Some(max_source_channel) = channels
            .iter()
            .map(|channel| if *channel == 0 { 2 } else { channel + 2 })
            .max()
        else {
            return Vec::new();
        };
        (0..=max_source_channel).collect()
    }

    fn direct_value(block: &SampleBlock, source_channel: usize, row: usize) -> f32 {
        block
            .channels
            .get(source_channel)
            .and_then(|values| values.get(row))
            .copied()
            .unwrap_or(f32::NAN)
    }

    fn bit_value(block: &SampleBlock, source_channel: usize, row: usize) -> f32 {
        let value = Self::direct_value(block, source_channel, row);
        if value.is_finite() && value.round() != 0.0 {
            1.0
        } else {
            0.0
        }
    }

    fn merged_value(block: &SampleBlock, row: usize) -> f32 {
        Self::bit_value(block, 0, row)
            + 2.0 * Self::bit_value(block, 1, row)
            + 4.0 * Self::bit_value(block, 2, row)
    }

    fn transform_block(block: SampleBlock, channels: &[usize]) -> SampleBlock {
        let mut output = vec![Vec::with_capacity(block.times.len()); channels.len()];
        for row in 0..block.times.len() {
            for (out_index, &channel) in channels.iter().enumerate() {
                let value = if channel == 0 {
                    Self::merged_value(&block, row)
                } else {
                    Self::direct_value(&block, channel + 2, row)
                };
                output[out_index].push(value);
            }
        }
        SampleBlock {
            times: block.times,
            channels: output,
        }
    }
}

impl DataSource for MergedLeadingBitsDataSource {
    fn open(_path: &std::path::Path) -> DataResult<Self>
    where
        Self: Sized,
    {
        Err(DataError::UnsupportedFormat(
            "MergedLeadingBitsDataSource must be created from an existing source".to_owned(),
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
        let source_channels = Self::source_channels(channels);
        let block = self
            .source
            .read_range(start_time, end_time, &source_channels, max_points)?;
        Ok(Self::transform_block(block, channels))
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
        Ok(RangeSummary::from_samples(
            &block,
            channels.len(),
            start_time,
            end_time,
            target_bins,
        ))
    }
}
