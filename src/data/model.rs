use std::path::Path;

use thiserror::Error;

pub type DataResult<T> = Result<T, DataError>;

#[derive(Debug, Error)]
pub enum DataError {
    #[error("文件读写错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("CSV 格式错误：{0}")]
    Csv(String),
    #[error("字段不足：没有找到数值通道。请确认第一行包含时间列和至少 1 个通道列。")]
    NoChannels,
    #[error("空文件或没有有效采样点。请确认文件不是空文件，且数据行格式正确。")]
    Empty,
    #[error("通道索引超出范围。当前文件的通道数量少于请求的通道。")]
    BadChannel,
    #[error("暂不支持该文件格式：{0}")]
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
