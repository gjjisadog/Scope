use std::path::Path;

use thiserror::Error;

pub type DataResult<T> = Result<T, DataError>;

#[derive(Debug, Error)]
pub enum DataError {
    #[error("文件读写错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("CSV 格式错误：{0}")]
    Csv(String),
    #[error("CSV read error: {0}")]
    CsvRead(#[from] csv::Error),
    #[error("DAT format error: {0}")]
    Dat(String),
    #[error("字段不足：没有找到数值通道。请确认第一行包含时间列和至少 1 个通道列。")]
    NoChannels,
    #[error("空文件或没有有效采样点。请确认文件不是空文件，且数据行格式正确。")]
    Empty,
    #[error("通道索引超出范围。当前文件的通道数量少于请求的通道。")]
    BadChannel,
    #[error("暂不支持该文件格式：{0}")]
    #[allow(dead_code)]
    UnsupportedFormat(String),
}

#[derive(Clone, Debug)]
pub struct ChannelMeta {
    pub index: usize,
    pub name: String,
    pub unit: String,
    #[allow(dead_code)]
    pub sample_rate_hz: f64,
    #[allow(dead_code)]
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

impl RangeSummary {
    pub fn from_samples(
        block: &SampleBlock,
        channel_count: usize,
        start_time: f64,
        end_time: f64,
        target_bins: usize,
    ) -> Self {
        if block.times.is_empty() || channel_count == 0 || end_time <= start_time {
            return Self {
                bin_start: Vec::new(),
                bin_end: Vec::new(),
                min: vec![Vec::new(); channel_count],
                max: vec![Vec::new(); channel_count],
            };
        }

        let bin_count = target_bins.max(1).min(block.times.len());
        let time_span = (end_time - start_time).max(f64::EPSILON);
        let mut counts = vec![0_u32; bin_count];
        let mut min = vec![vec![f32::INFINITY; bin_count]; channel_count];
        let mut max = vec![vec![f32::NEG_INFINITY; bin_count]; channel_count];

        for (row, time) in block.times.iter().enumerate() {
            if !time.is_finite() {
                continue;
            }
            let relative = ((*time - start_time) / time_span).clamp(0.0, 1.0);
            let bin = ((relative * bin_count as f64).floor() as usize).min(bin_count - 1);
            let mut any_value = false;
            for channel_index in 0..channel_count {
                let Some(value) = block
                    .channels
                    .get(channel_index)
                    .and_then(|values| values.get(row))
                    .copied()
                else {
                    continue;
                };
                if value.is_finite() {
                    min[channel_index][bin] = min[channel_index][bin].min(value);
                    max[channel_index][bin] = max[channel_index][bin].max(value);
                    any_value = true;
                }
            }
            if any_value {
                counts[bin] += 1;
            }
        }

        let mut bin_start = Vec::new();
        let mut bin_end = Vec::new();
        let mut compact_min = vec![Vec::new(); channel_count];
        let mut compact_max = vec![Vec::new(); channel_count];
        for (bin, count) in counts.iter().enumerate() {
            if *count == 0 {
                continue;
            }
            bin_start.push(start_time + time_span * bin as f64 / bin_count as f64);
            bin_end.push(start_time + time_span * (bin + 1) as f64 / bin_count as f64);
            for channel_index in 0..channel_count {
                compact_min[channel_index].push(min[channel_index][bin]);
                compact_max[channel_index].push(max[channel_index][bin]);
            }
        }

        Self {
            bin_start,
            bin_end,
            min: compact_min,
            max: compact_max,
        }
    }
}

pub trait DataSource: Send + Sync {
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
