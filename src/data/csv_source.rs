use std::{
    cmp::Ordering,
    io::Cursor,
    path::{Path, PathBuf},
};

use csv::{Position, StringRecord};

use super::{
    text_encoding::{csv_reader_from_path, normalize_label},
    ChannelMeta, DataError, DataResult, DataSource, DatasetMeta, RangeSummary, SampleBlock,
};

const MAX_CHANNELS: usize = 128;
const INDEX_BLOCK_ROWS: u64 = 4096;

#[derive(Clone, Debug)]
struct BlockIndex {
    position: Position,
    start_sample: u64,
    rows: u64,
    start_time: f64,
    end_time: f64,
    min: Vec<f32>,
    max: Vec<f32>,
}

#[derive(Clone, Copy, Debug)]
enum CsvLayout {
    TimeColumn,
    SyntheticTime { sample_interval_s: f64 },
}

pub struct CsvDataSource {
    path: PathBuf,
    layout: CsvLayout,
    meta: DatasetMeta,
    blocks: Vec<BlockIndex>,
}

impl CsvDataSource {
    fn reader_from_path(path: &Path) -> DataResult<csv::Reader<Cursor<Vec<u8>>>> {
        csv_reader_from_path(path)
    }

    fn is_end_marker(record: &StringRecord) -> bool {
        record.iter().any(|field| {
            let field = field.trim();
            !field.is_empty() && field.to_ascii_lowercase().contains("end")
        })
    }

    fn first_non_empty(record: &StringRecord) -> Option<&str> {
        record.iter().find_map(|field| {
            let field = field.trim().trim_start_matches('\u{feff}');
            (!field.is_empty()).then_some(field)
        })
    }

    fn parse_positive_f64(raw: &str) -> Option<f64> {
        let value = raw.trim().parse::<f64>().ok()?;
        (value.is_finite() && value > 0.0).then_some(value)
    }

    fn looks_like_time_header(name: &str) -> bool {
        let normalized = name
            .trim()
            .trim_start_matches('\u{feff}')
            .to_ascii_lowercase()
            .replace([' ', '_', '-', '(', ')', '[', ']'], "");
        normalized == "t"
            || normalized == "s"
            || normalized == "sec"
            || normalized == "second"
            || normalized == "seconds"
            || normalized == "time"
            || normalized == "times"
            || normalized == "timestamp"
            || normalized.contains("time")
            || name.contains("时间")
            || name.contains("时刻")
    }

    fn discover_layout(
        reader: &mut csv::Reader<Cursor<Vec<u8>>>,
        fallback_sample_rate_hz: f64,
    ) -> DataResult<(CsvLayout, Vec<String>)> {
        let mut record = StringRecord::new();
        if !reader.read_record(&mut record)? {
            return Err(DataError::Empty);
        }

        let first_key = Self::first_non_empty(&record)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if first_key != "file_path" {
            let names = record
                .iter()
                .map(|name| normalize_label(name.trim_start_matches('\u{feff}')))
                .collect::<Vec<_>>();
            let first_name = names.first().map(String::as_str).unwrap_or_default();
            if Self::looks_like_time_header(first_name) {
                return Ok((CsvLayout::TimeColumn, names));
            }
            let sample_interval_s = 1.0 / fallback_sample_rate_hz.max(1.0);
            return Ok((CsvLayout::SyntheticTime { sample_interval_s }, names));
        }

        let mut sample_interval_s = None;
        loop {
            let mut metadata = StringRecord::new();
            if !reader.read_record(&mut metadata)? {
                return Err(DataError::Csv(
                    "Metadata CSV is missing an END marker and channel header row.".to_owned(),
                ));
            }

            let key = Self::first_non_empty(&metadata)
                .unwrap_or_default()
                .to_ascii_lowercase();
            if key == "dt" {
                sample_interval_s = metadata.iter().skip(1).find_map(Self::parse_positive_f64);
            }

            if Self::is_end_marker(&metadata) {
                break;
            }
        }

        loop {
            let mut names = StringRecord::new();
            if !reader.read_record(&mut names)? {
                return Err(DataError::Csv(
                    "Metadata CSV is missing a channel header row after END.".to_owned(),
                ));
            }
            if names.iter().any(|field| !field.trim().is_empty()) {
                let names = names
                    .iter()
                    .map(|name| normalize_label(name.trim_start_matches('\u{feff}')))
                    .collect::<Vec<_>>();
                let sample_interval_s = sample_interval_s.ok_or_else(|| {
                    DataError::Csv("Metadata CSV is missing a positive dt value.".to_owned())
                })?;
                return Ok((CsvLayout::SyntheticTime { sample_interval_s }, names));
            }
        }
    }

    fn parse_time_field(raw_time: &str) -> Result<f64, String> {
        let time = raw_time
            .parse::<f64>()
            .map_err(|_| format!("Invalid time value: {raw_time}"))?;
        if time.is_finite() {
            Ok(time)
        } else {
            Err(format!("Time value is not finite: {raw_time}"))
        }
    }

    fn parse_channel_value(raw: &str, channel_index: usize) -> Result<f32, String> {
        if raw.is_empty() {
            return Ok(f32::NAN);
        }
        let value = raw
            .parse::<f32>()
            .map_err(|_| format!("Invalid value in channel {}: {raw}", channel_index + 1))?;
        Ok(if value.is_finite() { value } else { f32::NAN })
    }

    fn parse_sample_into(
        record: &StringRecord,
        channel_count: usize,
        values: &mut Vec<f32>,
    ) -> Result<f64, String> {
        let raw_time = record
            .get(0)
            .ok_or_else(|| "Missing time column".to_owned())?;
        let time = Self::parse_time_field(raw_time)?;
        values.clear();
        if values.capacity() < channel_count {
            values.reserve(channel_count);
        }
        for index in 0..channel_count {
            let raw = record.get(index + 1).ok_or_else(|| {
                format!(
                    "Missing channel fields: expected {channel_count}, got {}",
                    record.len().saturating_sub(1)
                )
            })?;
            values.push(Self::parse_channel_value(raw, index)?);
        }
        Ok(time)
    }

    fn parse_time(record: &StringRecord) -> Result<f64, String> {
        let raw_time = record
            .get(0)
            .ok_or_else(|| "Missing time column".to_owned())?;
        Self::parse_time_field(raw_time)
    }

    fn parse_selected_values(
        record: &StringRecord,
        channel_count: usize,
        channels: &[usize],
    ) -> Result<Vec<f32>, String> {
        let mut selected = Vec::with_capacity(channels.len());
        for &channel in channels {
            if channel >= channel_count {
                return Err("Requested channel is out of range".to_owned());
            }
            let raw = record
                .get(channel + 1)
                .ok_or_else(|| "Requested channel column is missing".to_owned())?;
            selected.push(Self::parse_channel_value(raw, channel)?);
        }
        Ok(selected)
    }

    fn parse_synthetic_sample_into(
        record: &StringRecord,
        channel_count: usize,
        sample_index: u64,
        sample_interval_s: f64,
        values: &mut Vec<f32>,
    ) -> Result<f64, String> {
        values.clear();
        if values.capacity() < channel_count {
            values.reserve(channel_count);
        }
        for index in 0..channel_count {
            let raw = record.get(index).ok_or_else(|| {
                format!(
                    "Missing channel fields: expected {channel_count}, got {}",
                    record.len()
                )
            })?;
            values.push(Self::parse_channel_value(raw, index)?);
        }
        Ok(sample_index as f64 * sample_interval_s)
    }

    fn parse_synthetic_selected_values(
        record: &StringRecord,
        channel_count: usize,
        channels: &[usize],
    ) -> Result<Vec<f32>, String> {
        let mut selected = Vec::with_capacity(channels.len());
        for &channel in channels {
            if channel >= channel_count {
                return Err("Requested channel is out of range".to_owned());
            }
            let raw = record
                .get(channel)
                .ok_or_else(|| "Requested channel column is missing".to_owned())?;
            selected.push(Self::parse_channel_value(raw, channel)?);
        }
        Ok(selected)
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
        if self.blocks.is_empty() || channels.is_empty() || end_time <= start_time {
            return Ok(SampleBlock::default());
        }

        let first_block = self.find_block_for_time(start_time);
        let mut reader = Self::reader_from_path(&self.path)?;
        reader.seek(self.blocks[first_block].position.clone())?;
        let mut sample_index = self.blocks[first_block].start_sample;
        let estimated_points =
            ((end_time - start_time) * self.meta.nominal_sample_rate_hz).max(1.0) as usize;
        let stride = (estimated_points / max_points.max(1)).max(1);
        let mut seen = 0_usize;
        let capacity = estimated_points.min(max_points.max(1)) + 1;
        let mut times = Vec::with_capacity(capacity);
        let mut channel_values = (0..channels.len())
            .map(|_| Vec::with_capacity(capacity))
            .collect::<Vec<_>>();
        let mut record = StringRecord::new();

        while reader.read_record(&mut record)? {
            let time = match self.layout {
                CsvLayout::TimeColumn => {
                    let Ok(time) = Self::parse_time(&record) else {
                        continue;
                    };
                    time
                }
                CsvLayout::SyntheticTime { sample_interval_s } => {
                    let time = sample_index as f64 * sample_interval_s;
                    sample_index += 1;
                    time
                }
            };
            if time < start_time {
                continue;
            }
            if time > end_time {
                break;
            }
            if seen.is_multiple_of(stride) {
                let values = match self.layout {
                    CsvLayout::TimeColumn => {
                        Self::parse_selected_values(&record, self.meta.channels.len(), channels)
                    }
                    CsvLayout::SyntheticTime { .. } => Self::parse_synthetic_selected_values(
                        &record,
                        self.meta.channels.len(),
                        channels,
                    ),
                };
                let Ok(values) = values else {
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
        let group = block_count.div_ceil(target_bins.max(1)).max(1);
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

impl CsvDataSource {
    pub fn open_with_sample_rate(path: &Path, fallback_sample_rate_hz: f64) -> DataResult<Self> {
        let mut reader = Self::reader_from_path(path)?;
        let (layout, names) = Self::discover_layout(&mut reader, fallback_sample_rate_hz)?;
        let channel_count = match layout {
            CsvLayout::TimeColumn => names.len().saturating_sub(1).min(MAX_CHANNELS),
            CsvLayout::SyntheticTime { .. } => names.len().min(MAX_CHANNELS),
        };
        if channel_count == 0 {
            return Err(DataError::NoChannels);
        }

        let mut blocks = Vec::new();
        let mut current: Option<BlockIndex> = None;
        let mut row_count = 0_u64;
        let mut skipped_rows = 0_u64;
        let mut first_parse_error: Option<String> = None;
        let mut first_time: Option<f64> = None;
        let mut last_time: Option<f64> = None;
        let mut previous_time: Option<f64> = None;
        let mut dt_sum = 0.0_f64;
        let mut dt_count = 0_u64;
        let mut record = StringRecord::new();
        let mut row_values = Vec::with_capacity(channel_count);

        loop {
            let position = reader.position().clone();
            if !reader.read_record(&mut record)? {
                break;
            }
            let line_number = record.position().map(|pos| pos.line()).unwrap_or(0);
            let sample_result = match layout {
                CsvLayout::TimeColumn => {
                    Self::parse_sample_into(&record, channel_count, &mut row_values)
                }
                CsvLayout::SyntheticTime { sample_interval_s } => {
                    Self::parse_synthetic_sample_into(
                        &record,
                        channel_count,
                        row_count,
                        sample_interval_s,
                        &mut row_values,
                    )
                }
            };
            let time = match sample_result {
                Ok(time) => time,
                Err(error) => {
                    skipped_rows += 1;
                    first_parse_error.get_or_insert_with(|| format!("line {line_number}: {error}"));
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
                .is_none_or(|block| block.rows >= INDEX_BLOCK_ROWS);
            if needs_new {
                if let Some(block) = current.take() {
                    blocks.push(block);
                }
                current = Some(BlockIndex {
                    position,
                    start_sample: row_count,
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
                for (index, value) in row_values.iter().enumerate() {
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
                    "No valid data rows parsed; skipped {skipped_rows} row(s). First error: {error}"
                )));
            }
            return Err(DataError::Empty);
        }

        let start_time = first_time.unwrap_or(0.0);
        let end_time = last_time.unwrap_or(start_time);
        let nominal_sample_rate_hz = if dt_count > 0 {
            1.0 / (dt_sum / dt_count as f64)
        } else if let CsvLayout::SyntheticTime { sample_interval_s } = layout {
            1.0 / sample_interval_s
        } else {
            1.0
        };

        let name_iter: Box<dyn Iterator<Item = &String>> = match layout {
            CsvLayout::TimeColumn => Box::new(names.iter().skip(1).take(channel_count)),
            CsvLayout::SyntheticTime { .. } => Box::new(names.iter().take(channel_count)),
        };
        let channels = name_iter
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
            layout,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::GBK;
    use std::{fs::File, io::Write};

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

    #[test]
    fn opens_gbk_csv_channel_names() {
        let path = std::env::temp_dir().join("scope_analyzer_gbk_csv_test.csv");
        let mut bytes = Vec::new();
        let (header, _, _) = GBK.encode("时间,电网电压,电网电流\n");
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(b"0.000,1,2\n0.001,3,4\n");
        std::fs::write(&path, bytes).unwrap();

        let source = CsvDataSource::open(&path).unwrap();
        assert_eq!(source.metadata().channels[0].name, "电网电压");
        assert_eq!(source.metadata().channels[1].name, "电网电流");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn opens_metadata_csv_with_generated_time_axis() {
        let path = std::env::temp_dir().join("scope_analyzer_metadata_test.csv");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "file_path,C:\\\\wave.csv").unwrap();
        writeln!(file, "dt,0.0001").unwrap();
        writeln!(file, "Number_of,3").unwrap();
        writeln!(file, "t0,49:35.4").unwrap();
        writeln!(file, "--------END--------").unwrap();
        writeln!(file, "A,B,C").unwrap();
        writeln!(file, "1,10,100").unwrap();
        writeln!(file, "2,20,200").unwrap();
        writeln!(file, "3,30,300").unwrap();
        drop(file);

        let source = CsvDataSource::open(&path).unwrap();
        assert_eq!(source.metadata().channels.len(), 3);
        assert_eq!(source.metadata().sample_count, 3);
        assert_eq!(source.metadata().nominal_sample_rate_hz, 10_000.0);

        let block = source.read_range(0.0, 0.0002, &[0, 2], 100).unwrap();
        assert_eq!(block.times, vec![0.0, 0.0001, 0.0002]);
        assert_eq!(block.channels[0], vec![1.0, 2.0, 3.0]);
        assert_eq!(block.channels[1], vec![100.0, 200.0, 300.0]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn opens_local_csv_without_time_column_with_generated_axis() {
        let path = std::env::temp_dir().join("scope_analyzer_no_time_test.csv");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "A,B").unwrap();
        for index in 0..2500 {
            writeln!(file, "{},{}", index, index * 2).unwrap();
        }
        drop(file);

        let source = CsvDataSource::open_with_sample_rate(&path, 500.0).unwrap();
        assert_eq!(source.metadata().channels.len(), 2);
        assert_eq!(source.metadata().sample_count, 2500);
        assert!((source.metadata().duration() - 4.998).abs() < 1e-9);

        let block = source.read_range(4.990, 4.998, &[0, 1], 100).unwrap();
        assert!(!block.times.is_empty());
        assert_eq!(block.channels.len(), 2);

        let _ = std::fs::remove_file(path);
    }
}
