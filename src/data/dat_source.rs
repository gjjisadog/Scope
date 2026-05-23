use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use super::{
    ChannelMeta, DataError, DataResult, DataSource, DatasetMeta, RangeSummary, SampleBlock,
};

const MAX_CHANNELS: usize = 128;
const INDEX_BLOCK_SAMPLES: u64 = 4096;
const HEADER_FIXED_WORDS: usize = 4;
const HEADER_WORD_BYTES: usize = 4;

#[derive(Clone, Debug)]
struct BlockIndex {
    samples: u64,
    min: Vec<f32>,
    max: Vec<f32>,
}

pub struct DatDataSource {
    path: PathBuf,
    header_len: u64,
    record_size: u64,
    sample_rate_hz: f64,
    meta: DatasetMeta,
    blocks: Vec<BlockIndex>,
}

impl DatDataSource {
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
                let raw = raw.trim_ascii();
                if raw.is_empty() {
                    None
                } else {
                    Some(String::from_utf8_lossy(raw).into_owned())
                }
            })
            .collect()
    }

    fn parse_record(record: &[u8], channel_count: usize) -> DataResult<Vec<f32>> {
        if record.len() < channel_count * 2 {
            return Err(DataError::Dat(format!(
                "DAT record is too short: expected {} bytes, got {}",
                channel_count * 2,
                record.len()
            )));
        }

        let mut values = Vec::with_capacity(channel_count);
        for channel in 0..channel_count {
            let start = channel * 2;
            values.push(i16::from_le_bytes([record[start], record[start + 1]]) as f32);
        }
        Ok(values)
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
}

impl DataSource for DatDataSource {
    fn open(path: &Path) -> DataResult<Self> {
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

        let mut blocks = Vec::new();
        let mut current: Option<BlockIndex> = None;
        let mut record = vec![0_u8; record_size as usize];
        file.seek(SeekFrom::Start(header_len))?;

        for _ in 0..sample_count {
            file.read_exact(&mut record)?;
            let values = Self::parse_record(&record, channel_count)?;

            let needs_new = current
                .as_ref()
                .is_none_or(|block| block.samples >= INDEX_BLOCK_SAMPLES);
            if needs_new {
                if let Some(block) = current.take() {
                    blocks.push(block);
                }
                current = Some(BlockIndex {
                    samples: 0,
                    min: vec![f32::INFINITY; channel_count],
                    max: vec![f32::NEG_INFINITY; channel_count],
                });
            }

            if let Some(block) = current.as_mut() {
                block.samples += 1;
                for (index, value) in values.iter().enumerate() {
                    block.min[index] = block.min[index].min(*value);
                    block.max[index] = block.max[index].max(*value);
                }
            }
        }
        if let Some(block) = current.take() {
            blocks.push(block);
        }

        let source_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("waveform.dat")
            .to_owned();

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

        let sample_count = self.meta.sample_count;
        let first_sample = (start_time.max(0.0) * self.sample_rate_hz).floor().max(0.0) as u64;
        let last_sample = ((end_time.max(0.0) * self.sample_rate_hz).ceil() as u64)
            .min(sample_count.saturating_sub(1));
        if first_sample > last_sample {
            return Ok(SampleBlock::default());
        }

        let estimated_points = (last_sample - first_sample + 1) as usize;
        let stride = (estimated_points / max_points.max(1)).max(1);
        let capacity = estimated_points.min(max_points.max(1)) + 1;
        let mut times = Vec::with_capacity(capacity);
        let mut channel_values = (0..channels.len())
            .map(|_| Vec::with_capacity(capacity))
            .collect::<Vec<_>>();
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(
            self.header_len + first_sample * self.record_size,
        ))?;
        let mut record = vec![0_u8; self.record_size as usize];
        let mut seen = 0_usize;

        for sample_index in first_sample..=last_sample {
            file.read_exact(&mut record)?;
            let time = sample_index as f64 / self.sample_rate_hz;
            if time < start_time || time > end_time {
                continue;
            }
            if seen.is_multiple_of(stride) {
                let values = Self::parse_record(&record, self.meta.channels.len())?;
                times.push(time);
                for (out_index, &channel) in channels.iter().enumerate() {
                    channel_values[out_index].push(values[channel]);
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
        if self.blocks.is_empty() || channels.is_empty() || end_time < start_time {
            return Ok(RangeSummary {
                bin_start: Vec::new(),
                bin_end: Vec::new(),
                min: vec![Vec::new(); channels.len()],
                max: vec![Vec::new(); channels.len()],
            });
        }

        let sample_count = self.meta.sample_count;
        let first_sample = (start_time.max(0.0) * self.sample_rate_hz).floor().max(0.0) as u64;
        let last_sample = ((end_time.max(0.0) * self.sample_rate_hz).ceil() as u64)
            .min(sample_count.saturating_sub(1));
        if first_sample > last_sample {
            return Ok(RangeSummary {
                bin_start: Vec::new(),
                bin_end: Vec::new(),
                min: vec![Vec::new(); channels.len()],
                max: vec![Vec::new(); channels.len()],
            });
        }

        let sample_span = (last_sample - first_sample + 1) as usize;
        let bin_count = target_bins.max(1).min(sample_span).max(1);
        let time_span = (end_time - start_time).max(1.0 / self.sample_rate_hz);
        let mut counts = vec![0_u32; bin_count];
        let mut mins = vec![vec![f32::INFINITY; bin_count]; channels.len()];
        let mut maxs = vec![vec![f32::NEG_INFINITY; bin_count]; channels.len()];
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(
            self.header_len + first_sample * self.record_size,
        ))?;
        let mut record = vec![0_u8; self.record_size as usize];

        for sample_index in first_sample..=last_sample {
            file.read_exact(&mut record)?;
            let time = sample_index as f64 / self.sample_rate_hz;
            if time < start_time || time > end_time {
                continue;
            }
            let relative = ((time - start_time) / time_span).clamp(0.0, 1.0);
            let bin = ((relative * bin_count as f64).floor() as usize).min(bin_count - 1);
            let values = Self::parse_selected_values(&record, self.meta.channels.len(), channels)?;
            counts[bin] += 1;
            for (out_index, value) in values.iter().enumerate() {
                mins[out_index][bin] = mins[out_index][bin].min(*value);
                maxs[out_index][bin] = maxs[out_index][bin].max(*value);
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
