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
    records: u64,
    start_time: f64,
    end_time: f64,
    min: [f32; CHANNEL_COUNT],
    max: [f32; CHANNEL_COUNT],
}

pub struct CloudCsvDataSource {
    path: PathBuf,
    header_offset: u64,
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
            .ok_or_else(|| DataError::Csv("hex record is shorter than expected".to_owned()))?;
        u8::from_str_radix(pair, 16)
            .map_err(|_| DataError::Csv(format!("invalid hex byte: {pair}")))
    }

    fn parse_words(raw: &str, start: usize, word_count: usize) -> DataResult<[u16; RAW_WORDS]> {
        if word_count != RAW_WORDS {
            return Err(DataError::Csv(format!(
                "expected {RAW_WORDS} words, found {word_count}"
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
            return Err(DataError::Csv("empty Content record".to_owned()));
        }

        let frame_len = Self::hex_byte_at(raw, 6)? as usize;
        let sublength = frame_len
            .checked_sub(5)
            .ok_or_else(|| DataError::Csv("frame length is too short".to_owned()))?;
        let word_count = sublength / 2;
        let frame1_start = 18;
        let frame2_start = frame1_start + word_count * 4 + 12;
        let words1 = Self::parse_words(raw, frame1_start, word_count)?;
        let words2 = Self::parse_words(raw, frame2_start, word_count)?;

        Ok([Self::expand_words(&words1), Self::expand_words(&words2)])
    }

    fn new_block(offset: u64, start_sample: u64) -> BlockIndex {
        BlockIndex {
            offset,
            records: 0,
            start_time: start_sample as f64,
            end_time: start_sample as f64,
            min: [f32::INFINITY; CHANNEL_COUNT],
            max: [f32::NEG_INFINITY; CHANNEL_COUNT],
        }
    }

    fn update_block(block: &mut BlockIndex, frames: &[[f32; CHANNEL_COUNT]; 2]) {
        block.records += 1;
        block.end_time = block.start_time + block.records as f64 * 2.0 - 1.0;
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
        let mut file = File::open(path)?;
        let mut reader = BufReader::new(&mut file);
        let mut header = String::new();
        let header_bytes = reader.read_line(&mut header)?;
        if header_bytes == 0 {
            return Err(DataError::Empty);
        }
        if !header.trim().eq_ignore_ascii_case("content") {
            return Err(DataError::Csv(
                "cloud waveform CSV must have a single Content column".to_owned(),
            ));
        }

        let mut blocks = Vec::new();
        let mut current: Option<BlockIndex> = None;
        let mut parsed_records = 0_u64;
        let mut skipped_records = 0_u64;

        loop {
            let offset = reader.stream_position()?;
            let mut line = String::new();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            let frames = match Self::parse_record(&line) {
                Ok(frames) => frames,
                Err(_) => {
                    skipped_records += 1;
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
                current = Some(Self::new_block(offset, parsed_records * 2 + 1));
            }

            if let Some(block) = current.as_mut() {
                Self::update_block(block, &frames);
            }
            parsed_records += 1;
        }

        if let Some(block) = current.take() {
            blocks.push(block);
        }
        if parsed_records == 0 {
            return Err(DataError::Csv(format!(
                "no valid cloud records were parsed; skipped {skipped_records} rows"
            )));
        }

        let channels = VARIABLE_NAMES
            .iter()
            .enumerate()
            .map(|(index, name)| ChannelMeta {
                index,
                name: (*name).to_owned(),
                unit: String::new(),
                sample_rate_hz: 1.0,
                scale: 1.0,
                default_visible: index < 30,
            })
            .collect::<Vec<_>>();

        let source_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cloud_wave.csv")
            .to_owned();

        Ok(Self {
            path: path.to_owned(),
            header_offset: header_bytes as u64,
            meta: DatasetMeta {
                source_name,
                channels,
                start_time: 1.0,
                end_time: (parsed_records * 2) as f64,
                sample_count: parsed_records * 2,
                nominal_sample_rate_hz: 1.0,
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
        if self.blocks.is_empty() || channels.is_empty() || end_time < start_time {
            return Ok(SampleBlock::default());
        }

        let first_block = self.find_block_for_time(start_time);
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.blocks[first_block].offset.max(self.header_offset)))?;
        let mut reader = BufReader::new(file);
        let estimated_points = (end_time - start_time + 1.0).max(1.0) as usize;
        let stride = (estimated_points / max_points.max(1)).max(1);
        let mut sample_index = self.blocks[first_block].start_time as u64;
        let mut seen = 0_usize;
        let mut times = Vec::new();
        let mut channel_values = vec![Vec::new(); channels.len()];

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
                let time = sample_index as f64;
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
        let mut bin_start = Vec::new();
        let mut bin_end = Vec::new();
        let mut mins = vec![Vec::new(); channels.len()];
        let mut maxs = vec![Vec::new(); channels.len()];

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
