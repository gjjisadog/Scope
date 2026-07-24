//! Frozen SCP1 V2 sampling-row metadata and client-side consistency checks.
//!
//! The checker intentionally does not reconstruct a row from scattered
//! variables.  It only accepts the row frozen and emitted by the DSP, then
//! records semantic defects for diagnostics.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::protocol_v2::{CausalRelation, SampleDomain, SignalRole, StreamDescriptor, StreamTable};

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
pub const MAX_CAUSAL_CACHE_ROWS: usize = 4_096;

/// Metadata attached once to every DSP-frozen stream row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SnapshotMeta {
    pub row_sequence: u64,
    pub logical_cycle_sequence: u64,
    pub source_sequence: u64,
    pub applied_sequence: u64,
    pub valid_flags: u32,
}

impl SnapshotMeta {
    pub const ENCODED_LEN: usize = 36;

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
    pub missing_causal_source: u64,
    pub causal_source_mismatch: u64,
    pub causal_application_mismatch: u64,
    pub causal_sequence_reorder: u64,
    pub causal_group_mismatch: u64,
    pub causal_cache_evictions: u64,
}

type RelationKey = (u16, u16, u16);

#[derive(Clone, Debug)]
pub struct SnapshotValidator {
    streams: BTreeMap<u16, StreamSequenceState>,
    /// Rows are keyed by stream-local *logical control-cycle sequence*.  They
    /// are never compared as physical timestamps across sampling domains.
    causal_rows: BTreeMap<(u16, u64), SnapshotMeta>,
    pending_results: BTreeSet<(RelationKey, u64)>,
    pending_applications: BTreeSet<(RelationKey, u64, u64)>,
    causal_last_result: BTreeMap<RelationKey, u64>,
    causal_last_application: BTreeMap<RelationKey, u64>,
    causal_capacity: usize,
    diagnostics: SnapshotDiagnostics,
}

#[derive(Clone, Debug, Default)]
struct StreamSequenceState {
    last_row_sequence: Option<u64>,
    last_source_sequence: Option<u64>,
    last_applied_sequence: Option<u64>,
}

impl Default for SnapshotValidator {
    fn default() -> Self {
        Self::with_causal_capacity(MAX_CAUSAL_CACHE_ROWS)
    }
}

impl SnapshotValidator {
    pub fn with_causal_capacity(causal_capacity: usize) -> Self {
        Self {
            streams: BTreeMap::new(),
            causal_rows: BTreeMap::new(),
            pending_results: BTreeSet::new(),
            pending_applications: BTreeSet::new(),
            causal_last_result: BTreeMap::new(),
            causal_last_application: BTreeMap::new(),
            causal_capacity: causal_capacity.max(1),
            diagnostics: SnapshotDiagnostics::default(),
        }
    }

    pub fn diagnostics(&self) -> &SnapshotDiagnostics {
        &self.diagnostics
    }

    pub fn causal_cache_len(&self) -> usize {
        self.causal_rows.len()
    }

    pub fn pending_causal_match_count(&self) -> usize {
        self.pending_results.len() + self.pending_applications.len()
    }

    pub fn observe(&mut self, descriptor: &StreamDescriptor, meta: SnapshotMeta) {
        self.observe_inner(descriptor, meta, false, false);
        // Compatibility-only local diagnostics for callers that have not
        // negotiated a STREAM_TABLE.  The table-aware path intentionally does
        // not apply this rule: causal offsets are defined by CausalRelation.
        if meta.valid_flags & SOURCE_SEQUENCE_VALID != 0
            && meta.source_sequence > meta.logical_cycle_sequence
        {
            self.diagnostics.source_sequence_faults =
                self.diagnostics.source_sequence_faults.saturating_add(1);
        }
        if meta.valid_flags & APPLIED_SEQUENCE_VALID != 0
            && meta.applied_sequence >= meta.logical_cycle_sequence
        {
            self.diagnostics.applied_sequence_faults =
                self.diagnostics.applied_sequence_faults.saturating_add(1);
        }
    }

    /// Observes one frozen row using the negotiated STREAM_TABLE.  Relations
    /// are evaluated strictly in their declared consistency group and use
    /// logical control-cycle sequences, never the wall-clock coincidence of
    /// a 32 kHz, 8 kHz, or 1 kHz row.
    pub fn observe_with_table(
        &mut self,
        table: &StreamTable,
        descriptor: &StreamDescriptor,
        meta: SnapshotMeta,
    ) {
        let source_required = matches!(
            descriptor.domain,
            SampleDomain::Fast32k | SampleDomain::Control8k
        ) || table
            .causal_relations
            .iter()
            .any(|relation| relation.result_stream_id == descriptor.stream_id);
        let applied_required = table.bindings.iter().any(|binding| {
            binding.stream_id == descriptor.stream_id && binding.role == SignalRole::AppliedCommand
        }) || table
            .causal_relations
            .iter()
            .any(|relation| relation.application_stream_id == descriptor.stream_id);
        self.observe_inner(descriptor, meta, source_required, applied_required);
        let participates_in_causal_relation = table.causal_relations.iter().any(|relation| {
            relation.input_stream_id == descriptor.stream_id
                || relation.result_stream_id == descriptor.stream_id
                || relation.application_stream_id == descriptor.stream_id
        });
        if !participates_in_causal_relation {
            return;
        }
        self.causal_rows
            .insert((descriptor.stream_id, meta.logical_cycle_sequence), meta);
        for relation in &table.causal_relations {
            self.observe_relation(table, descriptor, meta, relation);
        }
        self.enforce_causal_bounds();
    }

    fn observe_inner(
        &mut self,
        descriptor: &StreamDescriptor,
        meta: SnapshotMeta,
        source_required: bool,
        applied_required: bool,
    ) {
        let required_flags = required_flags(descriptor, source_required, applied_required);
        if !meta.is_frozen()
            || meta.valid_flags & !SNAPSHOT_KNOWN_FLAGS != 0
            || meta.valid_flags & required_flags != required_flags
        {
            self.diagnostics.invalid_snapshot_rows =
                self.diagnostics.invalid_snapshot_rows.saturating_add(1);
        }
        let state = self.streams.entry(descriptor.stream_id).or_default();
        if let Some(previous) = state.last_row_sequence {
            if meta.row_sequence <= previous {
                self.diagnostics.row_sequence_reorders =
                    self.diagnostics.row_sequence_reorders.saturating_add(1);
            } else if meta.row_sequence != previous.saturating_add(1) {
                self.diagnostics.row_sequence_gaps =
                    self.diagnostics.row_sequence_gaps.saturating_add(1);
            }
        }

        if meta.valid_flags & SOURCE_SEQUENCE_VALID != 0 {
            if state
                .last_source_sequence
                .is_some_and(|previous| meta.source_sequence < previous)
            {
                self.diagnostics.source_sequence_faults =
                    self.diagnostics.source_sequence_faults.saturating_add(1);
            }
            state.last_source_sequence = Some(meta.source_sequence);
        } else if source_required {
            self.diagnostics.source_sequence_faults =
                self.diagnostics.source_sequence_faults.saturating_add(1);
        }

        if meta.valid_flags & APPLIED_SEQUENCE_VALID != 0 {
            let reversed = state
                .last_applied_sequence
                .is_some_and(|previous| meta.applied_sequence < previous);
            if reversed {
                self.diagnostics.applied_sequence_faults =
                    self.diagnostics.applied_sequence_faults.saturating_add(1);
            }
            state.last_applied_sequence = Some(meta.applied_sequence);
        } else if applied_required {
            self.diagnostics.applied_sequence_faults =
                self.diagnostics.applied_sequence_faults.saturating_add(1);
        }

        state.last_row_sequence = Some(meta.row_sequence);
    }

    fn observe_relation(
        &mut self,
        table: &StreamTable,
        descriptor: &StreamDescriptor,
        meta: SnapshotMeta,
        relation: &CausalRelation,
    ) {
        if !relation_group_matches(table, relation, descriptor) {
            self.diagnostics.causal_group_mismatch =
                self.diagnostics.causal_group_mismatch.saturating_add(1);
            return;
        }
        let key = (
            relation.input_stream_id,
            relation.result_stream_id,
            relation.application_stream_id,
        );
        if descriptor.stream_id == relation.result_stream_id {
            if self
                .causal_last_result
                .insert(key, meta.logical_cycle_sequence)
                .is_some_and(|previous| meta.logical_cycle_sequence <= previous)
            {
                self.diagnostics.causal_sequence_reorder =
                    self.diagnostics.causal_sequence_reorder.saturating_add(1);
            }
            if let Some(input) = self
                .causal_rows
                .get(&(relation.input_stream_id, meta.logical_cycle_sequence))
                .copied()
            {
                self.validate_result_match(input, meta, relation);
            } else {
                self.pending_results
                    .insert((key, meta.logical_cycle_sequence));
            }
            self.resolve_pending_application(key, meta.logical_cycle_sequence, relation);
        }
        if descriptor.stream_id == relation.input_stream_id
            && self
                .pending_results
                .remove(&(key, meta.logical_cycle_sequence))
        {
            if let Some(result) = self
                .causal_rows
                .get(&(relation.result_stream_id, meta.logical_cycle_sequence))
                .copied()
            {
                self.validate_result_match(meta, result, relation);
            } else {
                self.record_missing_causal_source();
            }
        }
        if descriptor.stream_id == relation.application_stream_id {
            if self
                .causal_last_application
                .insert(key, meta.logical_cycle_sequence)
                .is_some_and(|previous| meta.logical_cycle_sequence <= previous)
            {
                self.diagnostics.causal_sequence_reorder =
                    self.diagnostics.causal_sequence_reorder.saturating_add(1);
            }
            let Some(result_sequence) = add_offset(
                meta.logical_cycle_sequence,
                -relation.application_result_offset,
            ) else {
                self.diagnostics.causal_application_mismatch = self
                    .diagnostics
                    .causal_application_mismatch
                    .saturating_add(1);
                return;
            };
            if let Some(result) = self
                .causal_rows
                .get(&(relation.result_stream_id, result_sequence))
            {
                self.validate_application_match(*result, meta, relation);
            } else {
                self.pending_applications.insert((
                    key,
                    result_sequence,
                    meta.logical_cycle_sequence,
                ));
            }
        }
    }

    fn validate_result_match(
        &mut self,
        input: SnapshotMeta,
        result: SnapshotMeta,
        relation: &CausalRelation,
    ) {
        let expected = add_offset(input.logical_cycle_sequence, relation.result_input_offset);
        if result.valid_flags & SOURCE_SEQUENCE_VALID == 0
            || expected != Some(result.source_sequence)
        {
            self.diagnostics.causal_source_mismatch =
                self.diagnostics.causal_source_mismatch.saturating_add(1);
        }
    }

    fn validate_application_match(
        &mut self,
        result: SnapshotMeta,
        application: SnapshotMeta,
        relation: &CausalRelation,
    ) {
        let expected = add_offset(
            result.logical_cycle_sequence,
            relation.application_result_offset,
        );
        if application.valid_flags & APPLIED_SEQUENCE_VALID == 0
            || expected != Some(application.applied_sequence)
        {
            self.diagnostics.causal_application_mismatch = self
                .diagnostics
                .causal_application_mismatch
                .saturating_add(1);
        }
    }

    fn resolve_pending_application(
        &mut self,
        key: RelationKey,
        result_sequence: u64,
        relation: &CausalRelation,
    ) {
        let pending = self
            .pending_applications
            .iter()
            .filter(|(pending_key, pending_result, _)| {
                *pending_key == key && *pending_result == result_sequence
            })
            .copied()
            .collect::<Vec<_>>();
        for entry @ (_, _, application_sequence) in pending {
            self.pending_applications.remove(&entry);
            if let Some(application) = self
                .causal_rows
                .get(&(relation.application_stream_id, application_sequence))
                .copied()
            {
                if let Some(result) = self
                    .causal_rows
                    .get(&(relation.result_stream_id, result_sequence))
                    .copied()
                {
                    self.validate_application_match(result, application, relation);
                }
            } else {
                self.record_missing_causal_source();
            }
        }
    }

    fn enforce_causal_bounds(&mut self) {
        while self.causal_rows.len() > self.causal_capacity {
            let Some(oldest) = self
                .causal_rows
                .keys()
                .min_by_key(|(_, logical_cycle_sequence)| *logical_cycle_sequence)
                .copied()
            else {
                break;
            };
            self.causal_rows.remove(&oldest);
            self.diagnostics.causal_cache_evictions =
                self.diagnostics.causal_cache_evictions.saturating_add(1);
        }
        while self.pending_causal_match_count() > self.causal_capacity {
            let result_oldest = self
                .pending_results
                .iter()
                .min_by_key(|(_, logical_cycle_sequence)| *logical_cycle_sequence)
                .copied();
            let application_oldest = self
                .pending_applications
                .iter()
                .min_by_key(|(_, _, application_sequence)| *application_sequence)
                .copied();
            match (result_oldest, application_oldest) {
                (Some(result), Some(application)) if result.1 <= application.2 => {
                    self.pending_results.remove(&result);
                }
                (_, Some(application)) => {
                    self.pending_applications.remove(&application);
                }
                (Some(result), None) => {
                    self.pending_results.remove(&result);
                }
                (None, None) => break,
            }
            self.record_missing_causal_source();
        }
    }

    fn record_missing_causal_source(&mut self) {
        self.diagnostics.missing_causal_source =
            self.diagnostics.missing_causal_source.saturating_add(1);
    }
}

fn required_flags(
    descriptor: &StreamDescriptor,
    source_required: bool,
    applied_required: bool,
) -> u32 {
    let mut flags = SNAPSHOT_VALID | FROZEN_ROW;
    match descriptor.domain {
        SampleDomain::Fast32k => {
            flags |= SOURCE_SEQUENCE_VALID | ADC_SAMPLE_VALID | CLA_RESULT_VALID;
        }
        SampleDomain::Control8k => flags |= SOURCE_SEQUENCE_VALID,
        SampleDomain::Slow1k => {}
    }
    if source_required {
        flags |= SOURCE_SEQUENCE_VALID;
    }
    if applied_required {
        flags |= APPLIED_SEQUENCE_VALID;
    }
    flags
}

fn relation_group_matches(
    table: &StreamTable,
    relation: &CausalRelation,
    descriptor: &StreamDescriptor,
) -> bool {
    let Some(input) = table.stream(relation.input_stream_id) else {
        return false;
    };
    let Some(result) = table.stream(relation.result_stream_id) else {
        return false;
    };
    let Some(application) = table.stream(relation.application_stream_id) else {
        return false;
    };
    input.consistency_group == result.consistency_group
        && result.consistency_group == application.consistency_group
        && descriptor.consistency_group == input.consistency_group
}

fn add_offset(sequence: u64, offset: i16) -> Option<u64> {
    if offset >= 0 {
        sequence.checked_add(offset as u64)
    } else {
        sequence.checked_sub(u64::from(offset.unsigned_abs()))
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
        valid_with_cycle(row_sequence, row_sequence)
    }

    fn valid_with_cycle(row_sequence: u64, logical_cycle_sequence: u64) -> SnapshotMeta {
        SnapshotMeta {
            row_sequence,
            logical_cycle_sequence,
            source_sequence: logical_cycle_sequence,
            applied_sequence: logical_cycle_sequence.saturating_sub(1),
            valid_flags: SNAPSHOT_VALID
                | SOURCE_SEQUENCE_VALID
                | APPLIED_SEQUENCE_VALID
                | CLA_RESULT_VALID
                | ADC_SAMPLE_VALID
                | FROZEN_ROW,
        }
    }

    fn causal_table() -> StreamTable {
        StreamTable {
            revision: 1,
            streams: vec![
                fast(),
                StreamDescriptor {
                    stream_id: 2,
                    domain: SampleDomain::Control8k,
                    capture_phase: CapturePhase::ControlCycleEnd,
                    sample_rate_hz: 8_000,
                    consistency_group: 1,
                    channel_ids: vec![2],
                },
                StreamDescriptor {
                    stream_id: 3,
                    domain: SampleDomain::Slow1k,
                    capture_phase: CapturePhase::LogicTaskEnd,
                    sample_rate_hz: 1_000,
                    consistency_group: 1,
                    channel_ids: vec![3],
                },
            ],
            bindings: vec![
                super::super::protocol_v2::StreamChannelBinding {
                    channel_id: 1,
                    stream_id: 1,
                    owner: super::super::protocol_v2::SignalOwner::Cpu1Cla1,
                    role: SignalRole::ControlInput,
                },
                super::super::protocol_v2::StreamChannelBinding {
                    channel_id: 2,
                    stream_id: 2,
                    owner: super::super::protocol_v2::SignalOwner::Cpu1,
                    role: SignalRole::ControlOutput,
                },
                super::super::protocol_v2::StreamChannelBinding {
                    channel_id: 3,
                    stream_id: 3,
                    owner: super::super::protocol_v2::SignalOwner::Cpu2,
                    role: SignalRole::AppliedCommand,
                },
            ],
            causal_relations: vec![CausalRelation {
                input_stream_id: 1,
                result_stream_id: 2,
                application_stream_id: 3,
                result_input_offset: 0,
                application_result_offset: 1,
            }],
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

    #[test]
    fn validates_cross_domain_causal_offsets_without_claiming_simultaneity() {
        let table = causal_table();
        let mut validator = SnapshotValidator::default();
        let input = table.stream(1).unwrap();
        let result = table.stream(2).unwrap();
        let application = table.stream(3).unwrap();
        validator.observe_with_table(&table, input, valid_with_cycle(160, 40));
        validator.observe_with_table(&table, result, valid_with_cycle(40, 40));
        let mut applied = valid_with_cycle(5, 41);
        applied.applied_sequence = 41;
        validator.observe_with_table(&table, application, applied);
        assert_eq!(validator.diagnostics(), &SnapshotDiagnostics::default());
    }

    #[test]
    fn diagnoses_missing_and_mismatched_causal_sequences() {
        let table = causal_table();
        let mut validator = SnapshotValidator::with_causal_capacity(1);
        let result = table.stream(2).unwrap();
        validator.observe_with_table(&table, result, valid(7));
        validator.observe_with_table(&table, result, valid(8));
        assert_eq!(validator.diagnostics().missing_causal_source, 1);

        let mut validator = SnapshotValidator::default();
        let input = table.stream(1).unwrap();
        validator.observe_with_table(&table, input, valid(7));
        let mut result_meta = valid(7);
        result_meta.source_sequence = 8;
        validator.observe_with_table(&table, result, result_meta);
        assert_eq!(validator.diagnostics().causal_source_mismatch, 1);
    }

    #[test]
    fn delays_out_of_order_causal_matching_and_bounds_all_caches() {
        let table = causal_table();
        let input = table.stream(1).unwrap();
        let result = table.stream(2).unwrap();
        let mut validator = SnapshotValidator::with_causal_capacity(2);

        validator.observe_with_table(&table, result, valid_with_cycle(10, 100));
        assert_eq!(validator.pending_causal_match_count(), 1);
        assert_eq!(validator.diagnostics().missing_causal_source, 0);

        validator.observe_with_table(&table, input, valid_with_cycle(400, 100));
        assert_eq!(validator.pending_causal_match_count(), 0);
        assert_eq!(validator.diagnostics().missing_causal_source, 0);

        validator.observe_with_table(&table, input, valid_with_cycle(404, 101));
        assert!(validator.causal_cache_len() <= 2);
        assert!(validator.diagnostics().causal_cache_evictions > 0);
    }

    #[test]
    fn diagnoses_early_application_reorder_and_group_mismatch() {
        let table = causal_table();
        let mut validator = SnapshotValidator::default();
        let input = table.stream(1).unwrap();
        let result = table.stream(2).unwrap();
        let application = table.stream(3).unwrap();
        validator.observe_with_table(&table, input, valid(12));
        validator.observe_with_table(&table, result, valid(12));
        let mut early = valid(13);
        early.applied_sequence = 12;
        validator.observe_with_table(&table, application, early);
        validator.observe_with_table(&table, input, valid(11));
        validator.observe_with_table(&table, result, valid(11));
        let mut wrong_group = input.clone();
        wrong_group.consistency_group = 2;
        validator.observe_with_table(&table, &wrong_group, valid(14));
        let diagnostics = validator.diagnostics();
        assert_eq!(diagnostics.causal_application_mismatch, 1);
        assert_eq!(diagnostics.causal_sequence_reorder, 1);
        assert_eq!(diagnostics.causal_group_mismatch, 1);
    }
}
