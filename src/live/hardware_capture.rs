//! In-memory SCP1 V2 hardware-capture assembly.

use std::collections::BTreeMap;

use serde::Serialize;

use super::protocol_v2::{CaptureBegin, CaptureData, CaptureEnd, CaptureState, StreamSampleBatch};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CaptureDiagnostics {
    pub capture_complete: bool,
    pub capture_missing_chunks: u32,
    pub capture_duplicate_chunks: u32,
    pub capture_reordered_chunks: u32,
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
    blocks: BTreeMap<u32, StreamSampleBatch>,
    expected_next_block: u32,
    diagnostics: CaptureDiagnostics,
}

impl CaptureAssembler {
    pub fn begin(&mut self, begin: CaptureBegin) -> Result<(), String> {
        if begin.capture_id == 0 || begin.row_count == 0 {
            return Err("capture begin has an invalid id or row count".to_owned());
        }
        self.begin = Some(begin);
        self.blocks.clear();
        self.expected_next_block = 0;
        self.diagnostics = CaptureDiagnostics::default();
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
            return Err("capture data stream does not match CAPTURE_BEGIN".to_owned());
        }
        if self.blocks.contains_key(&data.block_index) {
            self.diagnostics.capture_duplicate_chunks =
                self.diagnostics.capture_duplicate_chunks.saturating_add(1);
            return Ok(());
        }
        if data.block_index != self.expected_next_block {
            self.diagnostics.capture_reordered_chunks =
                self.diagnostics.capture_reordered_chunks.saturating_add(1);
        }
        self.expected_next_block = self
            .expected_next_block
            .max(data.block_index.saturating_add(1));
        self.blocks.insert(data.block_index, data.batch);
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
        let actual_blocks =
            u32::try_from(self.blocks.len()).map_err(|_| "too many capture blocks")?;
        for block_index in 0..expected_blocks {
            if !self.blocks.contains_key(&block_index) {
                self.diagnostics.capture_missing_chunks =
                    self.diagnostics.capture_missing_chunks.saturating_add(1);
            }
        }
        let sample_count = self.blocks.values().try_fold(0_u32, |total, batch| {
            total
                .checked_add(u32::from(batch.row_count))
                .ok_or("capture sample count overflow")
        })?;
        if actual_blocks != expected_blocks
            || sample_count != end.total_samples
            || sample_count != begin.row_count
        {
            return Err("CAPTURE_END block or sample totals do not match uploaded data".to_owned());
        }
        if self.diagnostics.capture_missing_chunks != 0 {
            return Err("capture upload is missing one or more chunks".to_owned());
        }
        self.diagnostics.capture_complete = true;
        let blocks = std::mem::take(&mut self.blocks).into_values().collect();
        Ok(AssembledCapture {
            begin,
            blocks,
            diagnostics: self.diagnostics.clone(),
        })
    }

    pub fn device_reset(&mut self) {
        self.begin = None;
        self.blocks.clear();
        self.diagnostics.capture_complete = false;
    }

    pub fn diagnostics(&self) -> &CaptureDiagnostics {
        &self.diagnostics
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
            integrity_summary: 0x1234,
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
        let capture = assembler.finish(end()).unwrap();
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
        assert!(error.contains("totals") || error.contains("missing"));
        assert_eq!(assembler.diagnostics().capture_duplicate_chunks, 1);
        assert_eq!(assembler.diagnostics().capture_reordered_chunks, 1);
    }
}
