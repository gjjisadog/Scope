use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    thread,
};

use super::{
    decimation_stride_for_budget, should_keep_decimated_sample, text_encoding::decode_label_bytes,
    ChannelMeta, DataCancelToken, DataError, DataResult, DataSource, DatasetMeta, RangeSummary,
    SampleBlock,
};

const MAX_CHANNELS: usize = 128;
const INDEX_BLOCK_SAMPLES: u64 = 4096;
const MAX_EXACT_SUMMARY_SAMPLES: u64 = INDEX_BLOCK_SAMPLES * 4;
const MAX_EXACT_SUMMARY_SAMPLES_PER_BIN: u64 = 512;
const HEADER_FIXED_WORDS: usize = 4;
const HEADER_WORD_BYTES: usize = 4;
const HEADERLESS_CHANNEL_COUNT: usize = 16;
const HEADERLESS_SAMPLE_RATE_HZ: f64 = 1000.0;

#[derive(Clone, Debug)]
struct BlockIndex {
    start_sample: u64,
    samples: u64,
    min: Vec<f32>,
    max: Vec<f32>,
}

impl BlockIndex {
    fn end_sample(&self) -> u64 {
        self.start_sample + self.samples.saturating_sub(1)
    }
}

#[derive(Debug, Default)]
struct DatPyramidIndex {
    levels: Vec<Vec<BlockIndex>>,
    complete: bool,
}

pub struct DatDataSource {
    path: PathBuf,
    header_len: u64,
    record_size: u64,
    sample_rate_hz: f64,
    meta: DatasetMeta,
    index: Arc<RwLock<DatPyramidIndex>>,
}

impl DatDataSource {
    pub fn open_cancellable(path: &Path, cancel: &DataCancelToken) -> DataResult<Self> {
        Self::open_with_cancel(path, Some(cancel))
    }

    fn open_with_cancel(path: &Path, cancel: Option<&DataCancelToken>) -> DataResult<Self> {
        if let Some(cancel) = cancel {
            cancel.check()?;
        }
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
        if file_len < (HEADER_FIXED_WORDS * HEADER_WORD_BYTES) as u64 {
            return Err(DataError::Empty);
        }

        let mut fixed_header = [0_u8; HEADER_FIXED_WORDS * HEADER_WORD_BYTES];
        file.read_exact(&mut fixed_header)?;
        let header_len = Self::read_u32(&fixed_header, 0)? as u64;
        let sample_rate_hz = Self::read_u32(&fixed_header, 2)?.max(1) as f64;
        let channel_count = Self::read_u32(&fixed_header, 3)? as usize;

        if !Self::has_valid_header(file_len, header_len, channel_count) {
            return Self::open_headerless_raw_i16(path, file_len, cancel);
        }

        if channel_count == 0 {
            return Err(DataError::NoChannels);
        }
        if channel_count > MAX_CHANNELS {
            return Err(DataError::Dat(format!(
                "DAT channel count {channel_count} exceeds supported maximum {MAX_CHANNELS}"
            )));
        }
        if header_len < fixed_header.len() as u64 || header_len >= file_len {
            return Err(DataError::Dat(format!(
                "Invalid DAT header length {header_len} for {} byte file",
                file_len
            )));
        }

        file.seek(SeekFrom::Start(0))?;
        let mut header = vec![0_u8; header_len as usize];
        file.read_exact(&mut header)?;

        let record_size = (channel_count * 2) as u64;
        let data_bytes = file_len - header_len;
        let sample_count = data_bytes / record_size;
        if sample_count == 0 {
            return Err(DataError::Empty);
        }

        let names = Self::parse_names(&header, channel_count);
        let channels = (0..channel_count)
            .map(|index| ChannelMeta {
                index,
                name: names
                    .get(index)
                    .filter(|name| !name.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| format!("CH{}", index + 1)),
                unit: String::new(),
                sample_rate_hz,
                scale: 1.0,
                default_visible: index < 8,
            })
            .collect::<Vec<_>>();

        if let Some(cancel) = cancel {
            cancel.check()?;
        }

        let source_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("waveform.dat")
            .to_owned();

        let index = Self::start_background_index_build(
            path.to_owned(),
            header_len,
            record_size,
            channel_count,
            sample_count,
        );

        Ok(Self {
            path: path.to_owned(),
            header_len,
            record_size,
            sample_rate_hz,
            meta: DatasetMeta {
                source_name,
                channels,
                start_time: 0.0,
                end_time: sample_count.saturating_sub(1) as f64 / sample_rate_hz,
                sample_count,
                nominal_sample_rate_hz: sample_rate_hz,
            },
            index,
        })
    }

    fn has_valid_header(file_len: u64, header_len: u64, channel_count: usize) -> bool {
        channel_count > 0
            && channel_count <= MAX_CHANNELS
            && header_len >= (HEADER_FIXED_WORDS * HEADER_WORD_BYTES) as u64
            && header_len < file_len
    }

    fn open_headerless_raw_i16(
        path: &Path,
        file_len: u64,
        cancel: Option<&DataCancelToken>,
    ) -> DataResult<Self> {
        if let Some(cancel) = cancel {
            cancel.check()?;
        }
        let record_size = (HEADERLESS_CHANNEL_COUNT * 2) as u64;
        if file_len < record_size || !file_len.is_multiple_of(record_size) {
            return Err(DataError::Dat(format!(
                "Invalid DAT header and file size {file_len} is not aligned to {HEADERLESS_CHANNEL_COUNT} raw int16 channels"
            )));
        }

        let sample_count = file_len / record_size;
        if sample_count == 0 {
            return Err(DataError::Empty);
        }
        let channels = (0..HEADERLESS_CHANNEL_COUNT)
            .map(|index| ChannelMeta {
                index,
                name: format!("CH{}", index + 1),
                unit: String::new(),
                sample_rate_hz: HEADERLESS_SAMPLE_RATE_HZ,
                scale: 1.0,
                default_visible: index < 8,
            })
            .collect::<Vec<_>>();
        let source_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("waveform.dat")
            .to_owned();

        let index = Self::start_background_index_build(
            path.to_owned(),
            0,
            record_size,
            HEADERLESS_CHANNEL_COUNT,
            sample_count,
        );

        Ok(Self {
            path: path.to_owned(),
            header_len: 0,
            record_size,
            sample_rate_hz: HEADERLESS_SAMPLE_RATE_HZ,
            meta: DatasetMeta {
                source_name,
                channels,
                start_time: 0.0,
                end_time: sample_count.saturating_sub(1) as f64 / HEADERLESS_SAMPLE_RATE_HZ,
                sample_count,
                nominal_sample_rate_hz: HEADERLESS_SAMPLE_RATE_HZ,
            },
            index,
        })
    }

    fn read_u32(header: &[u8], index: usize) -> DataResult<u32> {
        let start = index * HEADER_WORD_BYTES;
        let bytes = header
            .get(start..start + HEADER_WORD_BYTES)
            .ok_or_else(|| DataError::Dat(format!("Missing header word {index}")))?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn parse_names(header: &[u8], channel_count: usize) -> Vec<String> {
        let names_start = (HEADER_FIXED_WORDS + channel_count * 5) * HEADER_WORD_BYTES;
        if names_start >= header.len() {
            return Vec::new();
        }

        header[names_start..]
            .split(|byte| *byte == 0xff)
            .filter_map(|raw| {
                let name = decode_label_bytes(raw);
                if name.is_empty() {
                    None
                } else {
                    Some(name)
                }
            })
            .collect()
    }

    fn parse_selected_values(
        record: &[u8],
        channel_count: usize,
        channels: &[usize],
    ) -> DataResult<Vec<f32>> {
        if record.len() < channel_count * 2 {
            return Err(DataError::Dat(format!(
                "DAT record is too short: expected {} bytes, got {}",
                channel_count * 2,
                record.len()
            )));
        }
        let mut values = Vec::with_capacity(channels.len());
        for &channel in channels {
            if channel >= channel_count {
                return Err(DataError::BadChannel);
            }
            let start = channel * 2;
            values.push(i16::from_le_bytes([record[start], record[start + 1]]) as f32);
        }
        Ok(values)
    }

    fn validate_channels(&self, channels: &[usize]) -> DataResult<()> {
        let count = self.meta.channels.len();
        if channels.iter().any(|&channel| channel >= count) {
            return Err(DataError::BadChannel);
        }
        Ok(())
    }

    fn start_background_index_build(
        path: PathBuf,
        header_len: u64,
        record_size: u64,
        channel_count: usize,
        sample_count: u64,
    ) -> Arc<RwLock<DatPyramidIndex>> {
        let index = Arc::new(RwLock::new(DatPyramidIndex::default()));
        let index_for_worker = Arc::clone(&index);
        thread::spawn(move || {
            let _ = Self::build_pyramid_index(
                &path,
                header_len,
                record_size,
                channel_count,
                sample_count,
                &index_for_worker,
            );
        });
        index
    }

    fn build_pyramid_index(
        path: &Path,
        header_len: u64,
        record_size: u64,
        channel_count: usize,
        sample_count: u64,
        index: &Arc<RwLock<DatPyramidIndex>>,
    ) -> DataResult<()> {
        if channel_count == 0 || sample_count == 0 {
            if let Ok(mut state) = index.write() {
                state.complete = true;
            }
            return Ok(());
        }

        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(header_len))?;
        let mut record = vec![0_u8; record_size as usize];
        let mut block_start = 0_u64;

        while block_start < sample_count {
            let block_samples = INDEX_BLOCK_SAMPLES.min(sample_count - block_start);
            let mut min = vec![f32::INFINITY; channel_count];
            let mut max = vec![f32::NEG_INFINITY; channel_count];

            for _ in 0..block_samples {
                reader.read_exact(&mut record)?;
                Self::update_all_min_max_from_record(&record, channel_count, &mut min, &mut max)?;
            }

            if let Ok(mut state) = index.write() {
                if state.levels.is_empty() {
                    state.levels.push(Vec::new());
                }
                state.levels[0].push(BlockIndex {
                    start_sample: block_start,
                    samples: block_samples,
                    min,
                    max,
                });
            }

            block_start += block_samples;
        }

        if let Ok(mut state) = index.write() {
            let base = state.levels.first().cloned().unwrap_or_default();
            state.levels = Self::build_pyramid_levels(base);
            state.complete = true;
        }
        Ok(())
    }

    fn build_pyramid_levels(base: Vec<BlockIndex>) -> Vec<Vec<BlockIndex>> {
        if base.is_empty() {
            return vec![base];
        }
        let mut levels = vec![base];
        while let Some(previous) = levels.last() {
            if previous.len() <= 1 {
                break;
            }
            let mut next = Vec::with_capacity(previous.len().div_ceil(4));
            for group in previous.chunks(4) {
                let Some(first) = group.first() else {
                    continue;
                };
                let mut min = first.min.clone();
                let mut max = first.max.clone();
                let mut samples = 0_u64;
                for block in group {
                    samples += block.samples;
                    for channel in 0..min.len() {
                        min[channel] = min[channel].min(block.min[channel]);
                        max[channel] = max[channel].max(block.max[channel]);
                    }
                }
                next.push(BlockIndex {
                    start_sample: first.start_sample,
                    samples,
                    min,
                    max,
                });
            }
            levels.push(next);
        }
        levels
    }

    fn read_range_with_cancel(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        max_points: usize,
        cancel: Option<&DataCancelToken>,
    ) -> DataResult<SampleBlock> {
        if let Some(cancel) = cancel {
            cancel.check()?;
        }
        self.validate_channels(channels)?;
        if self.meta.sample_count == 0 || channels.is_empty() || end_time < start_time {
            return Ok(SampleBlock::default());
        }

        let sample_count = self.meta.sample_count;
        let first_sample = (start_time.max(0.0) * self.sample_rate_hz).floor().max(0.0) as u64;
        let last_sample = ((end_time.max(0.0) * self.sample_rate_hz).ceil() as u64)
            .min(sample_count.saturating_sub(1));
        if first_sample > last_sample {
            return Ok(SampleBlock::default());
        }

        let estimated_points = (last_sample - first_sample + 1) as usize;
        let stride = decimation_stride_for_budget(estimated_points, max_points);
        let capacity = estimated_points.min(max_points.max(1)) + 1;
        let mut times = Vec::with_capacity(capacity);
        let mut channel_values = (0..channels.len())
            .map(|_| Vec::with_capacity(capacity))
            .collect::<Vec<_>>();
        let file = File::open(&self.path)?;
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(
            self.header_len + first_sample * self.record_size,
        ))?;
        let mut record = vec![0_u8; self.record_size as usize];
        let mut seen = 0_usize;

        for sample_index in first_sample..=last_sample {
            if seen.is_multiple_of(4096) {
                if let Some(cancel) = cancel {
                    cancel.check()?;
                }
            }
            reader.read_exact(&mut record)?;
            let time = sample_index as f64 / self.sample_rate_hz;
            if time < start_time || time > end_time {
                continue;
            }
            if should_keep_decimated_sample(seen, estimated_points, max_points, stride) {
                let values =
                    Self::parse_selected_values(&record, self.meta.channels.len(), channels)?;
                times.push(time);
                for (out_index, value) in values.iter().enumerate() {
                    channel_values[out_index].push(*value);
                }
            }
            seen += 1;
        }

        if let Some(cancel) = cancel {
            cancel.check()?;
        }
        Ok(SampleBlock {
            times,
            channels: channel_values,
        })
    }

    fn empty_summary(channels: &[usize]) -> RangeSummary {
        RangeSummary {
            bin_start: Vec::new(),
            bin_end: Vec::new(),
            min: vec![Vec::new(); channels.len()],
            max: vec![Vec::new(); channels.len()],
        }
    }

    fn find_index_block_for_sample(blocks: &[BlockIndex], sample: u64) -> Option<usize> {
        if blocks.is_empty() {
            return None;
        }
        match blocks.binary_search_by(|block| {
            if block.end_sample() < sample {
                std::cmp::Ordering::Less
            } else if block.start_sample > sample {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        }) {
            Ok(index) => Some(index),
            Err(index) => Some(index.saturating_sub(1).min(blocks.len().saturating_sub(1))),
        }
    }

    fn indexed_blocks_for_range(
        &self,
        first_sample: u64,
        last_sample: u64,
        target_bins: usize,
    ) -> Option<Vec<BlockIndex>> {
        let state = self.index.read().ok()?;
        let base = state.levels.first()?;
        let last_indexed_sample = base.last()?.end_sample();
        if !state.complete && last_indexed_sample < last_sample {
            return None;
        }

        let target_bins = target_bins.max(1);
        let mut selected_level = base;
        let mut previous_count = usize::MAX;
        for level in &state.levels {
            let first = Self::find_index_block_for_sample(level, first_sample)?;
            let last = Self::find_index_block_for_sample(level, last_sample)?;
            if first > last {
                continue;
            }
            let count = last - first + 1;
            if count <= target_bins || count > previous_count {
                break;
            }
            selected_level = level;
            previous_count = count;
        }

        let first = Self::find_index_block_for_sample(selected_level, first_sample)?;
        let last = Self::find_index_block_for_sample(selected_level, last_sample)?;
        (first <= last).then(|| selected_level[first..=last].to_vec())
    }

    fn summarize_range_from_index(
        &self,
        first_sample: u64,
        last_sample: u64,
        channels: &[usize],
        target_bins: usize,
    ) -> DataResult<Option<RangeSummary>> {
        self.summarize_range_from_index_with_cancel(
            first_sample,
            last_sample,
            channels,
            target_bins,
            None,
        )
    }

    fn summarize_range_from_index_with_cancel(
        &self,
        first_sample: u64,
        last_sample: u64,
        channels: &[usize],
        target_bins: usize,
        cancel: Option<&DataCancelToken>,
    ) -> DataResult<Option<RangeSummary>> {
        let Some(blocks) = self.indexed_blocks_for_range(first_sample, last_sample, target_bins)
        else {
            return Ok(None);
        };

        let target_bins = target_bins.max(1);
        let block_count = blocks.len();
        let group = block_count.div_ceil(target_bins).max(1);
        let capacity = target_bins.min(block_count).max(1);
        let mut bin_start = Vec::with_capacity(capacity);
        let mut bin_end = Vec::with_capacity(capacity);
        let mut mins = (0..channels.len())
            .map(|_| Vec::with_capacity(capacity))
            .collect::<Vec<_>>();
        let mut maxs = (0..channels.len())
            .map(|_| Vec::with_capacity(capacity))
            .collect::<Vec<_>>();

        let mut block_index = 0;
        while block_index < block_count {
            if let Some(cancel) = cancel {
                cancel.check()?;
            }
            let group_end = (block_index + group - 1).min(block_count - 1);
            let group_first_sample = blocks[block_index].start_sample;
            let group_last_sample = blocks[group_end].end_sample();
            let start_sample = group_first_sample.max(first_sample);
            let end_sample = group_last_sample.min(last_sample);
            if start_sample <= end_sample {
                let mut group_min = vec![f32::INFINITY; channels.len()];
                let mut group_max = vec![f32::NEG_INFINITY; channels.len()];
                for block in &blocks[block_index..=group_end] {
                    for (out_index, &channel) in channels.iter().enumerate() {
                        group_min[out_index] = group_min[out_index].min(block.min[channel]);
                        group_max[out_index] = group_max[out_index].max(block.max[channel]);
                    }
                }

                bin_start.push(start_sample as f64 / self.sample_rate_hz);
                bin_end.push(end_sample as f64 / self.sample_rate_hz);
                for out_index in 0..channels.len() {
                    mins[out_index].push(group_min[out_index]);
                    maxs[out_index].push(group_max[out_index]);
                }
            }

            if group_end == usize::MAX {
                break;
            }
            block_index = group_end + 1;
        }

        if let Some(cancel) = cancel {
            cancel.check()?;
        }
        Ok(Some(RangeSummary {
            bin_start,
            bin_end,
            min: mins,
            max: maxs,
        }))
    }

    fn update_all_min_max_from_record(
        record: &[u8],
        channel_count: usize,
        mins: &mut [f32],
        maxs: &mut [f32],
    ) -> DataResult<()> {
        if record.len() < channel_count * 2
            || mins.len() < channel_count
            || maxs.len() < channel_count
        {
            return Err(DataError::Dat(format!(
                "DAT record is too short: expected {} bytes, got {}",
                channel_count * 2,
                record.len()
            )));
        }
        for channel in 0..channel_count {
            let start = channel * 2;
            let value = i16::from_le_bytes([record[start], record[start + 1]]) as f32;
            mins[channel] = mins[channel].min(value);
            maxs[channel] = maxs[channel].max(value);
        }
        Ok(())
    }

    fn summarize_range_exact(
        &self,
        start_time: f64,
        end_time: f64,
        first_sample: u64,
        last_sample: u64,
        channels: &[usize],
        target_bins: usize,
    ) -> DataResult<RangeSummary> {
        self.summarize_range_exact_with_cancel(
            start_time,
            end_time,
            first_sample,
            last_sample,
            channels,
            target_bins,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn summarize_range_exact_with_cancel(
        &self,
        start_time: f64,
        end_time: f64,
        first_sample: u64,
        last_sample: u64,
        channels: &[usize],
        target_bins: usize,
        cancel: Option<&DataCancelToken>,
    ) -> DataResult<RangeSummary> {
        let sample_span = (last_sample - first_sample + 1) as usize;
        let bin_count = target_bins.max(1).min(sample_span).max(1);
        let time_span = (end_time - start_time).max(1.0 / self.sample_rate_hz);
        let mut counts = vec![0_u32; bin_count];
        let mut mins = vec![vec![f32::INFINITY; bin_count]; channels.len()];
        let mut maxs = vec![vec![f32::NEG_INFINITY; bin_count]; channels.len()];
        let file = File::open(&self.path)?;
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(
            self.header_len + first_sample * self.record_size,
        ))?;
        let mut record = vec![0_u8; self.record_size as usize];

        for (offset, sample_index) in (first_sample..=last_sample).enumerate() {
            if offset.is_multiple_of(4096) {
                if let Some(cancel) = cancel {
                    cancel.check()?;
                }
            }
            reader.read_exact(&mut record)?;
            let time = sample_index as f64 / self.sample_rate_hz;
            if time < start_time || time > end_time {
                continue;
            }
            let relative = ((time - start_time) / time_span).clamp(0.0, 1.0);
            let bin = ((relative * bin_count as f64).floor() as usize).min(bin_count - 1);
            counts[bin] += 1;
            let bin_mins = mins.iter_mut().map(|values| &mut values[bin]);
            let bin_maxs = maxs.iter_mut().map(|values| &mut values[bin]);
            for ((min, max), &channel) in bin_mins.zip(bin_maxs).zip(channels) {
                if channel >= self.meta.channels.len() {
                    return Err(DataError::BadChannel);
                }
                let start = channel * 2;
                let value = i16::from_le_bytes([record[start], record[start + 1]]) as f32;
                *min = (*min).min(value);
                *max = (*max).max(value);
            }
        }

        if let Some(cancel) = cancel {
            cancel.check()?;
        }
        let mut bin_start = Vec::with_capacity(bin_count);
        let mut bin_end = Vec::with_capacity(bin_count);
        let mut compact_mins = (0..channels.len()).map(|_| Vec::new()).collect::<Vec<_>>();
        let mut compact_maxs = (0..channels.len()).map(|_| Vec::new()).collect::<Vec<_>>();

        for (bin, count) in counts.iter().enumerate() {
            if *count == 0 {
                continue;
            }
            bin_start.push(start_time + time_span * bin as f64 / bin_count as f64);
            bin_end.push(start_time + time_span * (bin + 1) as f64 / bin_count as f64);
            for out_index in 0..channels.len() {
                compact_mins[out_index].push(mins[out_index][bin]);
                compact_maxs[out_index].push(maxs[out_index][bin]);
            }
        }

        Ok(RangeSummary {
            bin_start,
            bin_end,
            min: compact_mins,
            max: compact_maxs,
        })
    }

    #[cfg(test)]
    fn wait_for_index_complete_for_test(&self) {
        for _ in 0..200 {
            if self
                .index
                .read()
                .map(|state| state.complete)
                .unwrap_or(false)
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("DAT background index did not complete in time");
    }

    #[cfg(test)]
    fn indexed_base_block_count_for_test(&self) -> usize {
        self.index
            .read()
            .ok()
            .and_then(|state| state.levels.first().map(Vec::len))
            .unwrap_or(0)
    }
}

impl DataSource for DatDataSource {
    fn open(path: &Path) -> DataResult<Self> {
        Self::open_with_cancel(path, None)
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
        self.read_range_with_cancel(start_time, end_time, channels, max_points, None)
    }

    fn read_range_cancellable(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        max_points: usize,
        cancel: &DataCancelToken,
    ) -> DataResult<SampleBlock> {
        self.read_range_with_cancel(start_time, end_time, channels, max_points, Some(cancel))
    }

    fn summarize_range(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        target_bins: usize,
    ) -> DataResult<RangeSummary> {
        self.validate_channels(channels)?;
        if self.meta.sample_count == 0 || channels.is_empty() || end_time < start_time {
            return Ok(Self::empty_summary(channels));
        }

        let sample_count = self.meta.sample_count;
        let first_sample = (start_time.max(0.0) * self.sample_rate_hz).floor().max(0.0) as u64;
        let last_sample = ((end_time.max(0.0) * self.sample_rate_hz).ceil() as u64)
            .min(sample_count.saturating_sub(1));
        if first_sample > last_sample {
            return Ok(Self::empty_summary(channels));
        }

        let sample_span = last_sample - first_sample + 1;
        let exact_summary_limit = MAX_EXACT_SUMMARY_SAMPLES.max(
            (target_bins as u64)
                .saturating_mul(MAX_EXACT_SUMMARY_SAMPLES_PER_BIN)
                .max(1),
        );
        if sample_span <= exact_summary_limit {
            self.summarize_range_exact(
                start_time,
                end_time,
                first_sample,
                last_sample,
                channels,
                target_bins,
            )
        } else if let Some(summary) =
            self.summarize_range_from_index(first_sample, last_sample, channels, target_bins)?
        {
            Ok(summary)
        } else {
            self.summarize_range_exact(
                start_time,
                end_time,
                first_sample,
                last_sample,
                channels,
                target_bins,
            )
        }
    }

    fn summarize_range_cancellable(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        target_bins: usize,
        cancel: &DataCancelToken,
    ) -> DataResult<RangeSummary> {
        cancel.check()?;
        self.validate_channels(channels)?;
        if self.meta.sample_count == 0 || channels.is_empty() || end_time < start_time {
            return Ok(Self::empty_summary(channels));
        }

        let sample_count = self.meta.sample_count;
        let first_sample = (start_time.max(0.0) * self.sample_rate_hz).floor().max(0.0) as u64;
        let last_sample = ((end_time.max(0.0) * self.sample_rate_hz).ceil() as u64)
            .min(sample_count.saturating_sub(1));
        if first_sample > last_sample {
            return Ok(Self::empty_summary(channels));
        }

        let sample_span = last_sample - first_sample + 1;
        let exact_summary_limit = MAX_EXACT_SUMMARY_SAMPLES.max(
            (target_bins as u64)
                .saturating_mul(MAX_EXACT_SUMMARY_SAMPLES_PER_BIN)
                .max(1),
        );
        if sample_span <= exact_summary_limit {
            self.summarize_range_exact_with_cancel(
                start_time,
                end_time,
                first_sample,
                last_sample,
                channels,
                target_bins,
                Some(cancel),
            )
        } else if let Some(summary) = self.summarize_range_from_index_with_cancel(
            first_sample,
            last_sample,
            channels,
            target_bins,
            Some(cancel),
        )? {
            Ok(summary)
        } else {
            self.summarize_range_exact_with_cancel(
                start_time,
                end_time,
                first_sample,
                last_sample,
                channels,
                target_bins,
                Some(cancel),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::GBK;
    use std::{fs::File, io::Write};

    #[test]
    fn opens_dat_and_reads_a_range() {
        let path = std::env::temp_dir().join("scope_analyzer_test.dat");
        let header_len = 64_u32;
        let channel_count = 2_u32;
        let mut header = Vec::new();
        header.extend_from_slice(&header_len.to_le_bytes());
        header.extend_from_slice(&0_u32.to_le_bytes());
        header.extend_from_slice(&1000_u32.to_le_bytes());
        header.extend_from_slice(&channel_count.to_le_bytes());
        for value in [0_u32; 10] {
            header.extend_from_slice(&value.to_le_bytes());
        }
        header.extend_from_slice(b"A\xffB\xff");
        header.resize(header_len as usize, 0xff);

        let mut file = File::create(&path).unwrap();
        file.write_all(&header).unwrap();
        for index in 0..10_i16 {
            file.write_all(&index.to_le_bytes()).unwrap();
            file.write_all(&(index * -2).to_le_bytes()).unwrap();
        }
        drop(file);

        let source = DatDataSource::open(&path).unwrap();
        assert_eq!(source.metadata().channels.len(), 2);
        assert_eq!(source.metadata().channels[0].name, "A");
        assert_eq!(source.metadata().sample_count, 10);

        let block = source.read_range(0.002, 0.006, &[0, 1], 100).unwrap();
        assert_eq!(block.times.len(), 5);
        assert_eq!(block.channels[0], vec![2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(block.channels[1], vec![-4.0, -6.0, -8.0, -10.0, -12.0]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn read_range_decimation_preserves_window_end_sample() {
        let path = std::env::temp_dir().join("scope_analyzer_decimated_range_test.dat");
        let header_len = 64_u32;
        let channel_count = 1_u32;
        let mut header = Vec::new();
        header.extend_from_slice(&header_len.to_le_bytes());
        header.extend_from_slice(&0_u32.to_le_bytes());
        header.extend_from_slice(&1000_u32.to_le_bytes());
        header.extend_from_slice(&channel_count.to_le_bytes());
        for value in [0_u32; 5] {
            header.extend_from_slice(&value.to_le_bytes());
        }
        header.extend_from_slice(b"A\xff");
        header.resize(header_len as usize, 0xff);

        let mut file = File::create(&path).unwrap();
        file.write_all(&header).unwrap();
        for index in 0..10_i16 {
            file.write_all(&index.to_le_bytes()).unwrap();
        }
        drop(file);

        let source = DatDataSource::open(&path).unwrap();
        let block = source.read_range(0.0, 0.009, &[0], 4).unwrap();

        assert_eq!(block.times, vec![0.0, 0.003, 0.006, 0.009]);
        assert_eq!(block.channels[0], vec![0.0, 3.0, 6.0, 9.0]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn opens_headerless_raw_16_channel_dat() {
        let path = std::env::temp_dir().join("scope_analyzer_headerless_raw16_test.dat");
        let mut file = File::create(&path).unwrap();
        for sample in 0..3_i16 {
            for channel in 0..HEADERLESS_CHANNEL_COUNT as i16 {
                file.write_all(&(sample * 100 + channel).to_le_bytes())
                    .unwrap();
            }
        }
        drop(file);

        let source = DatDataSource::open(&path).unwrap();
        assert_eq!(source.metadata().channels.len(), HEADERLESS_CHANNEL_COUNT);
        assert_eq!(source.metadata().sample_count, 3);
        assert_eq!(source.metadata().channels[0].name, "CH1");

        let block = source.read_range(0.0, 0.002, &[0, 15], 100).unwrap();
        assert_eq!(block.times, vec![0.0, 0.001, 0.002]);
        assert_eq!(block.channels[0], vec![0.0, 100.0, 200.0]);
        assert_eq!(block.channels[1], vec![15.0, 115.0, 215.0]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn decodes_gbk_dat_channel_names() {
        let path = std::env::temp_dir().join("scope_analyzer_gbk_name_test.dat");
        let header_len = 64_u32;
        let channel_count = 1_u32;
        let mut header = Vec::new();
        header.extend_from_slice(&header_len.to_le_bytes());
        header.extend_from_slice(&0_u32.to_le_bytes());
        header.extend_from_slice(&1000_u32.to_le_bytes());
        header.extend_from_slice(&channel_count.to_le_bytes());
        for value in [0_u32; 5] {
            header.extend_from_slice(&value.to_le_bytes());
        }
        let (name, _, _) = GBK.encode("电网电压");
        header.extend_from_slice(&name);
        header.push(0xff);
        header.resize(header_len as usize, 0xff);

        let mut file = File::create(&path).unwrap();
        file.write_all(&header).unwrap();
        file.write_all(&123_i16.to_le_bytes()).unwrap();
        drop(file);

        let source = DatDataSource::open(&path).unwrap();
        assert_eq!(source.metadata().channels[0].name, "电网电压");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn summarizes_large_dat_range_from_stream_without_eager_index() {
        let path = std::env::temp_dir().join("scope_analyzer_large_summary_test.dat");
        let header_len = 64_u32;
        let sample_rate_hz = 1000_u32;
        let channel_count = 2_u32;
        let sample_count = INDEX_BLOCK_SAMPLES * 5;
        let mut header = Vec::new();
        header.extend_from_slice(&header_len.to_le_bytes());
        header.extend_from_slice(&0_u32.to_le_bytes());
        header.extend_from_slice(&sample_rate_hz.to_le_bytes());
        header.extend_from_slice(&channel_count.to_le_bytes());
        for value in [0_u32; 10] {
            header.extend_from_slice(&value.to_le_bytes());
        }
        header.extend_from_slice(b"A\xffB\xff");
        header.resize(header_len as usize, 0xff);

        let mut file = File::create(&path).unwrap();
        file.write_all(&header).unwrap();
        for index in 0..sample_count as i16 {
            file.write_all(&index.to_le_bytes()).unwrap();
            file.write_all(&(-index).to_le_bytes()).unwrap();
        }
        drop(file);

        let source = DatDataSource::open(&path).unwrap();
        source.wait_for_index_complete_for_test();
        assert_eq!(source.indexed_base_block_count_for_test(), 5);
        let summary = source
            .summarize_range(
                0.0,
                (sample_count - 1) as f64 / sample_rate_hz as f64,
                &[0, 1],
                2,
            )
            .unwrap();

        assert_eq!(summary.bin_start.len(), 2);
        assert_eq!(summary.bin_start, vec![0.0, 12.288]);
        assert_eq!(summary.bin_end, vec![12.287, 20.479]);
        assert_eq!(summary.min[0], vec![0.0, 12288.0]);
        assert_eq!(summary.max[0], vec![12287.0, 20479.0]);
        assert_eq!(summary.min[1], vec![-12287.0, -20479.0]);
        assert_eq!(summary.max[1], vec![0.0, -12288.0]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn builds_multi_level_min_max_pyramid() {
        let base = (0..17)
            .map(|index| BlockIndex {
                start_sample: index * INDEX_BLOCK_SAMPLES,
                samples: INDEX_BLOCK_SAMPLES,
                min: vec![index as f32],
                max: vec![(index * 10) as f32],
            })
            .collect::<Vec<_>>();

        let levels = DatDataSource::build_pyramid_levels(base);

        assert_eq!(levels[0].len(), 17);
        assert_eq!(levels[1].len(), 5);
        assert_eq!(levels[2].len(), 2);
        assert_eq!(levels[1][0].min[0], 0.0);
        assert_eq!(levels[1][0].max[0], 30.0);
        assert_eq!(levels[1][4].min[0], 16.0);
        assert_eq!(levels[1][4].max[0], 160.0);
    }

    #[test]
    fn summarizes_medium_dat_sine_with_screen_level_bins() {
        let path = std::env::temp_dir().join("scope_analyzer_medium_sine_summary_test.dat");
        let header_len = 64_u32;
        let sample_rate_hz = 1000_u32;
        let channel_count = 1_u32;
        let sample_count = 80_000_u64;
        let mut header = Vec::new();
        header.extend_from_slice(&header_len.to_le_bytes());
        header.extend_from_slice(&0_u32.to_le_bytes());
        header.extend_from_slice(&sample_rate_hz.to_le_bytes());
        header.extend_from_slice(&channel_count.to_le_bytes());
        for value in [0_u32; 5] {
            header.extend_from_slice(&value.to_le_bytes());
        }
        header.extend_from_slice(b"SINE\xff");
        header.resize(header_len as usize, 0xff);

        let mut file = File::create(&path).unwrap();
        file.write_all(&header).unwrap();
        for index in 0..sample_count {
            let time = index as f64 / sample_rate_hz as f64;
            let value = (10_000.0 * (std::f64::consts::TAU * time).sin()).round() as i16;
            file.write_all(&value.to_le_bytes()).unwrap();
        }
        drop(file);

        let source = DatDataSource::open(&path).unwrap();
        let summary = source
            .summarize_range(
                0.0,
                (sample_count - 1) as f64 / sample_rate_hz as f64,
                &[0],
                1000,
            )
            .unwrap();

        assert!(summary.bin_start.len() > 900);
        let max_range = summary.max[0]
            .iter()
            .copied()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), value| {
                (lo.min(value), hi.max(value))
            });
        let min_range = summary.min[0]
            .iter()
            .copied()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), value| {
                (lo.min(value), hi.max(value))
            });
        assert!(max_range.1 - max_range.0 > 15_000.0);
        assert!(min_range.1 - min_range.0 > 15_000.0);

        let _ = std::fs::remove_file(path);
    }
}
