use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

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
    #[error("SCOPE recording format error: {0}")]
    Scope(String),
    #[error("字段不足：没有找到数值通道。请确认第一行包含时间列和至少 1 个通道列。")]
    NoChannels,
    #[error("空文件或没有有效采样点。请确认文件不是空文件，且数据行格式正确。")]
    Empty,
    #[error("通道索引超出范围。当前文件的通道数量少于请求的通道。")]
    BadChannel,
    #[error("暂不支持该文件格式：{0}")]
    #[allow(dead_code)]
    UnsupportedFormat(String),
    #[error("Operation cancelled.")]
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct DataCancelToken {
    cancelled: Arc<AtomicBool>,
}

impl DataCancelToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn check(&self) -> DataResult<()> {
        if self.is_cancelled() {
            Err(DataError::Cancelled)
        } else {
            Ok(())
        }
    }
}

impl Default for DataCancelToken {
    fn default() -> Self {
        Self::new()
    }
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

pub fn decimation_stride_for_budget(sample_count: usize, max_points: usize) -> usize {
    if sample_count <= 1 || max_points <= 1 || sample_count <= max_points {
        1
    } else {
        (sample_count - 1).div_ceil(max_points - 1).max(1)
    }
}

pub fn should_keep_decimated_sample(
    sample_offset: usize,
    sample_count: usize,
    max_points: usize,
    stride: usize,
) -> bool {
    if max_points <= 1 {
        return sample_offset == 0;
    }
    sample_offset == 0
        || sample_offset + 1 == sample_count
        || sample_offset.is_multiple_of(stride.max(1))
}

pub fn append_sample_columns(
    times: &mut Vec<f64>,
    channel_values: &mut [Vec<f32>],
    time: f64,
    values: &[f32],
) {
    times.push(time);
    for (out_index, value) in values.iter().enumerate().take(channel_values.len()) {
        channel_values[out_index].push(*value);
    }
}

pub fn ensure_last_sample_columns(
    times: &mut Vec<f64>,
    channel_values: &mut [Vec<f32>],
    time: Option<f64>,
    values: Option<&[f32]>,
    max_points: usize,
) {
    if max_points <= 1 {
        return;
    }
    let (Some(time), Some(values)) = (time, values) else {
        return;
    };
    if values.len() < channel_values.len()
        || times
            .last()
            .is_some_and(|last_time| (*last_time - time).abs() <= f64::EPSILON)
    {
        return;
    }
    let budget = max_points.max(1);
    if times.len() < budget {
        append_sample_columns(times, channel_values, time, values);
        return;
    }
    let Some(last_index) = times.len().checked_sub(1) else {
        return;
    };
    times[last_index] = time;
    for (out_index, value) in values.iter().enumerate().take(channel_values.len()) {
        if let Some(slot) = channel_values
            .get_mut(out_index)
            .and_then(|channel| channel.get_mut(last_index))
        {
            *slot = *value;
        }
    }
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

    fn read_range_cancellable(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        max_points: usize,
        cancel: &DataCancelToken,
    ) -> DataResult<SampleBlock> {
        cancel.check()?;
        let block = self.read_range(start_time, end_time, channels, max_points)?;
        cancel.check()?;
        Ok(block)
    }

    fn summarize_range(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        target_bins: usize,
    ) -> DataResult<RangeSummary>;

    fn summarize_range_cancellable(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        target_bins: usize,
        cancel: &DataCancelToken,
    ) -> DataResult<RangeSummary> {
        cancel.check()?;
        let summary = self.summarize_range(start_time, end_time, channels, target_bins)?;
        cancel.check()?;
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::{decimation_stride_for_budget, should_keep_decimated_sample, DataCancelToken};

    #[test]
    fn cloned_data_cancel_token_observes_cancellation() {
        let token = DataCancelToken::new();
        let cloned = token.clone();

        assert!(!token.is_cancelled());

        cloned.cancel();

        assert!(token.is_cancelled());
    }

    #[test]
    fn decimation_keeps_first_and_last_within_budget() {
        let sample_count = 10;
        let max_points = 4;
        let stride = decimation_stride_for_budget(sample_count, max_points);

        let kept = (0..sample_count)
            .filter(|offset| {
                should_keep_decimated_sample(*offset, sample_count, max_points, stride)
            })
            .collect::<Vec<_>>();

        assert_eq!(stride, 3);
        assert_eq!(kept, vec![0, 3, 6, 9]);
    }

    #[test]
    fn decimation_single_point_budget_keeps_only_start() {
        let sample_count = 10;
        let max_points = 1;
        let stride = decimation_stride_for_budget(sample_count, max_points);

        let kept = (0..sample_count)
            .filter(|offset| {
                should_keep_decimated_sample(*offset, sample_count, max_points, stride)
            })
            .collect::<Vec<_>>();

        assert_eq!(stride, 1);
        assert_eq!(kept, vec![0]);
    }
}
