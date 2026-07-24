//! In-memory SCP1 V2 hardware-capture assembly.

use std::collections::BTreeMap;

use serde::Serialize;

use super::protocol_v2::{
    capture_integrity_summary, CaptureBegin, CaptureData, CaptureEnd, CaptureState, MessageV2,
    StreamDescriptor, StreamSampleBatch, MAX_CAPTURE_BLOCKS, MAX_CAPTURE_BLOCK_ROWS,
    MAX_CAPTURE_PAYLOAD_BYTES, MAX_CAPTURE_ROWS,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CaptureDiagnostics {
    pub capture_complete: bool,
    pub capture_missing_chunks: u32,
    pub capture_duplicate_chunks: u32,
    pub capture_reordered_chunks: u32,
    pub capture_row_discontinuities: u32,
    pub capture_descriptor_mismatches: u32,
    pub capture_integrity_mismatches: u32,
    pub capture_row_overflows: u32,
    pub capture_too_many_blocks: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssembledCapture {
    pub begin: CaptureBegin,
    pub blocks: Vec<StreamSampleBatch>,
    pub diagnostics: CaptureDiagnostics,
}

#[derive(Clone, Debug, Default)]
pub struct CaptureAssembler {
    begin: Option<CaptureBegin>,
    blocks: BTreeMap<u32, CaptureData>,
    descriptor: Option<CaptureDescriptor>,
    expected_descriptor: Option<CaptureDescriptor>,
    expected_next_block: u32,
    total_rows: u32,
    total_payload_bytes: usize,
    diagnostics: CaptureDiagnostics,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CaptureDescriptor {
    stream_id: u16,
    stream_revision: u32,
    domain: super::protocol_v2::SampleDomain,
    capture_phase: super::protocol_v2::CapturePhase,
    consistency_group: u16,
    channel_ids: Vec<u16>,
    sample_period_ticks: u32,
}

impl CaptureDescriptor {
    fn from_batch(batch: &StreamSampleBatch) -> Self {
        Self {
            stream_id: batch.stream_id,
            stream_revision: batch.stream_revision,
            domain: batch.domain,
            capture_phase: batch.capture_phase,
            consistency_group: batch.consistency_group,
            channel_ids: batch.channel_ids.clone(),
            sample_period_ticks: batch.sample_period_ticks,
        }
    }

    fn from_stream(stream: &StreamDescriptor, revision: u32) -> Self {
        Self {
            stream_id: stream.stream_id,
            stream_revision: revision,
            domain: stream.domain,
            capture_phase: stream.capture_phase,
            consistency_group: stream.consistency_group,
            channel_ids: stream.channel_ids.clone(),
            // The period is learned from the first block; the value must then
            // remain invariant for the rest of this capture.
            sample_period_ticks: 0,
        }
    }

    fn matches_batch(&self, batch: &StreamSampleBatch) -> bool {
        self.stream_id == batch.stream_id
            && self.stream_revision == batch.stream_revision
            && self.domain == batch.domain
            && self.capture_phase == batch.capture_phase
            && self.consistency_group == batch.consistency_group
            && self.channel_ids == batch.channel_ids
            && (self.sample_period_ticks == 0
                || self.sample_period_ticks == batch.sample_period_ticks)
    }
}

impl CaptureAssembler {
    pub fn begin(&mut self, begin: CaptureBegin) -> Result<(), String> {
        if begin.capture_id == 0 || begin.row_count == 0 || begin.row_count > MAX_CAPTURE_ROWS {
            return Err("CaptureTooLarge: CAPTURE_BEGIN has an invalid id or row count".to_owned());
        }
        self.begin = Some(begin);
        self.blocks.clear();
        self.descriptor = None;
        self.expected_descriptor = None;
        self.expected_next_block = 0;
        self.total_rows = 0;
        self.total_payload_bytes = 0;
        self.diagnostics = CaptureDiagnostics::default();
        Ok(())
    }

    /// Starts a capture with the descriptor negotiated in STREAM_TABLE.  The
    /// legacy `begin` entry point remains useful for isolated codec tests,
    /// while all live V2 traffic uses this stricter form.
    pub fn begin_with_descriptor(
        &mut self,
        begin: CaptureBegin,
        stream: &StreamDescriptor,
        stream_revision: u32,
    ) -> Result<(), String> {
        if begin.stream_id != stream.stream_id {
            return Err(
                "CaptureDescriptorMismatch: CAPTURE_BEGIN stream is not negotiated".to_owned(),
            );
        }
        self.begin(begin)?;
        self.expected_descriptor = Some(CaptureDescriptor::from_stream(stream, stream_revision));
        Ok(())
    }

    pub fn push(&mut self, data: CaptureData) -> Result<(), String> {
        let begin = self
            .begin
            .as_ref()
            .ok_or_else(|| "capture data arrived before CAPTURE_BEGIN".to_owned())?;
        if data.capture_id != begin.capture_id {
            return Err("capture id does not match CAPTURE_BEGIN".to_owned());
        }
        if data.batch.stream_id != begin.stream_id {
            self.diagnostics.capture_descriptor_mismatches = self
                .diagnostics
                .capture_descriptor_mismatches
                .saturating_add(1);
            return Err(
                "CaptureDescriptorMismatch: capture data stream does not match CAPTURE_BEGIN"
                    .to_owned(),
            );
        }
        if data.block_index >= MAX_CAPTURE_BLOCKS {
            self.diagnostics.capture_too_many_blocks =
                self.diagnostics.capture_too_many_blocks.saturating_add(1);
            return Err("CaptureTooManyBlocks: block index exceeds protocol limit".to_owned());
        }
        if data.batch.row_count > MAX_CAPTURE_BLOCK_ROWS {
            self.diagnostics.capture_row_overflows =
                self.diagnostics.capture_row_overflows.saturating_add(1);
            return Err(
                "CaptureTooLarge: capture block row count exceeds protocol limit".to_owned(),
            );
        }
        if self.blocks.contains_key(&data.block_index) {
            self.diagnostics.capture_duplicate_chunks =
                self.diagnostics.capture_duplicate_chunks.saturating_add(1);
            return Ok(());
        }
        // Encoding invokes the normal V2 batch-header checks before any
        // allocation or map insertion, including metadata and row continuity
        // within the block.
        let payload_bytes = MessageV2::CaptureData(data.clone())
            .encode_payload()
            .map_err(|error| format!("CaptureDescriptorMismatch: {error}"))?
            .len();
        let observed = CaptureDescriptor::from_batch(&data.batch);
        if let Some(expected) = &self.expected_descriptor {
            if !expected.matches_batch(&data.batch) {
                self.diagnostics.capture_descriptor_mismatches = self
                    .diagnostics
                    .capture_descriptor_mismatches
                    .saturating_add(1);
                return Err(
                    "CaptureDescriptorMismatch: CAPTURE_DATA disagrees with STREAM_TABLE"
                        .to_owned(),
                );
            }
        }
        if let Some(existing) = &self.descriptor {
            if !existing.matches_batch(&data.batch) {
                self.diagnostics.capture_descriptor_mismatches = self
                    .diagnostics
                    .capture_descriptor_mismatches
                    .saturating_add(1);
                return Err(
                    "CaptureDescriptorMismatch: capture block descriptor or period changed"
                        .to_owned(),
                );
            }
        } else {
            self.descriptor = Some(observed);
        }
        let next_rows = self
            .total_rows
            .checked_add(u32::from(data.batch.row_count))
            .ok_or_else(|| "CaptureRowOverflow: capture row count overflow".to_owned())?;
        if next_rows > begin.row_count || next_rows > MAX_CAPTURE_ROWS {
            self.diagnostics.capture_row_overflows =
                self.diagnostics.capture_row_overflows.saturating_add(1);
            return Err(
                "CaptureRowOverflow: CAPTURE_DATA exceeds CAPTURE_BEGIN row_count".to_owned(),
            );
        }
        let next_bytes = self
            .total_payload_bytes
            .checked_add(payload_bytes)
            .ok_or_else(|| "CaptureTooLarge: capture payload size overflow".to_owned())?;
        if next_bytes > MAX_CAPTURE_PAYLOAD_BYTES {
            return Err(
                "CaptureTooLarge: capture payload exceeds protocol memory limit".to_owned(),
            );
        }
        if data.block_index != self.expected_next_block {
            self.diagnostics.capture_reordered_chunks =
                self.diagnostics.capture_reordered_chunks.saturating_add(1);
        }
        self.expected_next_block = self
            .expected_next_block
            .max(data.block_index.saturating_add(1));
        self.total_rows = next_rows;
        self.total_payload_bytes = next_bytes;
        self.blocks.insert(data.block_index, data);
        self.validate_adjacent_rows()?;
        Ok(())
    }

    pub fn finish(&mut self, end: CaptureEnd) -> Result<AssembledCapture, String> {
        let begin = self
            .begin
            .take()
            .ok_or_else(|| "CAPTURE_END arrived before CAPTURE_BEGIN".to_owned())?;
        if end.capture_id != begin.capture_id {
            return Err("capture id does not match CAPTURE_END".to_owned());
        }
        if !matches!(end.state, CaptureState::Complete) {
            return Err(format!(
                "capture ended in non-success state {:?}",
                end.state
            ));
        }
        let expected_blocks = end.total_blocks;
        if expected_blocks > MAX_CAPTURE_BLOCKS {
            return Err("CaptureTooManyBlocks: CAPTURE_END exceeds protocol limit".to_owned());
        }
        let actual_blocks =
            u32::try_from(self.blocks.len()).map_err(|_| "too many capture blocks")?;
        for block_index in 0..expected_blocks {
            if !self.blocks.contains_key(&block_index) {
                self.diagnostics.capture_missing_chunks =
                    self.diagnostics.capture_missing_chunks.saturating_add(1);
            }
        }
        let sample_count = self.blocks.values().try_fold(0_u32, |total, data| {
            total
                .checked_add(u32::from(data.batch.row_count))
                .ok_or("capture sample count overflow")
        })?;
        if end.uploaded_rows != sample_count || end.dropped_rows != 0 {
            return Err(
                "CaptureRowOverflow: CAPTURE_END uploaded or dropped row count is invalid"
                    .to_owned(),
            );
        }
        if actual_blocks != expected_blocks
            || sample_count != end.total_samples
            || sample_count != begin.row_count
        {
            return Err("CAPTURE_END block or sample totals do not match uploaded data".to_owned());
        }
        if self.diagnostics.capture_missing_chunks != 0 {
            return Err("capture upload is missing one or more chunks".to_owned());
        }
        self.validate_all_rows()?;
        let first = self
            .blocks
            .values()
            .next()
            .and_then(|data| data.batch.row_metadata.first())
            .map(|row| row.row_sequence)
            .ok_or_else(|| "CaptureRowDiscontinuity: capture has no rows".to_owned())?;
        let last = self
            .blocks
            .values()
            .next_back()
            .and_then(|data| data.batch.row_metadata.last())
            .map(|row| row.row_sequence)
            .ok_or_else(|| "CaptureRowDiscontinuity: capture has no rows".to_owned())?;
        if end.capture_id != begin.capture_id
            || begin.trigger_row_seq < first
            || begin.trigger_row_seq > last
        {
            return Err("CaptureRowDiscontinuity: trigger row is outside capture range".to_owned());
        }
        let blocks = self.blocks.values().cloned().collect::<Vec<_>>();
        let actual_integrity = capture_integrity_summary(begin.capture_id, &blocks)
            .map_err(|error| format!("CaptureIntegrityMismatch: {error}"))?;
        if end.integrity_summary != actual_integrity {
            self.diagnostics.capture_integrity_mismatches = self
                .diagnostics
                .capture_integrity_mismatches
                .saturating_add(1);
            return Err(
                "CaptureIntegrityMismatch: CAPTURE_END CRC32C does not match uploaded blocks"
                    .to_owned(),
            );
        }
        self.diagnostics.capture_complete = true;
        let blocks = std::mem::take(&mut self.blocks)
            .into_values()
            .map(|data| data.batch)
            .collect();
        Ok(AssembledCapture {
            begin,
            blocks,
            diagnostics: self.diagnostics.clone(),
        })
    }

    pub fn device_reset(&mut self) {
        self.begin = None;
        self.blocks.clear();
        self.descriptor = None;
        self.expected_descriptor = None;
        self.total_rows = 0;
        self.total_payload_bytes = 0;
        self.diagnostics.capture_complete = false;
    }

    pub fn diagnostics(&self) -> &CaptureDiagnostics {
        &self.diagnostics
    }

    fn validate_adjacent_rows(&mut self) -> Result<(), String> {
        let mut discontinuity = false;
        for pair in self.blocks.values().collect::<Vec<_>>().windows(2) {
            let previous = pair[0]
                .batch
                .row_metadata
                .last()
                .expect("validated batch metadata");
            let next = pair[1]
                .batch
                .row_metadata
                .first()
                .expect("validated batch metadata");
            if previous.row_sequence.checked_add(1) != Some(next.row_sequence) {
                discontinuity = true;
                break;
            }
        }
        if discontinuity {
            self.diagnostics.capture_row_discontinuities = self
                .diagnostics
                .capture_row_discontinuities
                .saturating_add(1);
            return Err(
                "CaptureRowDiscontinuity: capture rows are not continuous across blocks".to_owned(),
            );
        }
        Ok(())
    }

    fn validate_all_rows(&mut self) -> Result<(), String> {
        self.validate_adjacent_rows()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::{
        protocol_v2::{CapturePhase, SampleDomain},
        snapshot::{SnapshotMeta, APPLIED_SEQUENCE_VALID, FROZEN_ROW, SNAPSHOT_VALID},
    };

    fn batch(row: u64) -> StreamSampleBatch {
        StreamSampleBatch {
            stream_id: 1,
            stream_revision: 1,
            domain: SampleDomain::Fast32k,
            capture_phase: CapturePhase::AfterClaComplete,
            consistency_group: 1,
            first_row_sequence: row,
            sample_period_ticks: 31,
            row_count: 1,
            channel_ids: vec![0],
            sample_data: 0_i16.to_le_bytes().to_vec(),
            row_metadata: vec![SnapshotMeta {
                row_sequence: row,
                source_sequence: row,
                applied_sequence: row.saturating_sub(1),
                valid_flags: SNAPSHOT_VALID | FROZEN_ROW | APPLIED_SEQUENCE_VALID,
            }],
        }
    }

    fn begin() -> CaptureBegin {
        CaptureBegin {
            capture_id: 7,
            stream_id: 1,
            row_count: 2,
            trigger_row_seq: 11,
        }
    }

    fn end() -> CaptureEnd {
        CaptureEnd {
            capture_id: 7,
            state: CaptureState::Complete,
            uploaded_rows: 2,
            dropped_rows: 0,
            total_blocks: 2,
            total_samples: 2,
            integrity_summary: 0,
        }
    }

    #[test]
    fn completes_only_a_contiguous_capture_with_matching_totals() {
        let mut assembler = CaptureAssembler::default();
        assembler.begin(begin()).unwrap();
        assembler
            .push(CaptureData {
                capture_id: 7,
                block_index: 0,
                batch: batch(10),
            })
            .unwrap();
        assembler
            .push(CaptureData {
                capture_id: 7,
                block_index: 1,
                batch: batch(11),
            })
            .unwrap();
        let first = CaptureData {
            capture_id: 7,
            block_index: 0,
            batch: batch(10),
        };
        let second = CaptureData {
            capture_id: 7,
            block_index: 1,
            batch: batch(11),
        };
        let mut end = end();
        end.integrity_summary = capture_integrity_summary(7, &[first, second]).unwrap();
        let capture = assembler.finish(end).unwrap();
        assert!(capture.diagnostics.capture_complete);
        assert_eq!(capture.blocks.len(), 2);
    }

    #[test]
    fn records_duplicate_and_reordered_chunks_and_rejects_incomplete_upload() {
        let mut assembler = CaptureAssembler::default();
        assembler.begin(begin()).unwrap();
        assembler
            .push(CaptureData {
                capture_id: 7,
                block_index: 1,
                batch: batch(11),
            })
            .unwrap();
        assembler
            .push(CaptureData {
                capture_id: 7,
                block_index: 1,
                batch: batch(11),
            })
            .unwrap();
        let error = assembler.finish(end()).unwrap_err();
        assert!(
            error.contains("CaptureRowOverflow")
                || error.contains("totals")
                || error.contains("missing")
        );
        assert_eq!(assembler.diagnostics().capture_duplicate_chunks, 1);
        assert_eq!(assembler.diagnostics().capture_reordered_chunks, 1);
    }

    #[test]
    fn rejects_descriptor_period_row_and_block_limit_violations_at_push_time() {
        let mut assembler = CaptureAssembler::default();
        assembler.begin(begin()).unwrap();
        assembler
            .push(CaptureData {
                capture_id: 7,
                block_index: 0,
                batch: batch(10),
            })
            .unwrap();

        let mut period_changed = batch(11);
        period_changed.sample_period_ticks = 32;
        assert!(assembler
            .push(CaptureData {
                capture_id: 7,
                block_index: 1,
                batch: period_changed,
            })
            .unwrap_err()
            .contains("CaptureDescriptorMismatch"));

        let mut oversized = CaptureAssembler::default();
        oversized.begin(begin()).unwrap();
        assert!(oversized
            .push(CaptureData {
                capture_id: 7,
                block_index: MAX_CAPTURE_BLOCKS,
                batch: batch(10),
            })
            .unwrap_err()
            .contains("CaptureTooManyBlocks"));

        let mut discontinuous = CaptureAssembler::default();
        discontinuous.begin(begin()).unwrap();
        discontinuous
            .push(CaptureData {
                capture_id: 7,
                block_index: 0,
                batch: batch(10),
            })
            .unwrap();
        assert!(discontinuous
            .push(CaptureData {
                capture_id: 7,
                block_index: 1,
                batch: batch(12),
            })
            .unwrap_err()
            .contains("CaptureRowDiscontinuity"));
    }

    #[test]
    fn rejects_integrity_dropped_rows_and_non_complete_end() {
        let mut assembler = CaptureAssembler::default();
        assembler.begin(begin()).unwrap();
        assembler
            .push(CaptureData {
                capture_id: 7,
                block_index: 0,
                batch: batch(10),
            })
            .unwrap();
        assembler
            .push(CaptureData {
                capture_id: 7,
                block_index: 1,
                batch: batch(11),
            })
            .unwrap();
        let error = assembler.finish(end()).unwrap_err();
        assert!(error.contains("CaptureIntegrityMismatch"));
        assert_eq!(assembler.diagnostics().capture_integrity_mismatches, 1);

        let mut incomplete = CaptureAssembler::default();
        incomplete.begin(begin()).unwrap();
        let mut failed = end();
        failed.state = CaptureState::Timeout;
        assert!(incomplete
            .finish(failed)
            .unwrap_err()
            .contains("non-success"));
    }
}
