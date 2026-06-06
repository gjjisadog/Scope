use std::{
    cmp::Ordering,
    io::Cursor,
    path::{Path, PathBuf},
};

use csv::{Position, StringRecord};

use super::{
    channel_names::VARIABLE_NAMES, text_encoding::csv_reader_from_path_with_headers, ChannelMeta,
    DataError, DataResult, DataSource, DatasetMeta, RangeSummary, SampleBlock,
};

const CHANNEL_COUNT: usize = 60;
const RAW_WORDS: usize = 32;
const INDEX_BLOCK_RECORDS: u64 = 4096;
const MAX_EXACT_SUMMARY_SAMPLES_PER_BIN: usize = 512;

#[derive(Clone, Debug)]
struct BlockIndex {
    position: Position,
    start_sample: u64,
    records: u64,
    start_time: f64,
    end_time: f64,
    min: [f32; CHANNEL_COUNT],
    max: [f32; CHANNEL_COUNT],
}

pub struct CloudCsvDataSource {
    path: PathBuf,
    content_column: usize,
    sample_rate_hz: f64,
    meta: DatasetMeta,
    blocks: Vec<BlockIndex>,
}

impl CloudCsvDataSource {
    fn reader_from_path(path: &Path) -> DataResult<csv::Reader<Cursor<Vec<u8>>>> {
        csv_reader_from_path_with_headers(path, true)
    }

    fn content_from_record(record: &StringRecord, content_column: usize) -> DataResult<&str> {
        record
            .get(content_column)
            .filter(|content| !content.is_empty())
            .ok_or_else(|| DataError::Csv("Content field is empty".to_owned()))
    }

    fn hex_byte_at(raw: &str, char_index: usize) -> DataResult<u8> {
        let end = char_index + 2;
        let pair = raw.get(char_index..end).ok_or_else(|| {
            DataError::Csv(format!(
                "Invalid record length: need characters {char_index}-{end}, got {}",
                raw.len()
            ))
        })?;
        u8::from_str_radix(pair, 16)
            .map_err(|_| DataError::Csv(format!("Invalid hexadecimal byte `{pair}`")))
    }

    fn parse_words(raw: &str, start: usize, word_count: usize) -> DataResult<[u16; RAW_WORDS]> {
        if word_count != RAW_WORDS {
            return Err(DataError::Csv(format!(
                "Invalid record word count: expected {RAW_WORDS}, got {word_count}"
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
        for value in values.iter_mut().take(43).skip(31) {
            *value = ((hex1 >> bit) & 1) as f32;
            bit += 1;
        }

        for value in values.iter_mut().take(CHANNEL_COUNT).skip(43) {
            if bit > 15 {
                bit = 0;
            }
            *value = ((hex2 >> bit) & 1) as f32;
            bit += 1;
        }

        values
    }

    fn parse_record(raw: &str) -> DataResult<[[f32; CHANNEL_COUNT]; 2]> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(DataError::Csv("Content field is empty".to_owned()));
        }

        let frame_len = Self::hex_byte_at(raw, 6)? as usize;
        let sublength = frame_len.checked_sub(5).ok_or_else(|| {
            DataError::Csv(format!(
                "Invalid frame length {frame_len}: shorter than the 5-byte header"
            ))
        })?;
        if sublength % 2 != 0 {
            return Err(DataError::Csv(format!(
                "Invalid payload byte count {sublength}: not a whole number of 16-bit words"
            )));
        }
        let word_count = sublength / 2;
        let frame1_start = 18;
        let frame2_start = frame1_start + word_count * 4 + 12;
        let words1 = Self::parse_words(raw, frame1_start, word_count)?;
        let words2 = Self::parse_words(raw, frame2_start, word_count)?;

        Ok([Self::expand_words(&words1), Self::expand_words(&words2)])
    }

    fn parse_record_from_csv(
        record: &StringRecord,
        content_column: usize,
    ) -> DataResult<[[f32; CHANNEL_COUNT]; 2]> {
        let raw = Self::content_from_record(record, content_column)?;
        Self::parse_record(raw)
    }

    pub fn open_with_sample_rate(path: &Path, sample_rate_hz: f64) -> DataResult<Self> {
        let sample_rate_hz = sample_rate_hz.max(1.0);
        let mut reader = Self::reader_from_path(path)?;
        let headers = reader.headers()?;
        let Some(content_column) = headers.iter().position(|header| {
            header
                .trim_start_matches('\u{feff}')
                .eq_ignore_ascii_case("content")
        }) else {
            return Err(DataError::Csv(format!(
                "Cloud Content CSV must include a `Content` header, found `{}`",
                headers.iter().collect::<Vec<_>>().join(",")
            )));
        };

        let mut blocks = Vec::new();
        let mut current: Option<BlockIndex> = None;
        let mut parsed_records = 0_u64;
        let mut skipped_records = 0_u64;
        let mut first_parse_error: Option<String> = None;
        let mut record = StringRecord::new();

        loop {
            let position = reader.position().clone();
            if !reader.read_record(&mut record)? {
                break;
            }
            let line_number = record.position().map(|pos| pos.line()).unwrap_or(0);
            let frames = match Self::parse_record_from_csv(&record, content_column) {
                Ok(frames) => frames,
                Err(error) => {
                    skipped_records += 1;
                    first_parse_error.get_or_insert_with(|| format!("line {line_number}: {error}"));
                    continue;
                }
            };

            let needs_new = current
                .as_ref()
                .is_none_or(|block| block.records >= INDEX_BLOCK_RECORDS);
            if needs_new {
                if let Some(block) = current.take() {
                    blocks.push(block);
                }
                current = Some(Self::new_block(
                    position,
                    parsed_records * 2,
                    sample_rate_hz,
                ));
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
                .map(|error| format!("First error: {error}"))
                .unwrap_or_else(|| "No Content data rows found.".to_owned());
            return Err(DataError::Csv(format!(
                "No valid cloud records parsed; skipped {skipped_records} row(s). {detail}"
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
            content_column,
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

    fn new_block(position: Position, start_sample: u64, sample_rate_hz: f64) -> BlockIndex {
        BlockIndex {
            position,
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
            for (channel, value) in frame.iter().enumerate() {
                block.min[channel] = block.min[channel].min(*value);
                block.max[channel] = block.max[channel].max(*value);
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
            Err(index) => index
                .saturating_sub(1)
                .min(self.blocks.len().saturating_sub(1)),
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
        let mut reader = Self::reader_from_path(&self.path)?;
        reader.seek(self.blocks[first_block].position.clone())?;
        let remaining_samples = self
            .meta
            .sample_count
            .saturating_sub(self.blocks[first_block].start_sample)
            .max(1) as usize;
        let estimated_points = (((end_time - start_time) * self.sample_rate_hz + 1.0).max(1.0)
            as usize)
            .min(remaining_samples);
        let stride = (estimated_points / max_points.max(1)).max(1);
        let mut sample_index = self.blocks[first_block].start_sample;
        let mut seen = 0_usize;
        let capacity = estimated_points.min(max_points.max(1)) + 1;
        let mut times = Vec::with_capacity(capacity);
        let mut channel_values = (0..channels.len())
            .map(|_| Vec::with_capacity(capacity))
            .collect::<Vec<_>>();
        let mut record = StringRecord::new();

        while reader.read_record(&mut record)? {
            let Ok(frames) = Self::parse_record_from_csv(&record, self.content_column) else {
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
                if seen.is_multiple_of(stride) {
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
        let estimated_points = (((end_time - start_time) * self.sample_rate_hz + 1.0).max(1.0)
            as usize)
            .min(self.meta.sample_count as usize);
        let exact_summary_limit = target_bins
            .max(1)
            .saturating_mul(MAX_EXACT_SUMMARY_SAMPLES_PER_BIN);
        if estimated_points <= exact_summary_limit {
            let block = self.read_range(
                start_time,
                end_time,
                channels,
                estimated_points.saturating_add(2),
            )?;
            return Ok(RangeSummary::from_samples(
                &block,
                channels.len(),
                start_time,
                end_time,
                target_bins,
            ));
        }

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
    use std::{fs::File, io::Write};

    const RECORD: &str = "01148c450610020000a9203d109b1f590b10f09d033ffe55011c00bb0b9df04202aa0b6cf01b03c4fa6d0d37f7b8147002590c9eff590c79ffdcfffbff26fe30fe0000000073fc4d804506100220007b2037109b1fb50edff2e8fdebf64b060601c70dc9f30bfdc00edff219fe26ef39128ffc81147102590caaff560c9dffc5ffefffc5f6c9f70000000073fc4d8057c5";

    #[test]
    fn parses_one_cloud_record_into_two_samples() {
        let frames = CloudCsvDataSource::parse_record(RECORD).unwrap();
        assert_eq!(frames[0].len(), CHANNEL_COUNT);
        assert_eq!(frames[0][0], 8361.0);
        assert_eq!(frames[0][30], 3.0);
    }

    #[test]
    fn opens_content_csv_with_standard_reader() {
        let path = std::env::temp_dir().join("scope_analyzer_cloud_test.csv");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "Content,Ignored").unwrap();
        writeln!(file, "{RECORD},metadata").unwrap();
        drop(file);

        let source = CloudCsvDataSource::open(&path).unwrap();
        assert_eq!(source.metadata().channels.len(), CHANNEL_COUNT);
        assert_eq!(source.metadata().sample_count, 2);

        let block = source.read_range(0.0, 1.0, &[0, 30], 100).unwrap();
        assert_eq!(block.channels.len(), 2);
        assert_eq!(block.times.len(), 2);

        let _ = std::fs::remove_file(path);
    }
}
