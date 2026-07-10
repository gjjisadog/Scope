use std::path::Path;

use crate::data::{
    decimation_stride_for_budget, should_keep_decimated_sample, ChannelMeta, DataError, DataResult,
    DataSource, DatasetMeta, RangeSummary, SampleBlock,
};

use super::{
    protocol::decode_sample_frame,
    recording::{RecordingError, ScopeRecording},
};

pub struct ScopeRecordingDataSource {
    recording: ScopeRecording,
    metadata: DatasetMeta,
    selected_channel_ids: Vec<u16>,
}

impl ScopeRecordingDataSource {
    fn open_recording(path: &Path) -> Result<Self, RecordingError> {
        let recording = ScopeRecording::open(path)?;
        let recording_metadata = recording.metadata();
        let selected = recording_metadata
            .channel_table
            .channels
            .iter()
            .filter(|channel| recording_metadata.channel_mask & (1_u64 << channel.channel_id) != 0)
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return Err(RecordingError::InvalidFormat(
                "recording contains no selected channels".to_owned(),
            ));
        }
        let channels = selected
            .iter()
            .enumerate()
            .map(|(index, channel)| ChannelMeta {
                index,
                name: channel.name.clone(),
                unit: channel.unit.clone(),
                sample_rate_hz: recording_metadata.sample_rate_hz as f64,
                scale: 1.0,
                default_visible: index < 8,
            })
            .collect();
        let selected_channel_ids = selected.iter().map(|channel| channel.channel_id).collect();
        let nominal_sample_rate_hz = recording_metadata.sample_rate_hz as f64;
        let sample_count = recording
            .sample_records()
            .iter()
            .map(|record| u64::from(record.sample_count))
            .sum();
        let start_time = recording
            .sample_records()
            .first()
            .map(|record| record.timestamp_ticks as f64 / recording_metadata.tick_hz as f64)
            .unwrap_or(0.0);
        let end_time = recording
            .sample_records()
            .last()
            .map(ScopeRecordingDataSource::record_end_ticks)
            .transpose()?
            .map(|ticks| ticks as f64 / recording_metadata.tick_hz as f64)
            .unwrap_or(start_time);
        let source_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Ok(Self {
            recording,
            metadata: DatasetMeta {
                source_name,
                channels,
                start_time,
                end_time,
                sample_count,
                nominal_sample_rate_hz,
            },
            selected_channel_ids,
        })
    }

    fn record_end_ticks(
        record: &super::recording::SampleRecordIndex,
    ) -> Result<u64, RecordingError> {
        record.last_timestamp_ticks()
    }

    fn read_full_range(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
    ) -> DataResult<SampleBlock> {
        if channels
            .iter()
            .any(|channel| *channel >= self.selected_channel_ids.len())
        {
            return Err(DataError::BadChannel);
        }
        if !start_time.is_finite() || !end_time.is_finite() || end_time < start_time {
            return Err(DataError::Scope("invalid time range".to_owned()));
        }
        let requested_ids = channels
            .iter()
            .map(|channel| self.selected_channel_ids[*channel])
            .collect::<Vec<_>>();
        let mut block = SampleBlock {
            times: Vec::new(),
            channels: channels.iter().map(|_| Vec::new()).collect(),
        };
        let tick_hz = self.recording.metadata().tick_hz as f64;
        for record in self.recording.sample_records() {
            let record_start = record.timestamp_ticks as f64 / tick_hz;
            let record_end = record.last_timestamp_ticks().map_err(scope_error)? as f64 / tick_hz;
            if record_end < start_time || record_start > end_time {
                continue;
            }
            let frame = self
                .recording
                .read_sample_frame(record)
                .map_err(scope_error)?;
            let decoded = decode_sample_frame(&frame, &self.recording.metadata().channel_table)
                .map_err(|error| DataError::Scope(error.to_string()))?;
            let positions = requested_ids
                .iter()
                .map(|channel_id| {
                    decoded
                        .channel_ids
                        .iter()
                        .position(|candidate| candidate == channel_id)
                        .ok_or(DataError::BadChannel)
                })
                .collect::<DataResult<Vec<_>>>()?;
            let sample_count = decoded.channels.first().map(Vec::len).unwrap_or(0);
            for sample_offset in 0..sample_count {
                let ticks = decoded.timestamp_ticks
                    + u64::from(decoded.sample_period_ticks) * sample_offset as u64;
                let time = ticks as f64 / tick_hz;
                if time < start_time || time > end_time {
                    continue;
                }
                block.times.push(time);
                for (output, position) in block.channels.iter_mut().zip(&positions) {
                    output.push(decoded.channels[*position][sample_offset]);
                }
            }
        }
        Ok(block)
    }

    fn decimate(block: SampleBlock, max_points: usize) -> SampleBlock {
        if block.times.len() <= max_points || block.times.is_empty() {
            return block;
        }
        if max_points == 0 {
            return SampleBlock {
                times: Vec::new(),
                channels: block.channels.iter().map(|_| Vec::new()).collect(),
            };
        }
        let stride = decimation_stride_for_budget(block.times.len(), max_points);
        let kept = (0..block.times.len())
            .filter(|offset| {
                should_keep_decimated_sample(*offset, block.times.len(), max_points, stride)
            })
            .collect::<Vec<_>>();
        SampleBlock {
            times: kept.iter().map(|index| block.times[*index]).collect(),
            channels: block
                .channels
                .iter()
                .map(|values| kept.iter().map(|index| values[*index]).collect())
                .collect(),
        }
    }
}

impl DataSource for ScopeRecordingDataSource {
    fn open(path: &Path) -> DataResult<Self> {
        Self::open_recording(path).map_err(scope_error)
    }

    fn metadata(&self) -> &DatasetMeta {
        &self.metadata
    }

    fn read_range(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        max_points: usize,
    ) -> DataResult<SampleBlock> {
        let block = self.read_full_range(start_time, end_time, channels)?;
        Ok(Self::decimate(block, max_points))
    }

    fn summarize_range(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        target_bins: usize,
    ) -> DataResult<RangeSummary> {
        let block = self.read_full_range(start_time, end_time, channels)?;
        Ok(RangeSummary::from_samples(
            &block,
            channels.len(),
            start_time,
            end_time,
            target_bins,
        ))
    }
}

fn scope_error(error: RecordingError) -> DataError {
    DataError::Scope(error.to_string())
}
