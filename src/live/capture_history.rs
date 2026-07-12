use std::{collections::VecDeque, sync::Arc};

use thiserror::Error;

use super::trigger::{TriggerCapture, TriggerConfig};

pub const DEFAULT_CAPTURE_HISTORY_ENTRIES: usize = 100;
pub const DEFAULT_CAPTURE_HISTORY_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CaptureId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureOrigin {
    Live,
    Recording {
        source_id: String,
        trigger_ordinal: usize,
    },
}

#[derive(Clone, Debug)]
pub enum CapturePayload {
    InMemory(Arc<TriggerCapture>),
    RecordingRange { start_sample: u64, end_sample: u64 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureQuality {
    pub auto_timeout: bool,
    pub contains_gap: bool,
    pub incomplete_pre: bool,
    pub incomplete_post: bool,
}

#[derive(Clone, Debug)]
pub struct CaptureEntry {
    pub id: CaptureId,
    pub origin: CaptureOrigin,
    pub payload: CapturePayload,
    pub trigger_config: TriggerConfig,
    pub trigger_sample_index: u64,
    pub trigger_timestamp: u64,
    pub label: String,
    pub note: String,
    pub pinned: bool,
    pub created_unix_ms: u64,
    pub approximate_bytes: usize,
    pub quality: CaptureQuality,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureInsertOutcome {
    pub id: CaptureId,
    pub evicted: Vec<CaptureId>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CaptureHistoryError {
    #[error("capture history limits must be greater than zero")]
    InvalidLimits,
    #[error("capture cannot be retained because pinned entries consume the history budget")]
    PinnedBudgetExceeded,
}

pub struct CaptureHistory {
    entries: VecDeque<CaptureEntry>,
    max_entries: usize,
    max_bytes: usize,
    in_memory_bytes: usize,
    next_id: u64,
    selected: Option<CaptureId>,
}

impl Default for CaptureHistory {
    fn default() -> Self {
        Self::new(
            DEFAULT_CAPTURE_HISTORY_ENTRIES,
            DEFAULT_CAPTURE_HISTORY_BYTES,
        )
        .expect("default capture history limits are valid")
    }
}

impl CaptureHistory {
    pub fn new(max_entries: usize, max_bytes: usize) -> Result<Self, CaptureHistoryError> {
        if max_entries == 0 || max_bytes == 0 {
            return Err(CaptureHistoryError::InvalidLimits);
        }
        Ok(Self {
            entries: VecDeque::new(),
            max_entries,
            max_bytes,
            in_memory_bytes: 0,
            next_id: 1,
            selected: None,
        })
    }

    pub fn entries(&self) -> &VecDeque<CaptureEntry> {
        &self.entries
    }

    pub fn selected_id(&self) -> Option<CaptureId> {
        self.selected
    }

    pub fn selected(&self) -> Option<&CaptureEntry> {
        let selected = self.selected?;
        self.entries.iter().find(|entry| entry.id == selected)
    }

    pub fn select(&mut self, id: CaptureId) -> bool {
        if self.entries.iter().any(|entry| entry.id == id) {
            self.selected = Some(id);
            true
        } else {
            false
        }
    }

    pub fn set_pinned(&mut self, id: CaptureId, pinned: bool) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        entry.pinned = pinned;
        true
    }

    pub fn set_metadata(
        &mut self,
        id: CaptureId,
        label: String,
        note: String,
        pinned: bool,
    ) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        entry.label = label;
        entry.note = note;
        entry.pinned = pinned;
        true
    }

    pub fn select_previous(&mut self) -> Option<CaptureId> {
        let current = self
            .selected
            .and_then(|selected| self.entries.iter().position(|entry| entry.id == selected))
            .unwrap_or(self.entries.len());
        let index = current.checked_sub(1)?;
        let id = self.entries.get(index)?.id;
        self.selected = Some(id);
        Some(id)
    }

    pub fn select_next(&mut self) -> Option<CaptureId> {
        let index = match self.selected {
            Some(selected) => self
                .entries
                .iter()
                .position(|entry| entry.id == selected)?
                .saturating_add(1),
            None => 0,
        };
        let id = self.entries.get(index)?.id;
        self.selected = Some(id);
        Some(id)
    }

    pub fn remove(&mut self, id: CaptureId) -> bool {
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        let removed = self.entries.remove(index).expect("capture index is valid");
        self.in_memory_bytes = self
            .in_memory_bytes
            .saturating_sub(in_memory_bytes(&removed));
        if self.selected == Some(id) {
            self.selected = self
                .entries
                .get(index.min(self.entries.len().saturating_sub(1)))
                .map(|entry| entry.id);
        }
        true
    }

    pub fn insert_live(
        &mut self,
        capture: TriggerCapture,
        trigger_config: TriggerConfig,
        created_unix_ms: u64,
        select_new: bool,
    ) -> Result<CaptureInsertOutcome, CaptureHistoryError> {
        let approximate_bytes = capture_bytes(&capture);
        if approximate_bytes > self.max_bytes {
            return Err(CaptureHistoryError::PinnedBudgetExceeded);
        }
        let id = CaptureId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        let trigger_position = capture
            .trigger_position
            .min(capture.sample_indices.len().saturating_sub(1));
        let entry = CaptureEntry {
            id,
            origin: CaptureOrigin::Live,
            trigger_sample_index: capture
                .sample_indices
                .get(trigger_position)
                .copied()
                .unwrap_or_default(),
            trigger_timestamp: capture
                .timestamps
                .get(trigger_position)
                .copied()
                .unwrap_or_default(),
            label: format!("Capture {}", id.0),
            note: String::new(),
            pinned: false,
            created_unix_ms,
            approximate_bytes,
            quality: CaptureQuality {
                auto_timeout: capture.auto_timeout,
                incomplete_pre: capture.trigger_position < trigger_config.pre_samples,
                incomplete_post: capture
                    .sample_indices
                    .len()
                    .saturating_sub(capture.trigger_position.saturating_add(1))
                    < trigger_config.post_samples,
                ..CaptureQuality::default()
            },
            payload: CapturePayload::InMemory(Arc::new(capture)),
            trigger_config,
        };
        self.in_memory_bytes = self.in_memory_bytes.saturating_add(approximate_bytes);
        self.entries.push_back(entry);

        let mut evicted = Vec::new();
        while self.entries.len() > self.max_entries || self.in_memory_bytes > self.max_bytes {
            let Some(index) = self.entries.iter().position(|entry| !entry.pinned) else {
                break;
            };
            let removed = self.entries.remove(index).expect("capture index is valid");
            self.in_memory_bytes = self
                .in_memory_bytes
                .saturating_sub(in_memory_bytes(&removed));
            evicted.push(removed.id);
        }
        if !self.entries.iter().any(|entry| entry.id == id) {
            return Err(CaptureHistoryError::PinnedBudgetExceeded);
        }
        if select_new || self.selected.is_none() {
            self.selected = Some(id);
        }
        if self
            .selected
            .is_some_and(|selected| evicted.contains(&selected))
        {
            self.selected = self.entries.back().map(|entry| entry.id);
        }
        Ok(CaptureInsertOutcome { id, evicted })
    }

    pub fn clear(&mut self, include_pinned: bool) {
        self.entries.retain(|entry| !include_pinned && entry.pinned);
        self.in_memory_bytes = self.entries.iter().map(in_memory_bytes).sum();
        self.selected = self.entries.back().map(|entry| entry.id);
    }
}

fn in_memory_bytes(entry: &CaptureEntry) -> usize {
    matches!(&entry.payload, CapturePayload::InMemory(_))
        .then_some(entry.approximate_bytes)
        .unwrap_or(0)
}

fn capture_bytes(capture: &TriggerCapture) -> usize {
    capture.sample_indices.len() * std::mem::size_of::<u64>()
        + capture.timestamps.len() * std::mem::size_of::<u64>()
        + capture.channel_ids.len() * std::mem::size_of::<u16>()
        + capture
            .channels
            .iter()
            .map(|channel| channel.len() * std::mem::size_of::<f32>())
            .sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(samples: usize) -> TriggerCapture {
        TriggerCapture {
            channel_ids: vec![0],
            sample_indices: (0..samples as u64).collect(),
            timestamps: (0..samples as u64).collect(),
            channels: vec![vec![0.0; samples]],
            trigger_position: samples / 2,
            auto_timeout: false,
        }
    }

    #[test]
    fn evicts_oldest_unpinned_and_keeps_selection_valid() {
        let bytes = capture_bytes(&capture(4));
        let mut history = CaptureHistory::new(2, bytes * 3).unwrap();
        let first = history
            .insert_live(capture(4), TriggerConfig::default(), 1, true)
            .unwrap()
            .id;
        history.set_pinned(first, true);
        history
            .insert_live(capture(4), TriggerConfig::default(), 2, true)
            .unwrap();
        let third = history
            .insert_live(capture(4), TriggerConfig::default(), 3, true)
            .unwrap()
            .id;
        assert_eq!(history.entries().len(), 2);
        assert!(history.entries().iter().any(|entry| entry.id == first));
        assert_eq!(history.selected_id(), Some(third));
    }

    #[test]
    fn navigates_edits_and_removes_entries_without_stale_selection() {
        let mut history = CaptureHistory::new(4, DEFAULT_CAPTURE_HISTORY_BYTES).unwrap();
        let first = history
            .insert_live(capture(4), TriggerConfig::default(), 1, true)
            .unwrap()
            .id;
        let second = history
            .insert_live(capture(4), TriggerConfig::default(), 2, true)
            .unwrap()
            .id;
        let third = history
            .insert_live(capture(4), TriggerConfig::default(), 3, true)
            .unwrap()
            .id;

        assert_eq!(history.select_previous(), Some(second));
        assert_eq!(history.select_previous(), Some(first));
        assert_eq!(history.select_previous(), None);
        assert_eq!(history.select_next(), Some(second));
        assert!(history.set_metadata(
            second,
            "DC-link trip".to_owned(),
            "Inspect desaturation edge".to_owned(),
            true,
        ));
        assert_eq!(history.selected().unwrap().label, "DC-link trip");
        assert_eq!(
            history.selected().unwrap().note,
            "Inspect desaturation edge"
        );
        assert!(history.selected().unwrap().pinned);
        assert!(history.remove(second));
        assert_eq!(history.selected_id(), Some(third));
        assert!(!history.remove(second));
    }

    #[test]
    fn rejects_new_capture_when_all_retained_entries_are_pinned() {
        let bytes = capture_bytes(&capture(4));
        let mut history = CaptureHistory::new(1, bytes * 2).unwrap();
        let first = history
            .insert_live(capture(4), TriggerConfig::default(), 1, true)
            .unwrap()
            .id;
        history.set_pinned(first, true);
        assert_eq!(
            history.insert_live(capture(4), TriggerConfig::default(), 2, true),
            Err(CaptureHistoryError::PinnedBudgetExceeded)
        );
        assert_eq!(history.entries().len(), 1);
    }
}
