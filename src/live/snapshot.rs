//! Frozen SCP1 V2 sampling-row metadata and client-side consistency checks.
//!
//! The checker intentionally does not reconstruct a row from scattered
//! variables.  It only accepts the row frozen and emitted by the DSP, then
//! records semantic defects for diagnostics.

use serde::Serialize;

use super::protocol_v2::{SampleDomain, StreamDescriptor};

pub const SNAPSHOT_VALID: u32 = 1 << 0;
pub const SOURCE_SEQUENCE_VALID: u32 = 1 << 1;
pub const APPLIED_SEQUENCE_VALID: u32 = 1 << 2;
pub const CLA_RESULT_VALID: u32 = 1 << 3;
pub const ADC_SAMPLE_VALID: u32 = 1 << 4;
pub const FROZEN_ROW: u32 = 1 << 5;
pub const SNAPSHOT_KNOWN_FLAGS: u32 = SNAPSHOT_VALID
    | SOURCE_SEQUENCE_VALID
    | APPLIED_SEQUENCE_VALID
    | CLA_RESULT_VALID
    | ADC_SAMPLE_VALID
    | FROZEN_ROW;

/// Metadata attached once to every DSP-frozen stream row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SnapshotMeta {
    pub row_sequence: u64,
    pub source_sequence: u64,
    pub applied_sequence: u64,
    pub valid_flags: u32,
}

impl SnapshotMeta {
    pub const ENCODED_LEN: usize = 28;

    pub const fn is_frozen(self) -> bool {
        self.valid_flags & (SNAPSHOT_VALID | FROZEN_ROW) == (SNAPSHOT_VALID | FROZEN_ROW)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SnapshotDiagnostics {
    pub row_sequence_gaps: u64,
    pub row_sequence_reorders: u64,
    pub source_sequence_faults: u64,
    pub applied_sequence_faults: u64,
    pub invalid_snapshot_rows: u64,
}

#[derive(Clone, Debug, Default)]
pub struct SnapshotValidator {
    last_row_sequence: Option<u64>,
    last_source_sequence: Option<u64>,
    last_applied_sequence: Option<u64>,
    diagnostics: SnapshotDiagnostics,
}

impl SnapshotValidator {
    pub fn diagnostics(&self) -> &SnapshotDiagnostics {
        &self.diagnostics
    }

    pub fn observe(&mut self, descriptor: &StreamDescriptor, meta: SnapshotMeta) {
        if !meta.is_frozen()
            || meta.valid_flags & !SNAPSHOT_KNOWN_FLAGS != 0
            || (descriptor.domain == SampleDomain::Fast32k
                && meta.valid_flags & CLA_RESULT_VALID == 0)
        {
            self.diagnostics.invalid_snapshot_rows =
                self.diagnostics.invalid_snapshot_rows.saturating_add(1);
        }

        if let Some(previous) = self.last_row_sequence {
            if meta.row_sequence <= previous {
                self.diagnostics.row_sequence_reorders =
                    self.diagnostics.row_sequence_reorders.saturating_add(1);
            } else if meta.row_sequence != previous.saturating_add(1) {
                self.diagnostics.row_sequence_gaps =
                    self.diagnostics.row_sequence_gaps.saturating_add(1);
            }
        }

        if meta.valid_flags & SOURCE_SEQUENCE_VALID != 0 {
            if self
                .last_source_sequence
                .is_some_and(|previous| meta.source_sequence < previous)
                || matches!(
                    descriptor.domain,
                    SampleDomain::Fast32k | SampleDomain::Control8k
                ) && meta.source_sequence > meta.row_sequence
            {
                self.diagnostics.source_sequence_faults =
                    self.diagnostics.source_sequence_faults.saturating_add(1);
            }
            self.last_source_sequence = Some(meta.source_sequence);
        } else {
            self.diagnostics.source_sequence_faults =
                self.diagnostics.source_sequence_faults.saturating_add(1);
        }

        if meta.valid_flags & APPLIED_SEQUENCE_VALID != 0 {
            let future = matches!(
                descriptor.domain,
                SampleDomain::Fast32k | SampleDomain::Control8k
            ) && meta.applied_sequence >= meta.row_sequence;
            let reversed = self
                .last_applied_sequence
                .is_some_and(|previous| meta.applied_sequence < previous);
            if future || reversed {
                self.diagnostics.applied_sequence_faults =
                    self.diagnostics.applied_sequence_faults.saturating_add(1);
            }
            self.last_applied_sequence = Some(meta.applied_sequence);
        } else {
            self.diagnostics.applied_sequence_faults =
                self.diagnostics.applied_sequence_faults.saturating_add(1);
        }

        self.last_row_sequence = Some(meta.row_sequence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::protocol_v2::{CapturePhase, StreamDescriptor};

    fn fast() -> StreamDescriptor {
        StreamDescriptor {
            stream_id: 1,
            domain: SampleDomain::Fast32k,
            capture_phase: CapturePhase::AfterClaComplete,
            sample_rate_hz: 32_000,
            consistency_group: 1,
            channel_ids: vec![1],
        }
    }

    fn valid(row_sequence: u64) -> SnapshotMeta {
        SnapshotMeta {
            row_sequence,
            source_sequence: row_sequence,
            applied_sequence: row_sequence.saturating_sub(1),
            valid_flags: SNAPSHOT_VALID
                | SOURCE_SEQUENCE_VALID
                | APPLIED_SEQUENCE_VALID
                | CLA_RESULT_VALID
                | ADC_SAMPLE_VALID
                | FROZEN_ROW,
        }
    }

    #[test]
    fn accepts_a_legal_one_cycle_pipeline() {
        let mut validator = SnapshotValidator::default();
        validator.observe(&fast(), valid(11));
        validator.observe(&fast(), valid(12));
        assert_eq!(validator.diagnostics(), &SnapshotDiagnostics::default());
    }

    #[test]
    fn records_row_and_validity_defects_without_stitching_rows() {
        let mut validator = SnapshotValidator::default();
        validator.observe(&fast(), valid(10));
        let mut invalid = valid(12);
        invalid.valid_flags &= !(FROZEN_ROW | CLA_RESULT_VALID);
        invalid.source_sequence = 13;
        invalid.applied_sequence = 12;
        validator.observe(&fast(), invalid);
        let diagnostics = validator.diagnostics();
        assert_eq!(diagnostics.row_sequence_gaps, 1);
        assert_eq!(diagnostics.source_sequence_faults, 1);
        assert_eq!(diagnostics.applied_sequence_faults, 1);
        assert_eq!(diagnostics.invalid_snapshot_rows, 1);
    }
}
