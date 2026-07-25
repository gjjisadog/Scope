//! In-memory SCP1 V2 hardware-capture assembly.

use std::{collections::BTreeMap, sync::Arc};

use serde::Serialize;

use super::protocol_v2::{
    capture_integrity_summary, CaptureBegin, CaptureData, CaptureEnd, CapturePhase, CaptureState,
    MessageV2, StreamDescriptor, StreamSampleBatch, MAX_CAPTURE_BLOCKS, MAX_CAPTURE_BLOCK_ROWS,
    MAX_CAPTURE_PAYLOAD_BYTES, MAX_CAPTURE_ROWS,
};
use super::{
    protocol::Crc32c,
    protocol_v2_r2::{
        capture_data_r2_payload_len, encode_capture_data_r2_payload, CaptureDataR2,
        StreamDescriptorR2, StreamSampleBatchR2,
    },
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssembledCaptureR2 {
    pub begin: CaptureBegin,
    pub blocks: Vec<StreamSampleBatchR2>,
    pub diagnostics: CaptureDiagnostics,
}

#[derive(Clone, Debug)]
struct CaptureBlockR2 {
    data: CaptureDataR2,
    encoded_payload: Option<Arc<[u8]>>,
}

#[derive(Clone, Debug, Default)]
pub struct CaptureAssemblerR2 {
    begin: Option<CaptureBegin>,
    blocks: BTreeMap<u32, CaptureBlockR2>,
    expected_stream: Option<(
        u16,
        u32,
        super::protocol_v2::SampleDomain,
        CapturePhase,
        u16,
    )>,
    expected_next_block: u32,
    next_crc_block: u32,
    crc: Crc32c,
    total_rows: u32,
    total_payload_bytes: usize,
    diagnostics: CaptureDiagnostics,
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

impl CaptureAssemblerR2 {
    pub fn begin_with_descriptor(
        &mut self,
        begin: CaptureBegin,
        stream: &StreamDescriptorR2,
        stream_revision: u32,
    ) -> Result<(), String> {
        if begin.capture_id == 0
            || begin.row_count == 0
            || begin.row_count > MAX_CAPTURE_ROWS
            || begin.stream_id != stream.stream_id
        {
            return Err("CaptureDescriptorMismatch: invalid R2 CAPTURE_BEGIN or stream".to_owned());
        }
        self.release_buffers();
        self.crc = Crc32c::new();
        self.crc.update(&begin.capture_id.to_le_bytes());
        self.expected_stream = Some((
            stream.stream_id,
            stream_revision,
            stream.domain,
            stream.capture_phase,
            stream.consistency_group,
        ));
        self.begin = Some(begin);
        self.diagnostics = CaptureDiagnostics::default();
        Ok(())
    }

    pub fn push(&mut self, data: CaptureDataR2) -> Result<(), String> {
        let encoded_payload = Arc::<[u8]>::from(
            encode_capture_data_r2_payload(&data)
                .map_err(|error| format!("CaptureDescriptorMismatch: {error}"))?,
        );
        self.push_encoded_payload(data, encoded_payload)
    }

    pub(crate) fn push_encoded_payload(
        &mut self,
        data: CaptureDataR2,
        encoded_payload: Arc<[u8]>,
    ) -> Result<(), String> {
        let begin = self
            .begin
            .as_ref()
            .ok_or_else(|| "R2 capture data arrived before CAPTURE_BEGIN".to_owned())?;
        if data.capture_id != begin.capture_id || data.batch.stream_id != begin.stream_id {
            self.diagnostics.capture_descriptor_mismatches = self
                .diagnostics
                .capture_descriptor_mismatches
                .saturating_add(1);
            return Err("CaptureDescriptorMismatch: R2 capture id or stream changed".to_owned());
        }
        if data.block_index >= MAX_CAPTURE_BLOCKS {
            self.diagnostics.capture_too_many_blocks =
                self.diagnostics.capture_too_many_blocks.saturating_add(1);
            return Err("CaptureTooManyBlocks: R2 block index exceeds protocol limit".to_owned());
        }
        if data.batch.row_count == 0 || data.batch.row_count > MAX_CAPTURE_BLOCK_ROWS {
            self.diagnostics.capture_row_overflows =
                self.diagnostics.capture_row_overflows.saturating_add(1);
            return Err("CaptureTooLarge: R2 block row count exceeds protocol limit".to_owned());
        }
        if self.blocks.contains_key(&data.block_index) {
            self.diagnostics.capture_duplicate_chunks =
                self.diagnostics.capture_duplicate_chunks.saturating_add(1);
            return Ok(());
        }
        let Some((stream_id, revision, domain, phase, group)) = self.expected_stream else {
            return Err("CaptureDescriptorMismatch: missing R2 stream descriptor".to_owned());
        };
        if data.batch.stream_id != stream_id
            || data.batch.stream_revision != revision
            || data.batch.domain != domain
            || data.batch.capture_phase != phase
            || data.batch.consistency_group != group
        {
            self.diagnostics.capture_descriptor_mismatches = self
                .diagnostics
                .capture_descriptor_mismatches
                .saturating_add(1);
            return Err(
                "CaptureDescriptorMismatch: R2 block disagrees with STREAM_TABLE_R2".to_owned(),
            );
        }
        let payload_len = capture_data_r2_payload_len(&data)
            .map_err(|error| format!("CaptureDescriptorMismatch: {error}"))?;
        if encoded_payload.len() != payload_len {
            return Err(
                "CaptureDescriptorMismatch: R2 raw payload length disagrees with decoded block"
                    .to_owned(),
            );
        }
        let next_rows = self
            .total_rows
            .checked_add(u32::from(data.batch.row_count))
            .ok_or_else(|| "CaptureRowOverflow: R2 row total overflow".to_owned())?;
        if next_rows > begin.row_count || next_rows > MAX_CAPTURE_ROWS {
            self.diagnostics.capture_row_overflows =
                self.diagnostics.capture_row_overflows.saturating_add(1);
            return Err("CaptureRowOverflow: R2 rows exceed CAPTURE_BEGIN".to_owned());
        }
        let next_payload = checked_r2_capture_payload_total(self.total_payload_bytes, payload_len)?;
        self.validate_neighbors(&data)?;
        if data.block_index != self.expected_next_block {
            self.diagnostics.capture_reordered_chunks =
                self.diagnostics.capture_reordered_chunks.saturating_add(1);
        }
        self.expected_next_block = self
            .expected_next_block
            .max(data.block_index.saturating_add(1));
        self.total_rows = next_rows;
        self.total_payload_bytes = next_payload;
        self.blocks.insert(
            data.block_index,
            CaptureBlockR2 {
                data,
                encoded_payload: Some(encoded_payload),
            },
        );
        self.advance_crc();
        Ok(())
    }

    pub fn finish(&mut self, end: CaptureEnd) -> Result<AssembledCaptureR2, String> {
        let result = self.finish_inner(end);
        self.release_buffers();
        result
    }

    fn finish_inner(&mut self, end: CaptureEnd) -> Result<AssembledCaptureR2, String> {
        let begin = self
            .begin
            .take()
            .ok_or_else(|| "R2 CAPTURE_END arrived before CAPTURE_BEGIN".to_owned())?;
        if end.capture_id != begin.capture_id || end.state != CaptureState::Complete {
            return Err("R2 capture ended with an invalid id or non-success state".to_owned());
        }
        let actual_blocks =
            u32::try_from(self.blocks.len()).map_err(|_| "too many R2 capture blocks")?;
        if end.total_blocks != actual_blocks
            || end.total_blocks > MAX_CAPTURE_BLOCKS
            || end.uploaded_rows != self.total_rows
            || end.total_samples != self.total_rows
            || end.dropped_rows != 0
            || self.total_rows != begin.row_count
        {
            return Err("R2 CAPTURE_END totals do not match uploaded blocks".to_owned());
        }
        if self.next_crc_block != end.total_blocks {
            self.diagnostics.capture_missing_chunks =
                self.diagnostics.capture_missing_chunks.saturating_add(1);
            return Err("R2 capture upload is missing one or more chunks".to_owned());
        }
        if self.crc.finalize() != end.integrity_summary {
            self.diagnostics.capture_integrity_mismatches = self
                .diagnostics
                .capture_integrity_mismatches
                .saturating_add(1);
            return Err(
                "CaptureIntegrityMismatch: R2 incremental CRC32C does not match".to_owned(),
            );
        }
        let first = self
            .blocks
            .first_key_value()
            .and_then(|(_, block)| block.data.batch.row_metadata.first())
            .map(|row| row.row_sequence)
            .ok_or_else(|| "CaptureRowDiscontinuity: R2 capture has no rows".to_owned())?;
        let last = self
            .blocks
            .last_key_value()
            .and_then(|(_, block)| block.data.batch.row_metadata.last())
            .map(|row| row.row_sequence)
            .ok_or_else(|| "CaptureRowDiscontinuity: R2 capture has no rows".to_owned())?;
        if begin.trigger_row_seq < first || begin.trigger_row_seq > last {
            return Err("CaptureRowDiscontinuity: R2 trigger row is outside capture".to_owned());
        }
        self.diagnostics.capture_complete = true;
        let blocks = std::mem::take(&mut self.blocks)
            .into_values()
            .map(|block| block.data.batch)
            .collect();
        Ok(AssembledCaptureR2 {
            begin,
            blocks,
            diagnostics: self.diagnostics.clone(),
        })
    }

    fn validate_neighbors(&mut self, data: &CaptureDataR2) -> Result<(), String> {
        let first = data
            .batch
            .row_metadata
            .first()
            .ok_or_else(|| "CaptureRowDiscontinuity: R2 block has no rows".to_owned())?
            .row_sequence;
        let last = data
            .batch
            .row_metadata
            .last()
            .ok_or_else(|| "CaptureRowDiscontinuity: R2 block has no rows".to_owned())?
            .row_sequence;
        if let Some((predecessor_index, predecessor)) =
            self.blocks.range(..data.block_index).next_back()
        {
            let previous_last = predecessor
                .data
                .batch
                .row_metadata
                .last()
                .expect("validated R2 capture block")
                .row_sequence;
            if predecessor_index.checked_add(1) == Some(data.block_index)
                && previous_last.checked_add(1) != Some(first)
            {
                self.diagnostics.capture_row_discontinuities = self
                    .diagnostics
                    .capture_row_discontinuities
                    .saturating_add(1);
                return Err("CaptureRowDiscontinuity: R2 predecessor is not adjacent".to_owned());
            }
        }
        if let Some((successor_index, successor)) = self
            .blocks
            .range(data.block_index.saturating_add(1)..)
            .next()
        {
            let next_first = successor
                .data
                .batch
                .row_metadata
                .first()
                .expect("validated R2 capture block")
                .row_sequence;
            if data.block_index.checked_add(1) == Some(*successor_index)
                && last.checked_add(1) != Some(next_first)
            {
                self.diagnostics.capture_row_discontinuities = self
                    .diagnostics
                    .capture_row_discontinuities
                    .saturating_add(1);
                return Err("CaptureRowDiscontinuity: R2 successor is not adjacent".to_owned());
            }
        }
        Ok(())
    }

    fn advance_crc(&mut self) {
        while let Some(block) = self.blocks.get_mut(&self.next_crc_block) {
            let Some(encoded_payload) = block.encoded_payload.take() else {
                break;
            };
            self.crc.update(&encoded_payload);
            self.next_crc_block = self.next_crc_block.saturating_add(1);
        }
    }

    pub fn device_reset(&mut self) {
        self.release_buffers();
        self.diagnostics.capture_complete = false;
    }

    pub fn diagnostics(&self) -> &CaptureDiagnostics {
        &self.diagnostics
    }

    pub fn buffered_block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn buffered_rows(&self) -> u32 {
        self.total_rows
    }

    pub fn buffered_payload_bytes(&self) -> usize {
        self.total_payload_bytes
    }

    fn release_buffers(&mut self) {
        self.begin = None;
        self.blocks.clear();
        self.expected_stream = None;
        self.expected_next_block = 0;
        self.next_crc_block = 0;
        self.crc = Crc32c::new();
        self.total_rows = 0;
        self.total_payload_bytes = 0;
    }
}

fn checked_r2_capture_payload_total(current: usize, block: usize) -> Result<usize, String> {
    let buffered = current
        .checked_add(block)
        .ok_or_else(|| "CaptureTooLarge: R2 payload length overflow".to_owned())?;
    let integrity_input = std::mem::size_of::<u32>()
        .checked_add(buffered)
        .ok_or_else(|| "CaptureTooLarge: R2 payload length overflow".to_owned())?;
    if integrity_input > MAX_CAPTURE_PAYLOAD_BYTES {
        return Err("CaptureTooLarge: R2 capture exceeds the 64 MiB limit".to_owned());
    }
    Ok(buffered)
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
        let result = self.finish_inner(end);
        self.release_buffers();
        result
    }

    fn finish_inner(&mut self, end: CaptureEnd) -> Result<AssembledCapture, String> {
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
        self.release_buffers();
        self.diagnostics.capture_complete = false;
    }

    pub fn diagnostics(&self) -> &CaptureDiagnostics {
        &self.diagnostics
    }

    pub fn buffered_block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn buffered_payload_bytes(&self) -> usize {
        self.total_payload_bytes
    }

    fn release_buffers(&mut self) {
        self.begin = None;
        self.blocks.clear();
        self.descriptor = None;
        self.expected_descriptor = None;
        self.expected_next_block = 0;
        self.total_rows = 0;
        self.total_payload_bytes = 0;
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
        protocol_v2_r2::{capture_integrity_summary_r2, MetadataEncodingR2, StreamDescriptorR2},
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
                logical_cycle_sequence: row,
                source_sequence: row,
                applied_sequence: row.saturating_sub(1),
                valid_flags: SNAPSHOT_VALID | FROZEN_ROW | APPLIED_SEQUENCE_VALID,
            }],
        }
    }

    fn r2_descriptor() -> StreamDescriptorR2 {
        StreamDescriptorR2 {
            stream_id: 1,
            domain: SampleDomain::Fast32k,
            capture_phase: CapturePhase::AfterClaComplete,
            sample_rate_hz: 32_000,
            consistency_group: 1,
            logical_cycle_step: 1,
            channel_ids: vec![0],
        }
    }

    fn r2_block(capture_id: u32, block_index: u32, row: u64) -> CaptureDataR2 {
        CaptureDataR2 {
            capture_id,
            block_index,
            batch: StreamSampleBatchR2 {
                stream_id: 1,
                stream_revision: 3,
                domain: SampleDomain::Fast32k,
                capture_phase: CapturePhase::AfterClaComplete,
                consistency_group: 1,
                first_row_sequence: row,
                row_sequence_step: 1,
                logical_cycle_step: 1,
                sample_period_ticks: 1_000,
                row_count: 1,
                channel_ids: vec![0],
                sample_data: 0_i16.to_le_bytes().to_vec(),
                metadata_encoding: MetadataEncodingR2::AffineWithOverrides,
                row_metadata: vec![SnapshotMeta {
                    row_sequence: row,
                    logical_cycle_sequence: row,
                    source_sequence: row,
                    applied_sequence: row,
                    valid_flags: SNAPSHOT_VALID | FROZEN_ROW | APPLIED_SEQUENCE_VALID,
                }],
            },
        }
    }

    #[test]
    fn r2_out_of_order_blocks_use_incremental_crc_and_release_buffers() {
        let mut assembler = CaptureAssemblerR2::default();
        assembler
            .begin_with_descriptor(
                CaptureBegin {
                    capture_id: 7,
                    stream_id: 1,
                    row_count: 3,
                    trigger_row_seq: 1,
                },
                &r2_descriptor(),
                3,
            )
            .unwrap();
        let first = r2_block(7, 0, 1);
        let second = r2_block(7, 1, 2);
        let third = r2_block(7, 2, 3);
        assembler.push(third.clone()).unwrap();
        assembler.push(first.clone()).unwrap();
        assembler.push(second.clone()).unwrap();
        assert_eq!(assembler.buffered_block_count(), 3);
        assert_eq!(assembler.buffered_rows(), 3);
        let integrity = capture_integrity_summary_r2(7, [&first, &second, &third]).unwrap();
        let capture = assembler
            .finish(CaptureEnd {
                capture_id: 7,
                state: CaptureState::Complete,
                uploaded_rows: 3,
                dropped_rows: 0,
                total_blocks: 3,
                total_samples: 3,
                integrity_summary: integrity,
            })
            .unwrap();
        assert!(capture.diagnostics.capture_complete);
        assert_eq!(capture.blocks.len(), 3);
        assert_eq!(assembler.buffered_block_count(), 0);
        assert_eq!(assembler.buffered_rows(), 0);
        assert_eq!(assembler.buffered_payload_bytes(), 0);
    }

    #[test]
    fn r2_capture_failure_releases_memory_and_next_capture_succeeds() {
        let mut assembler = CaptureAssemblerR2::default();
        let begin = CaptureBegin {
            capture_id: 8,
            stream_id: 1,
            row_count: 2,
            trigger_row_seq: 1,
        };
        assembler
            .begin_with_descriptor(begin, &r2_descriptor(), 3)
            .unwrap();
        assembler.push(r2_block(8, 1, 2)).unwrap();
        assert!(assembler
            .finish(CaptureEnd {
                capture_id: 8,
                state: CaptureState::Complete,
                uploaded_rows: 1,
                dropped_rows: 0,
                total_blocks: 2,
                total_samples: 1,
                integrity_summary: 0,
            })
            .is_err());
        assert_eq!(assembler.buffered_block_count(), 0);

        assembler
            .begin_with_descriptor(begin, &r2_descriptor(), 3)
            .unwrap();
        let first = r2_block(8, 0, 1);
        let second = r2_block(8, 1, 2);
        assembler.push(first.clone()).unwrap();
        assembler.push(second.clone()).unwrap();
        let integrity = capture_integrity_summary_r2(8, [&first, &second]).unwrap();
        assert!(assembler
            .finish(CaptureEnd {
                capture_id: 8,
                state: CaptureState::Complete,
                uploaded_rows: 2,
                dropped_rows: 0,
                total_blocks: 2,
                total_samples: 2,
                integrity_summary: integrity,
            })
            .is_ok());
    }

    #[test]
    fn r2_capture_limit_is_checked_before_allocating_a_block() {
        assert_eq!(
            checked_r2_capture_payload_total(MAX_CAPTURE_PAYLOAD_BYTES - 5, 1).unwrap(),
            MAX_CAPTURE_PAYLOAD_BYTES - 4
        );
        assert!(
            checked_r2_capture_payload_total(MAX_CAPTURE_PAYLOAD_BYTES - 5, 2)
                .unwrap_err()
                .contains("64 MiB")
        );
        assert!(checked_r2_capture_payload_total(usize::MAX, 1)
            .unwrap_err()
            .contains("overflow"));
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

    #[test]
    fn every_capture_end_releases_buffered_blocks() {
        let mut assembler = CaptureAssembler::default();
        assembler.begin(begin()).unwrap();
        assembler
            .push(CaptureData {
                capture_id: 7,
                block_index: 0,
                batch: batch(10),
            })
            .unwrap();
        assert_eq!(assembler.buffered_block_count(), 1);
        assert!(assembler.buffered_payload_bytes() > 0);

        let mut failed = end();
        failed.state = CaptureState::Timeout;
        assert!(assembler.finish(failed).is_err());
        assert_eq!(assembler.buffered_block_count(), 0);
        assert_eq!(assembler.buffered_payload_bytes(), 0);
    }
}
