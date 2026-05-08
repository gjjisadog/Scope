use std::path::Path;

use thiserror::Error;

pub type DataResult<T> = Result<T, DataError>;

#[derive(Debug, Error)]
pub enum DataError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("CSV format error: {0}")]
    Csv(String),
    #[error("No numeric channels were found")]
    NoChannels,
    #[error("The file contains no samples")]
    Empty,
    #[error("Requested channel index is out of range")]
    BadChannel,
    #[error("This file format is not implemented yet: {0}")]
    UnsupportedFormat(String),
}

#[derive(Clone, Debug)]
pub struct ChannelMeta {
    pub index: usize,
    pub name: String,
    pub unit: String,
    pub sample_rate_hz: f64,
    pub scale: f32,
    pub default_visible: bool,
}

#[derive(Clone, Debug)]
pub struct DatasetMeta {
    pub source_name: String,
    pub channels: Vec<ChannelMeta>,
    pub start_time: f64,
    pub end_time: f64,
    pub sample_count: u64,
    pub nominal_sample_rate_hz: f64,
}

impl DatasetMeta {
    pub fn duration(&self) -> f64 {
        (self.end_time - self.start_time).max(0.0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SampleBlock {
    pub times: Vec<f64>,
    pub channels: Vec<Vec<f32>>,
}

#[derive(Clone, Debug)]
pub struct RangeSummary {
    pub bin_start: Vec<f64>,
    pub bin_end: Vec<f64>,
    pub min: Vec<Vec<f32>>,
    pub max: Vec<Vec<f32>>,
}

pub trait DataSource {
    fn open(path: &Path) -> DataResult<Self>
    where
        Self: Sized;

    fn metadata(&self) -> &DatasetMeta;

    fn read_range(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        max_points: usize,
    ) -> DataResult<SampleBlock>;

    fn summarize_range(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        target_bins: usize,
    ) -> DataResult<RangeSummary>;
}
