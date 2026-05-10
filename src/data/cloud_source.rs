use std::{
    cmp::Ordering,
    fs::File,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use super::{
    channel_names::VARIABLE_NAMES, ChannelMeta, DataError, DataResult, DataSource, DatasetMeta,
    RangeSummary, SampleBlock,
};

const CHANNEL_COUNT: usize = 60;
const RAW_WORDS: usize = 32;
const INDEX_BLOCK_RECORDS: u64 = 4096;

#[derive(Clone, Debug)]
struct BlockIndex {
    offset: u64,
    start_sample: u64,
    records: u64,
    start_time: f64,
    end_time: f64,
    min: [f32; CHANNEL_COUNT],
    max: [f32; CHANNEL_COUNT],
}

pub struct CloudCsvDataSource {
    path: PathBuf,
    header_offset: u64,
    sample_rate_hz: f64,
    meta: DatasetMeta,
    blocks: Vec<BlockIndex>,
}

impl CloudCsvDataSource {
    fn content_field(line: &str) -> &str {
        line.trim()
            .split(',')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"')
    }

    fn hex_byte_at(raw: &str, char_index: usize) -> DataResult<u8> {
        let end = char_index + 2;
        let pair = raw
            .get(char_index..end)
            .ok_or_else(|| {
                DataError::Csv(format!(
                    "报文长度异常：需要读取第 {char_index}-{end} 个十六进制字符，但当前报文只有 {} 个字符",
                    raw.len()
                ))
            })?;
        u8::from_str_radix(pair, 16)
            .map_err(|_| DataError::Csv(format!("文件格式错误：发现非法十六进制字节 `{pair}`")))
    }

    fn parse_words(raw: &str, start: usize, word_count: usize) -> DataResult<[u16; RAW_WORDS]> {
        if word_count != RAW_WORDS {
            return Err(DataError::Csv(format!(
                "报文字段不足或长度异常：每个截面需要 {RAW_WORDS} 个 16-bit word，实际解析到 {word_count} 个"
            )));
        }
        let mut words = [0_u16; RAW_WORDS];
        for (index, word) in words.iter_mut().enumerate() {
            let pos = start + index * 4;
            let low = Self::hex_byte_at(raw, pos)? as u16;
            let high = Self::hex_byte_at(raw, pos + 2)? as u16;
            *word = low + high * 256;
        }
        Ok(words)
    }

    fn signed_word(raw: u16) -> f32 {
        i16::from_le_bytes(raw.to_le_bytes()) as f32
    }

    fn expand_words(words: &[u16; RAW_WORDS]) -> [f32; CHANNEL_COUNT] {
        let mut values = [0.0_f32; CHANNEL_COUNT];
        for channel in 0..30 {
            values[channel] = Self::signed_word(words[channel]);
        }

        let hex1 = words[30];
        let hex2 = words[31];
        values[30] = (hex1 & 0x0007) as f32;

        let mut bit = 3_u16;
        for channel in 31..43 {
            values[channel] = ((hex1 >> bit) & 1) as f32;
            bit += 1;
        }

        for channel in 43..CHANNEL_COUNT {
            if bit > 15 {
                bit = 0;
            }
            values[channel] = ((hex2 >> bit) & 1) as f32;
            bit += 1;
        }

        values
    }

    fn parse_record(line: &str) -> DataResult<[[f32; CHANNEL_COUNT]; 2]> {
        let raw = Self::content_field(line);
        if raw.is_empty() {
            return Err(DataError::Csv("字段不足：Content 为空".to_owned()));
        }

        let frame_len = Self::hex_byte_at(raw, 6)? as usize;
        let sublength = frame_len
            .checked_sub(5)
            .ok_or_else(|| {
                DataError::Csv(format!(
                    "报文长度异常：长度字段为 {frame_len}，小于最小头部长度 5"
                ))
            })?;
        if sublength % 2 != 0 {
            return Err(DataError::Csv(format!(
                "报文长度异常：数据区字节数 {sublength} 不是 16-bit word 的整数倍"
            )));
        }
        let word_count = sublength / 2;
        let frame1_start = 18;
        let frame2_start = frame1_start + word_count * 4 + 12;
        let words1 = Self::parse_words(raw, frame1_start, word_count)?;
        let words2 = Self::parse_words(raw, frame2_start, word_count)?;

        Ok([Self::expand_words(&words1), Self::expand_words(&words2)])
    }

    pub fn open_with_sample_rate(path: &Path, sample_rate_hz: f64) -> DataResult<Self> {
        let sample_rate_hz = sample_rate_hz.max(1.0);
        let mut file = File::open(path)?;
        let mut reader = BufReader::new(&mut file);
        let mut header = String::new();
        let header_bytes = reader.read_line(&mut header)?;
        if header_bytes == 0 {
            return Err(DataError::Empty);
        }
        let normalized_header = header
            .trim_start_matches('\u{feff}')
            .trim()
            .trim_matches('"');
        if !normalized_header.eq_ignore_ascii_case("content") {
            return Err(DataError::Csv(format!(
                "文件格式错误：云端录波 CSV 第一行必须是单列 `Content`，当前为 `{normalized_header}`"
            )));
        }

        let mut blocks = Vec::new();
        let mut current: Option<BlockIndex> = None;
        let mut parsed_records = 0_u64;
        let mut skipped_records = 0_u64;
        let mut first_parse_error: Option<String> = None;
        let mut line_number = 1_u64;

        loop {
            let offset = reader.stream_position()?;
            let mut line = String::new();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            line_number += 1;
            let frames = match Self::parse_record(&line) {
                Ok(frames) => frames,
                Err(error) => {
                    skipped_records += 1;
                    first_parse_error
                        .get_or_insert_with(|| format!("第 {line_number} 行：{error}"));
                    continue;
                }
            };

            let needs_new = current
                .as_ref()
                .map_or(true, |block| block.records >= INDEX_BLOCK_RECORDS);
            if needs_new {
                if let Some(block) = current.take() {
                    blocks.push(block);
                }
                current = Some(Self::new_block(offset, parsed_records * 2, sample_rate_hz));
            }

            if let Some(block) = current.as_mut() {
                Self::update_block(block, &frames, sample_rate_hz);
            }
            parsed_records += 1;
        }

        if let Some(block) = current.take() {
            blocks.push(block);
        }
        if parsed_records == 0 {
            let detail = first_parse_error
                .map(|error| format!("第一条错误：{error}"))
                .unwrap_or_else(|| "未发现 Content 数据行".to_owned());
            return Err(DataError::Csv(format!(
                "没有解析到有效云端报文；跳过 {skipped_records} 行。{detail}"
            )));
        }

        let channels = VARIABLE_NAMES
            .iter()
            .enumerate()
            .map(|(index, name)| ChannelMeta {
                index,
                name: (*name).to_owned(),
                unit: String::new(),
                sample_rate_hz,
                scale: 1.0,
                default_visible: index < 30,
            })
            .collect::<Vec<_>>();

        let source_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cloud_wave.csv")
            .to_owned();
        let sample_count = parsed_records * 2;

        Ok(Self {
            path: path.to_owned(),
            header_offset: header_bytes as u64,
            sample_rate_hz,
            meta: DatasetMeta {
                source_name,
                channels,
                start_time: 0.0,
                end_time: sample_count.saturating_sub(1) as f64 / sample_rate_hz,
                sample_count,
                nominal_sample_rate_hz: sample_rate_hz,
            },
            blocks,
        })
    }

    fn new_block(offset: u64, start_sample: u64, sample_rate_hz: f64) -> BlockIndex {
        BlockIndex {
            offset,
            start_sample,
            records: 0,
            start_time: start_sample as f64 / sample_rate_hz,
            end_time: start_sample as f64 / sample_rate_hz,
            min: [f32::INFINITY; CHANNEL_COUNT],
            max: [f32::NEG_INFINITY; CHANNEL_COUNT],
        }
    }

    fn update_block(
        block: &mut BlockIndex,
        frames: &[[f32; CHANNEL_COUNT]; 2],
        sample_rate_hz: f64,
    ) {
        block.records += 1;
        block.end_time = (block.start_sample + block.records * 2 - 1) as f64 / sample_rate_hz;
        for frame in frames {
            for channel in 0..CHANNEL_COUNT {
                block.min[channel] = block.min[channel].min(frame[channel]);
                block.max[channel] = block.max[channel].max(frame[channel]);
            }
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
            Err(index) => index.saturating_sub(1).min(self.blocks.len().saturating_sub(1)),
        }
    }

    fn validate_channels(&self, channels: &[usize]) -> DataResult<()> {
        if channels.iter().any(|&channel| channel >= CHANNEL_COUNT) {
            return Err(DataError::BadChannel);
        }
        Ok(())
    }
}

impl DataSource for CloudCsvDataSource {
    fn open(path: &Path) -> DataResult<Self> {
        Self::open_with_sample_rate(path, 1000.0)
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
        if self.blocks.is_empty() || channels.is_empty() || end_time < start_time {
            return Ok(SampleBlock::default());
        }

        let first_block = self.find_block_for_time(start_time);
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.blocks[first_block].offset.max(self.header_offset)))?;
        let mut reader = BufReader::new(file);
        let estimated_points =
            ((end_time - start_time) * self.sample_rate_hz + 1.0).max(1.0) as usize;
        let stride = (estimated_points / max_points.max(1)).max(1);
        let mut sample_index = self.blocks[first_block].start_sample;
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
            let Ok(frames) = Self::parse_record(&line) else {
                continue;
            };

            for frame in &frames {
                let time = sample_index as f64 / self.sample_rate_hz;
                sample_index += 1;
                if time < start_time {
                    continue;
                }
                if time > end_time {
                    return Ok(SampleBlock {
                        times,
                        channels: channel_values,
                    });
                }
                if seen % stride == 0 {
                    times.push(time);
                    for (out_index, &channel) in channels.iter().enumerate() {
                        channel_values[out_index].push(frame[channel]);
                    }
                }
                seen += 1;
            }
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
        if self.blocks.is_empty() || channels.is_empty() || end_time < start_time {
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

    #[test]
    fn parses_one_cloud_record_into_two_samples() {
        let line = "01148c450610020000a9203d109b1f590b10f09d033ffe55011c00bb0b9df04202aa0b6cf01b03c4fa6d0d37f7b8147002590c9eff590c79ffdcfffbff26fe30fe0000000073fc4d804506100220007b2037109b1fb50edff2e8fdebf64b060601c70dc9f30bfdc00edff219fe26ef39128ffc81147102590caaff560c9dffc5ffefffc5f6c9f70000000073fc4d8057c5";
        let frames = CloudCsvDataSource::parse_record(line).unwrap();
        assert_eq!(frames[0].len(), CHANNEL_COUNT);
        assert_eq!(frames[0][0], 8361.0);
        assert_eq!(frames[0][30], 3.0);
    }
}
