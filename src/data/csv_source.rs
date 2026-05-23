use std::{
    cmp::Ordering,
    fs::File,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use super::{
    ChannelMeta, DataError, DataResult, DataSource, DatasetMeta, RangeSummary, SampleBlock,
};

const MAX_CHANNELS: usize = 128;
const INDEX_BLOCK_ROWS: u64 = 4096;

#[derive(Clone, Debug)]
struct BlockIndex {
    offset: u64,
    first_row: u64,
    rows: u64,
    start_time: f64,
    end_time: f64,
    min: Vec<f32>,
    max: Vec<f32>,
}

pub struct CsvDataSource {
    path: PathBuf,
    header_offset: u64,
    meta: DatasetMeta,
    blocks: Vec<BlockIndex>,
}

impl CsvDataSource {
    fn parse_header(line: &str) -> Vec<String> {
        line.trim_end()
            .split(',')
            .map(|s| s.trim().trim_matches('"').to_owned())
            .collect()
    }

    fn parse_sample(line: &str, channel_count: usize) -> Result<(f64, Vec<f32>), String> {
        let mut parts = line.trim_end().split(',');
        let Some(raw_time) = parts.next() else {
            return Err("字段不足：缺少时间列".to_owned());
        };
        let time = raw_time
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("时间列不是有效数字：{}", raw_time.trim()))?;
        if !time.is_finite() {
            return Err(format!("时间列不是有限数字：{}", raw_time.trim()));
        }
        let mut values = Vec::with_capacity(channel_count);
        for (index, raw) in parts.take(channel_count).enumerate() {
            let raw = raw.trim();
            if raw.is_empty() {
                values.push(f32::NAN);
            } else {
                let value = raw
                    .parse::<f32>()
                    .map_err(|_| format!("第 {} 个通道值不是有效数字：{raw}", index + 1))?;
                values.push(if value.is_finite() { value } else { f32::NAN });
            }
        }
        if values.len() == channel_count {
            Ok((time, values))
        } else {
            Err(format!(
                "字段不足：需要 {channel_count} 个通道值，实际只有 {} 个",
                values.len()
            ))
        }
    }

    fn parse_time(line: &str) -> Result<f64, String> {
        let Some(raw_time) = line.trim_end().split(',').next() else {
            return Err("字段不足：缺少时间列".to_owned());
        };
        let time = raw_time
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("时间列不是有效数字：{}", raw_time.trim()))?;
        if time.is_finite() {
            Ok(time)
        } else {
            Err(format!("时间列不是有限数字：{}", raw_time.trim()))
        }
    }

    fn parse_selected_values(
        line: &str,
        channel_count: usize,
        channels: &[usize],
    ) -> Result<Vec<f32>, String> {
        let mut selected = vec![f32::NAN; channels.len()];
        let mut found = vec![false; channels.len()];
        let mut parts = line.trim_end().split(',');
        parts.next();
        for (index, raw) in parts.take(channel_count).enumerate() {
            let Some(out_index) = channels.iter().position(|channel| *channel == index) else {
                continue;
            };
            let raw = raw.trim();
            selected[out_index] = if raw.is_empty() {
                f32::NAN
            } else {
                let value = raw
                    .parse::<f32>()
                    .map_err(|_| format!("第 {} 个通道值不是有效数字：{raw}", index + 1))?;
                if value.is_finite() {
                    value
                } else {
                    f32::NAN
                }
            };
            found[out_index] = true;
        }
        if found.iter().all(|present| *present) {
            Ok(selected)
        } else {
            Err("字段不足：请求的通道列不存在或数据列不足".to_owned())
        }
    }

    fn find_block_for_time(&self, time: f64) -> usize {
        match self.blocks.binary_search_by(|block| {
            if block.end_time < time {
                Ordering::Less
            } else if block.start_time > time {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }) {
            Ok(index) => index,
            Err(index) => index
                .saturating_sub(1)
                .min(self.blocks.len().saturating_sub(1)),
        }
    }

    fn validate_channels(&self, channels: &[usize]) -> DataResult<()> {
        let count = self.meta.channels.len();
        if channels.iter().any(|&channel| channel >= count) {
            return Err(DataError::BadChannel);
        }
        Ok(())
    }
}

impl DataSource for CsvDataSource {
    fn open(path: &Path) -> DataResult<Self> {
        let mut file = File::open(path)?;
        let mut reader = BufReader::new(&mut file);
        let mut header = String::new();
        let header_bytes = reader.read_line(&mut header)?;
        if header_bytes == 0 {
            return Err(DataError::Empty);
        }

        let names = Self::parse_header(&header);
        if names.len() < 2 {
            return Err(DataError::NoChannels);
        }

        let channel_count = (names.len() - 1).min(MAX_CHANNELS);
        let mut blocks = Vec::new();
        let mut current: Option<BlockIndex> = None;
        let mut row_count = 0_u64;
        let mut skipped_rows = 0_u64;
        let mut first_parse_error: Option<String> = None;
        let mut line_number = 1_u64;
        let mut first_time: Option<f64> = None;
        let mut last_time: Option<f64> = None;
        let mut previous_time: Option<f64> = None;
        let mut dt_sum = 0.0_f64;
        let mut dt_count = 0_u64;

        loop {
            let offset = reader.stream_position()?;
            let mut line = String::new();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            line_number += 1;
            let (time, values) = match Self::parse_sample(&line, channel_count) {
                Ok(sample) => sample,
                Err(error) => {
                    skipped_rows += 1;
                    first_parse_error
                        .get_or_insert_with(|| format!("第 {line_number} 行：{error}"));
                    continue;
                }
            };

            if let Some(prev) = previous_time {
                let dt = time - prev;
                if dt.is_finite() && dt > 0.0 {
                    dt_sum += dt;
                    dt_count += 1;
                }
            }
            previous_time = Some(time);
            first_time.get_or_insert(time);
            last_time = Some(time);

            let needs_new = current
                .as_ref()
                .map_or(true, |block| block.rows >= INDEX_BLOCK_ROWS);
            if needs_new {
                if let Some(block) = current.take() {
                    blocks.push(block);
                }
                current = Some(BlockIndex {
                    offset,
                    first_row: row_count,
                    rows: 0,
                    start_time: time,
                    end_time: time,
                    min: vec![f32::INFINITY; channel_count],
                    max: vec![f32::NEG_INFINITY; channel_count],
                });
            }

            if let Some(block) = current.as_mut() {
                block.rows += 1;
                block.end_time = time;
                for (index, value) in values.iter().enumerate() {
                    if value.is_finite() {
                        block.min[index] = block.min[index].min(*value);
                        block.max[index] = block.max[index].max(*value);
                    }
                }
            }
            row_count += 1;
        }

        if let Some(block) = current.take() {
            blocks.push(block);
        }

        if row_count == 0 {
            if let Some(error) = first_parse_error {
                return Err(DataError::Csv(format!(
                    "没有解析到有效数据行；跳过 {skipped_rows} 行。第一条错误：{error}"
                )));
            }
            return Err(DataError::Empty);
        }

        let start_time = first_time.unwrap_or(0.0);
        let end_time = last_time.unwrap_or(start_time);
        let nominal_sample_rate_hz = if dt_count > 0 {
            1.0 / (dt_sum / dt_count as f64)
        } else {
            1.0
        };

        let channels = names
            .iter()
            .skip(1)
            .take(channel_count)
            .enumerate()
            .map(|(index, name)| ChannelMeta {
                index,
                name: if name.is_empty() {
                    format!("CH{}", index + 1)
                } else {
                    name.clone()
                },
                unit: String::new(),
                sample_rate_hz: nominal_sample_rate_hz,
                scale: 1.0,
                default_visible: index < 8,
            })
            .collect::<Vec<_>>();

        let source_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("waveform.csv")
            .to_owned();

        Ok(Self {
            path: path.to_owned(),
            header_offset: header_bytes as u64,
            meta: DatasetMeta {
                source_name,
                channels,
                start_time,
                end_time,
                sample_count: row_count,
                nominal_sample_rate_hz,
            },
            blocks,
        })
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
        if self.blocks.is_empty() || channels.is_empty() || end_time <= start_time {
            return Ok(SampleBlock::default());
        }

        let first_block = self.find_block_for_time(start_time);
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(
            self.blocks[first_block].offset.max(self.header_offset),
        ))?;
        let mut reader = BufReader::new(file);
        let estimated_points =
            ((end_time - start_time) * self.meta.nominal_sample_rate_hz).max(1.0) as usize;
        let stride = (estimated_points / max_points.max(1)).max(1);
        let mut seen = 0_usize;
        let capacity = estimated_points.min(max_points.max(1)) + 1;
        let mut times = Vec::with_capacity(capacity);
        let mut channel_values = (0..channels.len())
            .map(|_| Vec::with_capacity(capacity))
            .collect::<Vec<_>>();

        loop {
            let mut line = String::new();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            let Ok(time) = Self::parse_time(&line) else {
                continue;
            };
            if time < start_time {
                continue;
            }
            if time > end_time {
                break;
            }
            if seen % stride == 0 {
                let Ok(values) =
                    Self::parse_selected_values(&line, self.meta.channels.len(), channels)
                else {
                    continue;
                };
                times.push(time);
                for (out_index, value) in values.iter().enumerate() {
                    channel_values[out_index].push(*value);
                }
            }
            seen += 1;
        }

        Ok(SampleBlock {
            times,
            channels: channel_values,
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
        if self.blocks.is_empty() || channels.is_empty() || end_time <= start_time {
            return Ok(RangeSummary {
                bin_start: Vec::new(),
                bin_end: Vec::new(),
                min: vec![Vec::new(); channels.len()],
                max: vec![Vec::new(); channels.len()],
            });
        }

        let first = self.find_block_for_time(start_time);
        let last = self.find_block_for_time(end_time);
        let block_count = last.saturating_sub(first) + 1;
        let group = (block_count / target_bins.max(1)).max(1);
        let capacity = target_bins.min(block_count).max(1);
        let mut bin_start = Vec::with_capacity(capacity);
        let mut bin_end = Vec::with_capacity(capacity);
        let mut mins = (0..channels.len())
            .map(|_| Vec::with_capacity(capacity))
            .collect::<Vec<_>>();
        let mut maxs = (0..channels.len())
            .map(|_| Vec::with_capacity(capacity))
            .collect::<Vec<_>>();

        let mut index = first;
        while index <= last {
            let group_end = (index + group - 1).min(last);
            let mut group_min = vec![f32::INFINITY; channels.len()];
            let mut group_max = vec![f32::NEG_INFINITY; channels.len()];
            let start = self.blocks[index].start_time.max(start_time);
            let end = self.blocks[group_end].end_time.min(end_time);

            for block in &self.blocks[index..=group_end] {
                for (out_index, &channel) in channels.iter().enumerate() {
                    group_min[out_index] = group_min[out_index].min(block.min[channel]);
                    group_max[out_index] = group_max[out_index].max(block.max[channel]);
                }
            }

            bin_start.push(start);
            bin_end.push(end);
            for out_index in 0..channels.len() {
                mins[out_index].push(group_min[out_index]);
                maxs[out_index].push(group_max[out_index]);
            }

            if group_end == usize::MAX {
                break;
            }
            index = group_end + 1;
        }

        Ok(RangeSummary {
            bin_start,
            bin_end,
            min: mins,
            max: maxs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn opens_csv_and_reads_a_range() {
        let path = std::env::temp_dir().join("scope_analyzer_test.csv");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "time,A,B").unwrap();
        for index in 0..10 {
            writeln!(file, "{:.3},{},{}", index as f64 * 0.001, index, index * 2).unwrap();
        }
        drop(file);

        let source = CsvDataSource::open(&path).unwrap();
        assert_eq!(source.metadata().channels.len(), 2);
        assert_eq!(source.metadata().sample_count, 10);

        let block = source.read_range(0.002, 0.006, &[0, 1], 100).unwrap();
        assert_eq!(block.channels.len(), 2);
        assert_eq!(block.times.len(), 5);

        let _ = std::fs::remove_file(path);
    }
}
