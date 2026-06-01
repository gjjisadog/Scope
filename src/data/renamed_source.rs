use std::sync::Arc;

use super::{
    ChannelMeta, DataError, DataResult, DataSource, DatasetMeta, RangeSummary, SampleBlock,
};

pub struct RenamedDataSource {
    source: Arc<dyn DataSource>,
    meta: DatasetMeta,
}

impl RenamedDataSource {
    pub fn new(
        source: Arc<dyn DataSource>,
        source_name: String,
        names: &[&str],
    ) -> DataResult<Self> {
        let source_meta = source.metadata();
        if source_meta.channels.len() != names.len() {
            return Err(DataError::Csv(format!(
                "Cannot rename {} channels with {} names.",
                source_meta.channels.len(),
                names.len()
            )));
        }
        let start_time = source_meta.start_time;
        let end_time = source_meta.end_time;
        let sample_count = source_meta.sample_count;
        let nominal_sample_rate_hz = source_meta.nominal_sample_rate_hz;

        let channels = source_meta
            .channels
            .iter()
            .zip(names.iter())
            .map(|(channel, name)| ChannelMeta {
                index: channel.index,
                name: (*name).to_owned(),
                unit: channel.unit.clone(),
                sample_rate_hz: channel.sample_rate_hz,
                scale: channel.scale,
                default_visible: channel.default_visible,
            })
            .collect::<Vec<_>>();

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
}

impl DataSource for RenamedDataSource {
    fn open(_path: &std::path::Path) -> DataResult<Self>
    where
        Self: Sized,
    {
        Err(DataError::UnsupportedFormat(
            "RenamedDataSource must be created from an existing source".to_owned(),
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
        self.source
            .read_range(start_time, end_time, channels, max_points)
    }

    fn summarize_range(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        target_bins: usize,
    ) -> DataResult<RangeSummary> {
        self.source
            .summarize_range(start_time, end_time, channels, target_bins)
    }
}
