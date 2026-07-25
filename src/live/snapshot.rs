//! Frozen SCP1 V2 sampling-row metadata and watermark-driven consistency checks.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::{
    protocol_v2::{CausalRelation, SampleDomain, SignalRole, StreamDescriptor},
    protocol_v2_r2::{StreamDescriptorR2, StreamTableR2},
};

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
pub const MAX_CAUSAL_PENDING_MATCHES: usize = 4_096;

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
    pub logical_cycle_faults: u64,
    pub source_sequence_faults: u64,
    pub applied_sequence_faults: u64,
    pub invalid_snapshot_rows: u64,
    pub missing_causal_source: u64,
    pub causal_source_mismatch: u64,
    pub causal_application_mismatch: u64,
    pub causal_sequence_reorder: u64,
    pub causal_group_mismatch: u64,
    pub causal_cached_rows: usize,
    pub causal_pending_matches: usize,
    pub causal_match_timeouts: u64,
    pub causal_cache_evictions: u64,
    pub causal_window_overflows: u64,
    pub causal_duplicate_cycles: u64,
}

type RelationKey = (u16, u16, u16, i16, i16);
type PendingKey = (RelationKey, u64);
type DeadlineKey = (u16, u64, RelationKey, u64);

#[derive(Clone, Copy, Debug)]
struct PendingResult {
    result: SnapshotMeta,
    deadline_cycle: u64,
}

#[derive(Clone, Copy, Debug)]
struct PendingApplication {
    application: SnapshotMeta,
    deadline_cycle: u64,
}

#[derive(Clone, Debug)]
pub struct SnapshotValidator {
    streams: BTreeMap<u16, StreamSequenceState>,
    causal_rows: BTreeMap<(u16, u64), SnapshotMeta>,
    pending_results: BTreeMap<PendingKey, PendingResult>,
    pending_applications: BTreeMap<PendingKey, PendingApplication>,
    result_deadlines: BTreeSet<DeadlineKey>,
    application_deadlines: BTreeSet<DeadlineKey>,
    causal_last_result: BTreeMap<RelationKey, u64>,
    causal_last_application: BTreeMap<RelationKey, u64>,
    causal_capacity: usize,
    pending_capacity: usize,
    diagnostics: SnapshotDiagnostics,
}

#[derive(Clone, Debug, Default)]
struct StreamSequenceState {
    last_row_sequence: Option<u64>,
    last_logical_cycle: Option<u64>,
    latest_logical_cycle: Option<u64>,
    committed_watermark: Option<u64>,
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
        let capacity = causal_capacity.max(1);
        Self {
            streams: BTreeMap::new(),
            causal_rows: BTreeMap::new(),
            pending_results: BTreeMap::new(),
            pending_applications: BTreeMap::new(),
            result_deadlines: BTreeSet::new(),
            application_deadlines: BTreeSet::new(),
            causal_last_result: BTreeMap::new(),
            causal_last_application: BTreeMap::new(),
            causal_capacity: capacity,
            pending_capacity: capacity.min(MAX_CAUSAL_PENDING_MATCHES),
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

    pub fn reset(&mut self) {
        let capacity = self.causal_capacity;
        *self = Self::with_causal_capacity(capacity);
    }

    pub fn observe(&mut self, descriptor: &StreamDescriptorR2, meta: SnapshotMeta) {
        self.observe_inner(descriptor, meta, false, false);
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
        self.update_gauges();
    }

    pub fn observe_r1(&mut self, descriptor: &StreamDescriptor, meta: SnapshotMeta) {
        let descriptor_r2 = StreamDescriptorR2 {
            stream_id: descriptor.stream_id,
            domain: descriptor.domain,
            capture_phase: descriptor.capture_phase,
            sample_rate_hz: descriptor.sample_rate_hz,
            consistency_group: descriptor.consistency_group,
            logical_cycle_step: 1,
            channel_ids: descriptor.channel_ids.clone(),
        };
        self.observe(&descriptor_r2, meta);
    }

    pub fn observe_with_table(
        &mut self,
        table: &StreamTableR2,
        descriptor: &StreamDescriptorR2,
        meta: SnapshotMeta,
    ) -> Result<(), String> {
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

        let participates = table.causal_relations.iter().any(|relation| {
            relation.input_stream_id == descriptor.stream_id
                || relation.result_stream_id == descriptor.stream_id
                || relation.application_stream_id == descriptor.stream_id
        });
        if !participates {
            self.update_gauges();
            return Ok(());
        }

        self.expire_deadlines(descriptor.stream_id, meta.logical_cycle_sequence);
        self.prune_stream_rows(table, descriptor)?;
        if self
            .causal_rows
            .contains_key(&(descriptor.stream_id, meta.logical_cycle_sequence))
        {
            self.diagnostics.causal_duplicate_cycles =
                self.diagnostics.causal_duplicate_cycles.saturating_add(1);
            self.update_gauges();
            return Ok(());
        }
        if self.causal_rows.len() >= self.causal_capacity {
            self.diagnostics.causal_window_overflows =
                self.diagnostics.causal_window_overflows.saturating_add(1);
            self.update_gauges();
            return Err("CausalWindowOverflow: cached row hard limit reached".to_owned());
        }
        self.causal_rows
            .insert((descriptor.stream_id, meta.logical_cycle_sequence), meta);

        for relation in &table.causal_relations {
            self.observe_relation(table, descriptor, meta, relation)?;
        }
        self.update_gauges();
        Ok(())
    }

    fn observe_inner(
        &mut self,
        descriptor: &StreamDescriptorR2,
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
        if let Some(previous_row) = state.last_row_sequence {
            if meta.row_sequence <= previous_row {
                self.diagnostics.row_sequence_reorders =
                    self.diagnostics.row_sequence_reorders.saturating_add(1);
            } else {
                let row_delta = meta.row_sequence - previous_row;
                if row_delta != 1 {
                    self.diagnostics.row_sequence_gaps =
                        self.diagnostics.row_sequence_gaps.saturating_add(1);
                }
                let expected_logical = u64::from(descriptor.logical_cycle_step)
                    .checked_mul(row_delta)
                    .and_then(|delta| {
                        state
                            .last_logical_cycle
                            .and_then(|previous| previous.checked_add(delta))
                    });
                if expected_logical != Some(meta.logical_cycle_sequence) {
                    self.diagnostics.logical_cycle_faults =
                        self.diagnostics.logical_cycle_faults.saturating_add(1);
                }
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
            if state
                .last_applied_sequence
                .is_some_and(|previous| meta.applied_sequence < previous)
            {
                self.diagnostics.applied_sequence_faults =
                    self.diagnostics.applied_sequence_faults.saturating_add(1);
            }
            state.last_applied_sequence = Some(meta.applied_sequence);
        } else if applied_required {
            self.diagnostics.applied_sequence_faults =
                self.diagnostics.applied_sequence_faults.saturating_add(1);
        }

        state.last_row_sequence = Some(meta.row_sequence);
        state.last_logical_cycle = Some(meta.logical_cycle_sequence);
        state.latest_logical_cycle = Some(
            state
                .latest_logical_cycle
                .map_or(meta.logical_cycle_sequence, |previous| {
                    previous.max(meta.logical_cycle_sequence)
                }),
        );
        state.committed_watermark = state.latest_logical_cycle;
    }

    fn observe_relation(
        &mut self,
        table: &StreamTableR2,
        descriptor: &StreamDescriptorR2,
        meta: SnapshotMeta,
        relation: &CausalRelation,
    ) -> Result<(), String> {
        if !relation_group_matches(table, relation, descriptor) {
            self.diagnostics.causal_group_mismatch =
                self.diagnostics.causal_group_mismatch.saturating_add(1);
            return Ok(());
        }
        let key = relation_key(relation);
        let max_reorder = table
            .group(descriptor.consistency_group)
            .map_or(0_u64, |group| u64::from(group.max_reorder_cycles));

        if descriptor.stream_id == relation.result_stream_id {
            if self
                .causal_last_result
                .insert(key, meta.logical_cycle_sequence)
                .is_some_and(|previous| meta.logical_cycle_sequence <= previous)
            {
                self.diagnostics.causal_sequence_reorder =
                    self.diagnostics.causal_sequence_reorder.saturating_add(1);
            }
            let Some(expected_input_cycle) =
                subtract_offset(meta.source_sequence, relation.result_input_offset)
            else {
                self.diagnostics.causal_source_mismatch =
                    self.diagnostics.causal_source_mismatch.saturating_add(1);
                return Ok(());
            };
            if let Some(input) = self
                .causal_rows
                .get(&(relation.input_stream_id, expected_input_cycle))
                .copied()
            {
                self.validate_result_match(input, meta, relation);
            } else {
                self.insert_pending_result(
                    key,
                    expected_input_cycle,
                    meta,
                    relation.input_stream_id,
                    max_reorder,
                )?;
            }
            self.resolve_pending_application(key, meta.logical_cycle_sequence, relation);
        }

        if descriptor.stream_id == relation.input_stream_id {
            self.resolve_pending_result(key, meta.logical_cycle_sequence, meta, relation);
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
            let Some(expected_result_cycle) =
                subtract_offset(meta.applied_sequence, relation.application_result_offset)
            else {
                self.diagnostics.causal_application_mismatch = self
                    .diagnostics
                    .causal_application_mismatch
                    .saturating_add(1);
                return Ok(());
            };
            if let Some(result) = self
                .causal_rows
                .get(&(relation.result_stream_id, expected_result_cycle))
                .copied()
            {
                self.validate_application_match(result, meta, relation);
            } else {
                self.insert_pending_application(
                    key,
                    expected_result_cycle,
                    meta,
                    relation.result_stream_id,
                    max_reorder,
                )?;
            }
        }
        Ok(())
    }

    fn insert_pending_result(
        &mut self,
        key: RelationKey,
        expected_cycle: u64,
        result: SnapshotMeta,
        source_stream_id: u16,
        max_reorder: u64,
    ) -> Result<(), String> {
        if self.pending_causal_match_count() >= self.pending_capacity {
            return self.pending_overflow();
        }
        let deadline = expected_cycle
            .checked_add(max_reorder)
            .ok_or_else(|| "CausalOffsetOverflow: result deadline overflow".to_owned())?;
        let pending_key = (key, expected_cycle);
        if self
            .pending_results
            .insert(
                pending_key,
                PendingResult {
                    result,
                    deadline_cycle: deadline,
                },
            )
            .is_none()
        {
            self.result_deadlines
                .insert((source_stream_id, deadline, key, expected_cycle));
        }
        Ok(())
    }

    fn insert_pending_application(
        &mut self,
        key: RelationKey,
        expected_cycle: u64,
        application: SnapshotMeta,
        source_stream_id: u16,
        max_reorder: u64,
    ) -> Result<(), String> {
        if self.pending_causal_match_count() >= self.pending_capacity {
            return self.pending_overflow();
        }
        let deadline = expected_cycle
            .checked_add(max_reorder)
            .ok_or_else(|| "CausalOffsetOverflow: application deadline overflow".to_owned())?;
        let pending_key = (key, expected_cycle);
        if self
            .pending_applications
            .insert(
                pending_key,
                PendingApplication {
                    application,
                    deadline_cycle: deadline,
                },
            )
            .is_none()
        {
            self.application_deadlines
                .insert((source_stream_id, deadline, key, expected_cycle));
        }
        Ok(())
    }

    fn resolve_pending_result(
        &mut self,
        key: RelationKey,
        input_cycle: u64,
        input: SnapshotMeta,
        relation: &CausalRelation,
    ) {
        let pending_key = (key, input_cycle);
        if let Some(pending) = self.pending_results.remove(&pending_key) {
            self.result_deadlines.remove(&(
                relation.input_stream_id,
                pending.deadline_cycle,
                key,
                input_cycle,
            ));
            self.validate_result_match(input, pending.result, relation);
        }
    }

    fn resolve_pending_application(
        &mut self,
        key: RelationKey,
        result_cycle: u64,
        relation: &CausalRelation,
    ) {
        let pending_key = (key, result_cycle);
        if let Some(pending) = self.pending_applications.remove(&pending_key) {
            self.application_deadlines.remove(&(
                relation.result_stream_id,
                pending.deadline_cycle,
                key,
                result_cycle,
            ));
            if let Some(result) = self
                .causal_rows
                .get(&(relation.result_stream_id, result_cycle))
                .copied()
            {
                self.validate_application_match(result, pending.application, relation);
            }
        }
    }

    fn expire_deadlines(&mut self, source_stream_id: u16, watermark: u64) {
        let result_expired = self
            .result_deadlines
            .range(
                (source_stream_id, 0, relation_key_min(), 0)
                    ..(source_stream_id, watermark, relation_key_min(), 0),
            )
            .copied()
            .collect::<Vec<_>>();
        for deadline @ (_, _, key, expected_cycle) in result_expired {
            self.result_deadlines.remove(&deadline);
            if self
                .pending_results
                .remove(&(key, expected_cycle))
                .is_some()
            {
                self.record_causal_timeout();
            }
        }
        let application_expired = self
            .application_deadlines
            .range(
                (source_stream_id, 0, relation_key_min(), 0)
                    ..(source_stream_id, watermark, relation_key_min(), 0),
            )
            .copied()
            .collect::<Vec<_>>();
        for deadline @ (_, _, key, expected_cycle) in application_expired {
            self.application_deadlines.remove(&deadline);
            if self
                .pending_applications
                .remove(&(key, expected_cycle))
                .is_some()
            {
                self.record_causal_timeout();
            }
        }
    }

    fn prune_stream_rows(
        &mut self,
        table: &StreamTableR2,
        descriptor: &StreamDescriptorR2,
    ) -> Result<(), String> {
        let Some(latest) = self
            .streams
            .get(&descriptor.stream_id)
            .and_then(|state| state.latest_logical_cycle)
        else {
            return Ok(());
        };
        let group = table.group(descriptor.consistency_group).ok_or_else(|| {
            "CausalGroupMismatch: stream references an unknown causal group".to_owned()
        })?;
        let max_offset = table
            .causal_relations
            .iter()
            .filter(|relation| {
                table
                    .stream(relation.input_stream_id)
                    .is_some_and(|stream| stream.consistency_group == descriptor.consistency_group)
            })
            .flat_map(|relation| {
                [
                    u64::from(relation.result_input_offset.unsigned_abs()),
                    u64::from(relation.application_result_offset.unsigned_abs()),
                ]
            })
            .max()
            .unwrap_or(0);
        let retention = u64::from(group.max_reorder_cycles)
            .checked_add(max_offset)
            .and_then(|value| value.checked_add(u64::from(descriptor.logical_cycle_step)))
            .ok_or_else(|| "CausalOffsetOverflow: retention window overflow".to_owned())?;
        let cutoff = latest.saturating_sub(retention);
        let stale = self
            .causal_rows
            .range((descriptor.stream_id, 0)..(descriptor.stream_id, cutoff))
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        for key in stale {
            self.causal_rows.remove(&key);
            self.diagnostics.causal_cache_evictions =
                self.diagnostics.causal_cache_evictions.saturating_add(1);
        }
        Ok(())
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

    fn pending_overflow(&mut self) -> Result<(), String> {
        self.diagnostics.causal_window_overflows =
            self.diagnostics.causal_window_overflows.saturating_add(1);
        self.update_gauges();
        Err("CausalWindowOverflow: pending relation hard limit reached".to_owned())
    }

    fn record_causal_timeout(&mut self) {
        self.diagnostics.missing_causal_source =
            self.diagnostics.missing_causal_source.saturating_add(1);
        self.diagnostics.causal_match_timeouts =
            self.diagnostics.causal_match_timeouts.saturating_add(1);
    }

    fn update_gauges(&mut self) {
        self.diagnostics.causal_cached_rows = self.causal_rows.len();
        self.diagnostics.causal_pending_matches = self.pending_causal_match_count();
    }
}

fn required_flags(
    descriptor: &StreamDescriptorR2,
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
    table: &StreamTableR2,
    relation: &CausalRelation,
    descriptor: &StreamDescriptorR2,
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

fn relation_key(relation: &CausalRelation) -> RelationKey {
    (
        relation.input_stream_id,
        relation.result_stream_id,
        relation.application_stream_id,
        relation.result_input_offset,
        relation.application_result_offset,
    )
}

const fn relation_key_min() -> RelationKey {
    (0, 0, 0, i16::MIN, i16::MIN)
}

fn add_offset(sequence: u64, offset: i16) -> Option<u64> {
    if offset >= 0 {
        sequence.checked_add(offset as u64)
    } else {
        sequence.checked_sub(u64::from(offset.unsigned_abs()))
    }
}

fn subtract_offset(sequence: u64, offset: i16) -> Option<u64> {
    if offset >= 0 {
        sequence.checked_sub(offset as u64)
    } else {
        sequence.checked_add(u64::from(offset.unsigned_abs()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::{
        protocol_v2::{SignalOwner, StreamChannelBinding},
        protocol_v2_r2::{CausalGroupDescriptorR2, StreamDescriptorR2},
    };

    fn stream(
        stream_id: u16,
        domain: SampleDomain,
        logical_cycle_step: u32,
        channel_id: u16,
    ) -> StreamDescriptorR2 {
        StreamDescriptorR2 {
            stream_id,
            domain,
            capture_phase: domain.fixed_capture_phase(),
            sample_rate_hz: domain.fixed_sample_rate_hz(),
            consistency_group: 1,
            logical_cycle_step,
            channel_ids: vec![channel_id],
        }
    }

    fn causal_table(result_offset: i16, application_offset: i16) -> StreamTableR2 {
        StreamTableR2 {
            revision: 2,
            causal_groups: vec![CausalGroupDescriptorR2 {
                consistency_group: 1,
                logical_cycle_rate_hz: 32_000,
                max_reorder_cycles: 64,
            }],
            streams: vec![
                stream(1, SampleDomain::Fast32k, 1, 1),
                stream(2, SampleDomain::Control8k, 4, 2),
                stream(3, SampleDomain::Slow1k, 32, 3),
            ],
            bindings: vec![
                binding(1, 1, SignalRole::ControlInput),
                binding(2, 2, SignalRole::ControlOutput),
                binding(3, 3, SignalRole::AppliedCommand),
            ],
            causal_relations: vec![CausalRelation {
                input_stream_id: 1,
                result_stream_id: 2,
                application_stream_id: 3,
                result_input_offset: result_offset,
                application_result_offset: application_offset,
            }],
        }
    }

    fn binding(channel_id: u16, stream_id: u16, role: SignalRole) -> StreamChannelBinding {
        StreamChannelBinding {
            channel_id,
            stream_id,
            owner: SignalOwner::Cpu1,
            role,
        }
    }

    fn meta(row: u64, logical: u64) -> SnapshotMeta {
        SnapshotMeta {
            row_sequence: row,
            logical_cycle_sequence: logical,
            source_sequence: logical,
            applied_sequence: logical,
            valid_flags: SNAPSHOT_VALID
                | SOURCE_SEQUENCE_VALID
                | APPLIED_SEQUENCE_VALID
                | CLA_RESULT_VALID
                | ADC_SAMPLE_VALID
                | FROZEN_ROW,
        }
    }

    #[test]
    fn validates_positive_negative_and_application_offsets() {
        for (result_offset, application_offset) in [(0, 4), (1, 32), (-1, 4)] {
            let table = causal_table(result_offset, application_offset);
            let input = table.stream(1).unwrap();
            let result = table.stream(2).unwrap();
            let application = table.stream(3).unwrap();
            let input_cycle = 64;
            let result_cycle = 68;
            let mut result_meta = meta(17, result_cycle);
            result_meta.source_sequence = add_offset(input_cycle, result_offset).unwrap();
            let mut application_meta = meta(3, 96);
            application_meta.applied_sequence =
                add_offset(result_cycle, application_offset).unwrap();

            let mut validator = SnapshotValidator::default();
            validator
                .observe_with_table(&table, input, meta(64, input_cycle))
                .unwrap();
            validator
                .observe_with_table(&table, result, result_meta)
                .unwrap();
            validator
                .observe_with_table(&table, application, application_meta)
                .unwrap();
            assert_eq!(validator.diagnostics().causal_source_mismatch, 0);
            assert_eq!(validator.diagnostics().causal_application_mismatch, 0);
        }
    }

    #[test]
    fn matches_result_application_and_input_in_any_order_within_window() {
        let table = causal_table(1, 4);
        let input = table.stream(1).unwrap();
        let result = table.stream(2).unwrap();
        let application = table.stream(3).unwrap();
        let mut result_meta = meta(25, 100);
        result_meta.source_sequence = 97;
        let mut application_meta = meta(4, 128);
        application_meta.applied_sequence = 104;
        let mut validator = SnapshotValidator::default();

        validator
            .observe_with_table(&table, application, application_meta)
            .unwrap();
        validator
            .observe_with_table(&table, result, result_meta)
            .unwrap();
        validator
            .observe_with_table(&table, input, meta(96, 96))
            .unwrap();

        assert_eq!(validator.pending_causal_match_count(), 0);
        assert_eq!(validator.diagnostics().causal_match_timeouts, 0);
        assert_eq!(validator.diagnostics().causal_source_mismatch, 0);
        assert_eq!(validator.diagnostics().causal_application_mismatch, 0);
    }

    #[test]
    fn watermark_times_out_only_after_deadline() {
        let mut table = causal_table(0, 4);
        table.causal_groups[0].max_reorder_cycles = 2;
        let input = table.stream(1).unwrap().clone();
        let result = table.stream(2).unwrap().clone();
        let mut validator = SnapshotValidator::default();
        let mut result_meta = meta(2, 8);
        result_meta.source_sequence = 8;
        validator
            .observe_with_table(&table, &result, result_meta)
            .unwrap();
        validator
            .observe_with_table(&table, &input, meta(9, 9))
            .unwrap();
        assert_eq!(validator.diagnostics().causal_match_timeouts, 0);
        validator
            .observe_with_table(&table, &input, meta(11, 11))
            .unwrap();
        assert_eq!(validator.diagnostics().causal_match_timeouts, 1);
        assert_eq!(validator.diagnostics().missing_causal_source, 1);
    }

    #[test]
    fn detects_duplicate_cycles_and_checked_offset_boundaries() {
        let table = causal_table(1, 4);
        let input = table.stream(1).unwrap();
        let result = table.stream(2).unwrap();
        let mut validator = SnapshotValidator::default();
        validator
            .observe_with_table(&table, input, meta(1, 1))
            .unwrap();
        validator
            .observe_with_table(&table, input, meta(2, 1))
            .unwrap();
        assert_eq!(validator.diagnostics().causal_duplicate_cycles, 1);

        let mut underflow = meta(1, 1);
        underflow.source_sequence = 0;
        validator
            .observe_with_table(&table, result, underflow)
            .unwrap();
        assert!(validator.diagnostics().causal_source_mismatch > 0);

        assert_eq!(add_offset(u64::MAX, 1), None);
        assert_eq!(subtract_offset(0, 1), None);
    }

    #[test]
    fn row_gaps_advance_logical_cycles_by_stream_step() {
        let descriptor = stream(2, SampleDomain::Control8k, 4, 2);
        let mut validator = SnapshotValidator::default();
        validator.observe(&descriptor, meta(10, 40));
        validator.observe(&descriptor, meta(13, 52));
        assert_eq!(validator.diagnostics().row_sequence_gaps, 1);
        assert_eq!(validator.diagnostics().logical_cycle_faults, 0);
        validator.observe(&descriptor, meta(14, 53));
        assert_eq!(validator.diagnostics().logical_cycle_faults, 1);
    }

    #[test]
    fn hard_limits_return_explicit_errors_without_silent_overwrite() {
        let table = causal_table(0, 4);
        let result = table.stream(2).unwrap();
        let mut validator = SnapshotValidator::with_causal_capacity(1);
        let mut first = meta(1, 4);
        first.source_sequence = 1_000;
        validator.observe_with_table(&table, result, first).unwrap();
        let mut second = meta(2, 8);
        second.source_sequence = 2_000;
        let error = validator
            .observe_with_table(&table, result, second)
            .unwrap_err();
        assert!(error.contains("CausalWindowOverflow"));
        assert_eq!(validator.diagnostics().causal_window_overflows, 1);
    }

    #[test]
    fn consistency_groups_keep_identical_logical_cycles_isolated() {
        let mut table = causal_table(0, 4);
        table.causal_groups.push(CausalGroupDescriptorR2 {
            consistency_group: 2,
            logical_cycle_rate_hz: 32_000,
            max_reorder_cycles: 64,
        });
        for (stream_id, domain, step, channel_id) in [
            (4, SampleDomain::Fast32k, 1, 4),
            (5, SampleDomain::Control8k, 4, 5),
            (6, SampleDomain::Slow1k, 32, 6),
        ] {
            let mut descriptor = stream(stream_id, domain, step, channel_id);
            descriptor.consistency_group = 2;
            table.streams.push(descriptor);
        }
        table.bindings.extend([
            binding(4, 4, SignalRole::ControlInput),
            binding(5, 5, SignalRole::ControlOutput),
            binding(6, 6, SignalRole::AppliedCommand),
        ]);
        table.causal_relations.push(CausalRelation {
            input_stream_id: 4,
            result_stream_id: 5,
            application_stream_id: 6,
            result_input_offset: 0,
            application_result_offset: 4,
        });
        table.validate().unwrap();

        let first_group = table.stream(1).unwrap().clone();
        let second_group = table.stream(4).unwrap().clone();
        let mut validator = SnapshotValidator::default();
        validator
            .observe_with_table(&table, &first_group, meta(64, 64))
            .unwrap();
        validator
            .observe_with_table(&table, &second_group, meta(64, 64))
            .unwrap();
        assert_eq!(validator.causal_cache_len(), 2);
        assert_eq!(validator.diagnostics().causal_duplicate_cycles, 0);
        assert_eq!(validator.diagnostics().causal_group_mismatch, 0);
    }

    #[test]
    #[ignore = "long-stability gate; run explicitly in CI"]
    fn one_million_rows_keep_causal_state_bounded() {
        let table = causal_table(0, 4);
        let input = table.stream(1).unwrap();
        let result = table.stream(2).unwrap();
        let mut validator = SnapshotValidator::default();
        for cycle in 1..=1_000_000_u64 {
            validator
                .observe_with_table(&table, input, meta(cycle, cycle))
                .unwrap();
            if cycle.is_multiple_of(4) {
                let mut result_meta = meta(cycle / 4, cycle);
                result_meta.source_sequence = cycle;
                validator
                    .observe_with_table(&table, result, result_meta)
                    .unwrap();
            }
        }
        assert!(validator.causal_cache_len() <= MAX_CAUSAL_CACHE_ROWS);
        assert!(validator.pending_causal_match_count() <= MAX_CAUSAL_PENDING_MATCHES);
        assert_eq!(validator.diagnostics().causal_window_overflows, 0);
        assert_eq!(validator.diagnostics().causal_match_timeouts, 0);
    }
}
