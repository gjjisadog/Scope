use std::{collections::BTreeMap, path::Path};

use crate::data::{
    decimation_stride_for_budget, should_keep_decimated_sample, ChannelMeta, DataError, DataResult,
    DataSource, DatasetMeta, RangeSummary, SampleBlock,
};
use crate::presentation::ChannelPresentation;

use super::{
    buffer::{LiveSnapshot, SnapshotSegment},
    protocol::ChannelTable,
};

/// Immutable, in-memory adapter that lets a frozen Live Scope snapshot use the
/// same analysis pipeline as file-backed datasets.
pub struct SnapshotDataSource {
    metadata: DatasetMeta,
    channel_ids: Vec<u16>,
    segments: Vec<SnapshotSegment>,
    presentations: BTreeMap<u16, ChannelPresentation>,
}

impl SnapshotDataSource {
    pub fn from_live_snapshot(
        source_name: impl Into<String>,
        snapshot: LiveSnapshot,
        channel_table: &ChannelTable,
        sample_rate_hz: f64,
        presentations: &BTreeMap<u16, ChannelPresentation>,
    ) -> DataResult<Self> {
        if snapshot.channel_ids.is_empty() {
            return Err(DataError::Scope(
                "live snapshot contains no channels".to_owned(),
            ));
        }
        let unique_channel_count = snapshot
            .channel_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        if unique_channel_count != snapshot.channel_ids.len() {
            return Err(DataError::Scope(
                "live snapshot channel ids must be unique".to_owned(),
            ));
        }
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            return Err(DataError::Scope(
                "live snapshot sample rate must be positive".to_owned(),
            ));
        }

        let channels = snapshot
            .channel_ids
            .iter()
            .enumerate()
            .map(|(index, channel_id)| {
                let descriptor = channel_table.channel(*channel_id).ok_or_else(|| {
                    DataError::Scope(format!(
                        "live snapshot channel {channel_id} is missing from the channel table"
                    ))
                })?;
                Ok(ChannelMeta {
                    index,
                    name: descriptor.name.clone(),
                    unit: descriptor.unit.clone(),
                    sample_rate_hz,
                    scale: 1.0,
                    default_visible: presentations
                        .get(channel_id)
                        .map(|presentation| presentation.visible)
                        .unwrap_or(index < 8),
                })
            })
            .collect::<DataResult<Vec<_>>>()?;

        let mut sample_count = 0_u64;
        let mut start_time = f64::INFINITY;
        let mut end_time = f64::NEG_INFINITY;
        let mut previous_segment_end = None;
        for (segment_index, segment) in snapshot.segments.iter().enumerate() {
            validate_segment(segment, snapshot.channel_ids.len(), segment_index)?;
            if let Some(first) = segment.times.first() {
                if previous_segment_end.is_some_and(|previous| *first <= previous) {
                    return Err(DataError::Scope(
                        "live snapshot segments must be strictly time ordered".to_owned(),
                    ));
                }
            }
            sample_count = sample_count
                .checked_add(u64::try_from(segment.times.len()).map_err(|_| {
                    DataError::Scope("live snapshot sample count does not fit u64".to_owned())
                })?)
                .ok_or_else(|| {
                    DataError::Scope("live snapshot sample count overflow".to_owned())
                })?;
            if let Some(first) = segment.times.first() {
                start_time = start_time.min(*first);
            }
            if let Some(last) = segment.times.last() {
                end_time = end_time.max(*last);
                previous_segment_end = Some(*last);
            }
        }
        if sample_count == 0 {
            return Err(DataError::Empty);
        }

        Ok(Self {
            metadata: DatasetMeta {
                source_name: source_name.into(),
                channels,
                start_time,
                end_time,
                sample_count,
                nominal_sample_rate_hz: sample_rate_hz,
            },
            channel_ids: snapshot.channel_ids,
            segments: snapshot.segments,
            presentations: presentations.clone(),
        })
    }

    pub fn channel_ids(&self) -> &[u16] {
        &self.channel_ids
    }

    /// Reads gap-separated blocks. The legacy `DataSource::read_range` path
    /// flattens these blocks for analysis; shared plotting can retain them as
    /// separate lines and must not bridge the gaps.
    pub fn read_segmented_range(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        max_points: usize,
    ) -> DataResult<Vec<SampleBlock>> {
        self.validate_request(start_time, end_time, channels)?;
        if max_points == 0 {
            return Ok(Vec::new());
        }

        let ranges = self
            .segments
            .iter()
            .filter_map(|segment| selected_range(segment, start_time, end_time))
            .collect::<Vec<_>>();
        if ranges.is_empty() {
            return Ok(Vec::new());
        }

        let total_samples = ranges.iter().map(|(_, range)| range.len()).sum::<usize>();
        let mut remaining_budget = max_points.min(total_samples).max(1);
        let mut remaining_samples = total_samples;
        let range_count = ranges.len();
        let mut blocks = Vec::with_capacity(range_count);
        for (range_index, (segment, range)) in ranges.into_iter().enumerate() {
            let remaining_ranges = range_count - range_index;
            let reserved_for_others = remaining_ranges.saturating_sub(1);
            let proportional = range
                .len()
                .saturating_mul(remaining_budget)
                .div_ceil(remaining_samples.max(1));
            let budget = proportional
                .max(1)
                .min(remaining_budget.saturating_sub(reserved_for_others).max(1));
            blocks.push(read_segment(segment, range.clone(), channels, budget));
            remaining_budget = remaining_budget.saturating_sub(budget);
            remaining_samples = remaining_samples.saturating_sub(range.len());
        }
        Ok(blocks)
    }

    fn validate_request(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
    ) -> DataResult<()> {
        if !start_time.is_finite() || !end_time.is_finite() || end_time < start_time {
            return Err(DataError::Scope(
                "invalid live snapshot time range".to_owned(),
            ));
        }
        if channels
            .iter()
            .any(|channel| *channel >= self.channel_ids.len())
        {
            return Err(DataError::BadChannel);
        }
        Ok(())
    }
}

impl DataSource for SnapshotDataSource {
    fn open(_path: &Path) -> DataResult<Self>
    where
        Self: Sized,
    {
        Err(DataError::UnsupportedFormat(
            "SnapshotDataSource is created from a frozen Live Scope capture".to_owned(),
        ))
    }

    fn metadata(&self) -> &DatasetMeta {
        &self.metadata
    }

    fn channel_presentation(&self, channel_index: usize) -> Option<ChannelPresentation> {
        self.channel_ids
            .get(channel_index)
            .and_then(|channel_id| self.presentations.get(channel_id))
            .cloned()
    }

    fn read_range(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        max_points: usize,
    ) -> DataResult<SampleBlock> {
        let blocks = self.read_segmented_range(start_time, end_time, channels, max_points)?;
        let sample_count = blocks.iter().map(|block| block.times.len()).sum();
        let mut output = SampleBlock {
            times: Vec::with_capacity(sample_count),
            channels: channels
                .iter()
                .map(|_| Vec::with_capacity(sample_count))
                .collect(),
        };
        for block in blocks {
            output.times.extend(block.times);
            for (target, source) in output.channels.iter_mut().zip(block.channels) {
                target.extend(source);
            }
        }
        Ok(output)
    }

    fn read_range_segments(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        max_points: usize,
    ) -> DataResult<Vec<SampleBlock>> {
        SnapshotDataSource::read_segmented_range(self, start_time, end_time, channels, max_points)
    }

    fn summarize_range(
        &self,
        start_time: f64,
        end_time: f64,
        channels: &[usize],
        target_bins: usize,
    ) -> DataResult<RangeSummary> {
        let block = self.read_range(start_time, end_time, channels, usize::MAX)?;
        Ok(RangeSummary::from_samples(
            &block,
            channels.len(),
            start_time,
            end_time,
            target_bins,
        ))
    }
}

fn validate_segment(
    segment: &SnapshotSegment,
    channel_count: usize,
    segment_index: usize,
) -> DataResult<()> {
    if segment.channels.len() != channel_count {
        return Err(DataError::Scope(format!(
            "live snapshot segment {segment_index} channel count does not match"
        )));
    }
    if segment
        .channels
        .iter()
        .any(|values| values.len() != segment.times.len())
    {
        return Err(DataError::Scope(format!(
            "live snapshot segment {segment_index} columns are not aligned"
        )));
    }
    if segment.times.iter().any(|time| !time.is_finite())
        || segment.times.windows(2).any(|pair| pair[1] <= pair[0])
    {
        return Err(DataError::Scope(format!(
            "live snapshot segment {segment_index} times must be finite and strictly increasing"
        )));
    }
    Ok(())
}

fn selected_range(
    segment: &SnapshotSegment,
    start_time: f64,
    end_time: f64,
) -> Option<(&SnapshotSegment, std::ops::Range<usize>)> {
    let start = segment.times.partition_point(|time| *time < start_time);
    let end = segment.times.partition_point(|time| *time <= end_time);
    (start < end).then_some((segment, start..end))
}

fn read_segment(
    segment: &SnapshotSegment,
    range: std::ops::Range<usize>,
    channels: &[usize],
    max_points: usize,
) -> SampleBlock {
    let stride = decimation_stride_for_budget(range.len(), max_points);
    let selected = range
        .clone()
        .enumerate()
        .filter_map(|(offset, index)| {
            should_keep_decimated_sample(offset, range.len(), max_points, stride).then_some(index)
        })
        .collect::<Vec<_>>();
    SampleBlock {
        times: selected.iter().map(|index| segment.times[*index]).collect(),
        channels: channels
            .iter()
            .map(|channel| {
                selected
                    .iter()
                    .map(|index| segment.channels[*channel][*index])
                    .collect()
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::protocol::{ChannelDescriptor, ChannelKind, WireFormat};

    fn table() -> ChannelTable {
        ChannelTable {
            revision: 1,
            channels: vec![
                ChannelDescriptor {
                    channel_id: 2,
                    kind: ChannelKind::Analog,
                    wire_format: WireFormat::F32,
                    scale: 1.0,
                    offset: 0.0,
                    unit: "V".to_owned(),
                    name: "A".to_owned(),
                },
                ChannelDescriptor {
                    channel_id: 7,
                    kind: ChannelKind::Analog,
                    wire_format: WireFormat::F32,
                    scale: 1.0,
                    offset: 0.0,
                    unit: "A".to_owned(),
                    name: "B".to_owned(),
                },
            ],
        }
    }

    #[test]
    fn snapshot_data_source_reads_selected_channels_and_preserves_segments() {
        let snapshot = LiveSnapshot {
            channel_ids: vec![2, 7],
            segments: vec![
                SnapshotSegment {
                    times: vec![0.0, 0.1, 0.2],
                    channels: vec![vec![1.0, 2.0, 3.0], vec![10.0, 20.0, 30.0]],
                },
                SnapshotSegment {
                    times: vec![0.5, 0.6],
                    channels: vec![vec![4.0, 5.0], vec![40.0, 50.0]],
                },
            ],
        };
        let source = SnapshotDataSource::from_live_snapshot(
            "capture",
            snapshot,
            &table(),
            10.0,
            &BTreeMap::new(),
        )
        .unwrap();

        let segments = source.read_segmented_range(0.1, 0.5, &[1], 10).unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].times, vec![0.1, 0.2]);
        assert_eq!(segments[0].channels, vec![vec![20.0, 30.0]]);
        assert_eq!(segments[1].times, vec![0.5]);
        assert_eq!(segments[1].channels, vec![vec![40.0]]);

        let flat = source.read_range(0.1, 0.5, &[0], 10).unwrap();
        assert_eq!(flat.times, vec![0.1, 0.2, 0.5]);
        assert_eq!(flat.channels, vec![vec![2.0, 3.0, 4.0]]);
    }

    #[test]
    fn snapshot_data_source_rejects_unaligned_columns() {
        let snapshot = LiveSnapshot {
            channel_ids: vec![2, 7],
            segments: vec![SnapshotSegment {
                times: vec![0.0, 0.1],
                channels: vec![vec![1.0], vec![2.0, 3.0]],
            }],
        };

        assert!(SnapshotDataSource::from_live_snapshot(
            "capture",
            snapshot,
            &table(),
            10.0,
            &BTreeMap::new(),
        )
        .is_err());
    }
}
