//! An O(1)-lookup index over an already-replayed event history.
//!
//! A verifier that scans `history.iter().find(|event| event.event_id == id)`
//! once per event it verifies costs O(history length) per lookup, so a full
//! pass over `history` costs O(history length squared). `EventIndex` builds
//! one `event_id -> event` map in a single O(history length) pass, so every
//! lookup after that is O(log history length), keeping a full verification
//! pass linear in history size.

use std::collections::BTreeMap;

use crate::StoredEvent;

/// Borrows `history` and indexes it by `event_id` for repeated lookups.
pub struct EventIndex<'a> {
    events: &'a [StoredEvent],
    by_id: BTreeMap<&'a str, &'a StoredEvent>,
}

impl<'a> EventIndex<'a> {
    /// Builds the index from already-replayed history. This is the only O(n)
    /// pass; every subsequent [`Self::get`] is O(log n).
    #[must_use]
    pub fn build(history: &'a [StoredEvent]) -> Self {
        Self {
            events: history,
            by_id: history
                .iter()
                .map(|event| (event.event_id.as_str(), event))
                .collect(),
        }
    }

    /// Looks up an event by its `event_id`.
    #[must_use]
    pub fn get(&self, event_id: &str) -> Option<&'a StoredEvent> {
        self.by_id.get(event_id).copied()
    }

    /// Returns the indexed history in its original, sequence order.
    #[must_use]
    pub const fn events(&self) -> &'a [StoredEvent] {
        self.events
    }
}
