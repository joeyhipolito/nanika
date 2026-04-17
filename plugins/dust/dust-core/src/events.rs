//! Per-plugin event ring buffer for event replay (REPLAY-15/16).
//!
//! The registry maintains one [`EventRing`] per live plugin. Incoming `event`
//! envelopes from the plugin are fed via [`EventRing::push`]. Subscribers
//! replay missed events via [`EventRing::subscribe`].
//!
//! # Retention policy
//!
//! The ring retains at most [`MAX_RING_EVENTS`] events **or** [`MAX_RING_BYTES`]
//! of total serialised JSON, whichever limit is hit first. When a new event
//! would exceed either bound, the oldest event is evicted until there is room
//! (REPLAY-16). A single oversized event is still accepted — it becomes the
//! sole occupant of the ring until the next push.

use std::collections::VecDeque;

use crate::envelope::EventEnvelope;

// ── Retention limits ─────────────────────────────────────────────────────────

/// Maximum number of events retained in the ring (REPLAY-15).
pub const MAX_RING_EVENTS: usize = 1_000;

/// Maximum total serialised bytes retained in the ring (REPLAY-15/16).
///
/// 512 KiB guarantees that a full `events.subscribe` response fits within the
/// 1 MiB frame cap (REPLAY-17).
pub const MAX_RING_BYTES: usize = 512 * 1_024;

// ── ReplayGap ─────────────────────────────────────────────────────────────────

/// Returned by [`EventRing::subscribe`] when the requested `since_sequence`
/// cursor is older than the oldest retained event.
///
/// Corresponds to error code `-33007 replay_gap` (REPLAY-05).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayGap {
    /// Sequence number of the oldest event still in the ring.
    pub oldest_available: u64,
    /// The `since_sequence` value that triggered this error.
    pub requested: u64,
}

impl std::fmt::Display for ReplayGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "replay_gap: requested {} but oldest available is {}",
            self.requested, self.oldest_available
        )
    }
}

impl std::error::Error for ReplayGap {}

// ── EventRing ─────────────────────────────────────────────────────────────────

/// In-memory ring buffer retaining plugin events for replay.
///
/// ## Retention
///
/// | Bound | Value |
/// |-------|-------|
/// | Max events | [`MAX_RING_EVENTS`] (1 000) |
/// | Max bytes | [`MAX_RING_BYTES`] (512 KiB) |
///
/// Whichever bound is hit first triggers eviction of the oldest event.
pub struct EventRing {
    events: VecDeque<EventEnvelope>,
    total_serialized_bytes: usize,
    /// Sequence of the last evicted event; `None` if nothing has been evicted.
    evicted_through: Option<u64>,
    /// Expected sequence for the next pushed event (`latest + 1`, or `1` if
    /// the ring has never seen an event).
    next_sequence: u64,
}

impl EventRing {
    /// Create an empty ring.
    pub fn new() -> Self {
        Self {
            events: VecDeque::new(),
            total_serialized_bytes: 0,
            evicted_through: None,
            next_sequence: 1,
        }
    }

    /// Push `event` into the ring.
    ///
    /// If adding the event would exceed either retention bound, the oldest
    /// event is evicted first (repeating until there is room, or the ring is
    /// empty). A single oversized event is always accepted.
    pub fn push(&mut self, event: EventEnvelope) {
        let size = serialized_size(&event);

        // Evict oldest until there is room. The `!is_empty()` guard on the
        // byte condition prevents an infinite loop when a single event exceeds
        // MAX_RING_BYTES.
        while !self.events.is_empty()
            && (self.events.len() >= MAX_RING_EVENTS
                || self.total_serialized_bytes + size > MAX_RING_BYTES)
        {
            if let Some(evicted) = self.events.pop_front() {
                let evicted_size = serialized_size(&evicted);
                self.total_serialized_bytes =
                    self.total_serialized_bytes.saturating_sub(evicted_size);
                if let Some(seq) = evicted.sequence {
                    self.evicted_through =
                        Some(self.evicted_through.map_or(seq, |prev| prev.max(seq)));
                }
            }
        }

        // Keep next_sequence in step with incoming sequence numbers.
        if let Some(seq) = event.sequence {
            if seq >= self.next_sequence {
                self.next_sequence = seq + 1;
            }
        }

        self.total_serialized_bytes += size;
        self.events.push_back(event);
    }

    /// Return all retained events with `sequence >= since_sequence`, ascending.
    ///
    /// | `since_sequence` | Behaviour |
    /// |---|---|
    /// | `0` | Returns **all** retained events — never a `ReplayGap` (REPLAY-05) |
    /// | `N > 0, in range` | Returns events with `sequence >= N` |
    /// | `N > latest` | Returns an empty `Vec` — live push delivers future events |
    /// | `N < oldest_available` (after eviction) | `Err(ReplayGap)` |
    pub fn subscribe(&self, since_sequence: u64) -> Result<Vec<EventEnvelope>, ReplayGap> {
        // since_sequence = 0 is the "give me everything" shorthand; never a gap.
        if since_sequence == 0 {
            return Ok(self.events.iter().cloned().collect());
        }

        // Replay gap: subscriber's cursor fell behind the eviction window.
        if let Some(evicted_through) = self.evicted_through {
            if since_sequence <= evicted_through {
                let oldest_available = self
                    .events
                    .front()
                    .and_then(|e| e.sequence)
                    .unwrap_or(evicted_through + 1);
                return Err(ReplayGap {
                    oldest_available,
                    requested: since_sequence,
                });
            }
        }

        // Normal: filter ring by sequence.
        Ok(self
            .events
            .iter()
            .filter(|e| e.sequence.map_or(false, |s| s >= since_sequence))
            .cloned()
            .collect())
    }

    /// Sequence number the next pushed event is expected to carry.
    ///
    /// Equals `latest_sequence + 1`, or `1` if the ring has never seen an event.
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Sequence of the oldest event still in the ring, or `None` if empty.
    pub fn oldest_sequence(&self) -> Option<u64> {
        self.events.front().and_then(|e| e.sequence)
    }

    /// Number of events currently in the ring.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Total serialised bytes of all events currently in the ring.
    pub fn total_serialized_bytes(&self) -> usize {
        self.total_serialized_bytes
    }
}

impl Default for EventRing {
    fn default() -> Self {
        Self::new()
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Return the JSON-serialised byte length of `event`.
fn serialized_size(event: &EventEnvelope) -> usize {
    serde_json::to_vec(event).map(|v| v.len()).unwrap_or(0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::EventType;
    use serde_json::json;

    // ── Helpers ───────────────────────────────────────────────────────────

    fn make_event(seq: u64) -> EventEnvelope {
        EventEnvelope {
            id: format!("evt_{seq:016x}"),
            event_type: EventType::DataUpdated,
            ts: "2026-04-12T00:00:00.000Z".into(),
            sequence: Some(seq),
            data: json!({}),
        }
    }

    /// Create an event whose serialised payload is approximately `kb` KiB.
    fn make_large_event(seq: u64, kb: usize) -> EventEnvelope {
        // The JSON string for `kb` KiB of payload data.
        let payload = "x".repeat(kb * 1_024);
        EventEnvelope {
            id: format!("evt_{seq:016x}"),
            event_type: EventType::DataUpdated,
            ts: "2026-04-12T00:00:00.000Z".into(),
            sequence: Some(seq),
            data: json!({ "payload": payload }),
        }
    }

    // ── Push + eviction by count ──────────────────────────────────────────

    #[test]
    fn push_eviction_by_count_at_limit() {
        let mut ring = EventRing::new();
        for i in 1..=MAX_RING_EVENTS as u64 {
            ring.push(make_event(i));
        }
        assert_eq!(ring.len(), MAX_RING_EVENTS);
        assert!(ring.evicted_through.is_none(), "no eviction yet");
    }

    #[test]
    fn push_eviction_by_count_over_limit() {
        let mut ring = EventRing::new();
        for i in 1..=(MAX_RING_EVENTS as u64 + 1) {
            ring.push(make_event(i));
        }
        assert_eq!(ring.len(), MAX_RING_EVENTS, "ring size stays at MAX_RING_EVENTS");
        assert_eq!(ring.evicted_through, Some(1), "seq=1 was evicted");
        assert_eq!(ring.oldest_sequence(), Some(2));
    }

    #[test]
    fn push_eviction_by_count_multiple() {
        let mut ring = EventRing::new();
        for i in 1..=(MAX_RING_EVENTS as u64 + 5) {
            ring.push(make_event(i));
        }
        assert_eq!(ring.len(), MAX_RING_EVENTS);
        assert_eq!(ring.evicted_through, Some(5));
        assert_eq!(ring.oldest_sequence(), Some(6));
    }

    // ── Push + eviction by byte bound ─────────────────────────────────────

    #[test]
    fn push_eviction_by_byte_bound() {
        let mut ring = EventRing::new();

        // Two 300 KiB events: together they exceed 512 KiB, so the first must
        // be evicted when the second is pushed.
        ring.push(make_large_event(1, 300));
        assert_eq!(ring.len(), 1);
        assert!(ring.total_serialized_bytes() >= 300 * 1_024);

        ring.push(make_large_event(2, 300));
        assert_eq!(ring.len(), 1, "seq=1 should have been evicted");
        assert_eq!(ring.oldest_sequence(), Some(2));
        assert_eq!(ring.evicted_through, Some(1));
        assert!(
            ring.total_serialized_bytes() <= MAX_RING_BYTES
                || ring.events.len() == 1,
            "ring must not exceed byte bound (or contain only one oversized event)"
        );
    }

    #[test]
    fn push_single_oversized_event_accepted() {
        let mut ring = EventRing::new();
        // A single 600 KiB event exceeds MAX_RING_BYTES but must still be accepted.
        ring.push(make_large_event(1, 600));
        assert_eq!(ring.len(), 1, "oversized event must be accepted as sole occupant");
    }

    // ── Subscribe: valid cursor ───────────────────────────────────────────

    #[test]
    fn subscribe_valid_cursor_returns_suffix() {
        let mut ring = EventRing::new();
        for i in 1..=5u64 {
            ring.push(make_event(i));
        }
        let result = ring.subscribe(3).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].sequence, Some(3));
        assert_eq!(result[1].sequence, Some(4));
        assert_eq!(result[2].sequence, Some(5));
    }

    #[test]
    fn subscribe_zero_returns_all() {
        let mut ring = EventRing::new();
        for i in 1..=5u64 {
            ring.push(make_event(i));
        }
        let result = ring.subscribe(0).unwrap();
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn subscribe_exact_oldest() {
        let mut ring = EventRing::new();
        for i in 1..=3u64 {
            ring.push(make_event(i));
        }
        let result = ring.subscribe(1).unwrap();
        assert_eq!(result.len(), 3);
    }

    // ── Subscribe: too-old cursor → ReplayGap ────────────────────────────

    #[test]
    fn subscribe_replay_gap_after_count_eviction() {
        let mut ring = EventRing::new();
        for i in 1..=(MAX_RING_EVENTS as u64 + 1) {
            ring.push(make_event(i));
        }
        // seq=1 was evicted; requesting it is a ReplayGap.
        let err = ring.subscribe(1).unwrap_err();
        assert_eq!(err.requested, 1);
        assert!(err.oldest_available >= 2);
    }

    #[test]
    fn subscribe_replay_gap_carries_oldest_available() {
        let mut ring = EventRing::new();
        for i in 1..=(MAX_RING_EVENTS as u64 + 10) {
            ring.push(make_event(i));
        }
        let err = ring.subscribe(1).unwrap_err();
        // oldest_available must match the sequence of the front of the ring.
        assert_eq!(
            err.oldest_available,
            ring.oldest_sequence().unwrap(),
            "oldest_available must equal the front of the ring"
        );
    }

    // ── Subscribe: future cursor → empty (live push delivers rest) ────────

    #[test]
    fn subscribe_future_cursor_returns_empty() {
        let mut ring = EventRing::new();
        for i in 1..=3u64 {
            ring.push(make_event(i));
        }
        let result = ring.subscribe(100).unwrap();
        assert!(result.is_empty(), "future cursor must return empty events");
        assert_eq!(
            ring.next_sequence(),
            4,
            "next_sequence must be latest + 1"
        );
    }

    #[test]
    fn subscribe_on_empty_ring_returns_empty() {
        let ring = EventRing::new();
        let result = ring.subscribe(0).unwrap();
        assert!(result.is_empty());
        let result2 = ring.subscribe(42).unwrap();
        assert!(result2.is_empty());
    }

    // ── since_sequence=0 never triggers ReplayGap ────────────────────────

    #[test]
    fn subscribe_zero_no_replay_gap_after_eviction() {
        let mut ring = EventRing::new();
        for i in 1..=(MAX_RING_EVENTS as u64 + 10) {
            ring.push(make_event(i));
        }
        // since_sequence=0 must never return ReplayGap, even after eviction.
        let result = ring.subscribe(0).unwrap();
        assert!(!result.is_empty());
        assert_eq!(result.len(), MAX_RING_EVENTS);
    }

    // ── next_sequence tracking ────────────────────────────────────────────

    #[test]
    fn next_sequence_starts_at_one() {
        let ring = EventRing::new();
        assert_eq!(ring.next_sequence(), 1);
    }

    #[test]
    fn next_sequence_advances_after_push() {
        let mut ring = EventRing::new();
        ring.push(make_event(1));
        assert_eq!(ring.next_sequence(), 2);
        ring.push(make_event(5)); // skip some — valid for non-contiguous seqs
        assert_eq!(ring.next_sequence(), 6);
    }

    #[test]
    fn next_sequence_correct_after_eviction() {
        let mut ring = EventRing::new();
        for i in 1..=(MAX_RING_EVENTS as u64 + 1) {
            ring.push(make_event(i));
        }
        assert_eq!(
            ring.next_sequence(),
            MAX_RING_EVENTS as u64 + 2,
            "next_sequence tracks last pushed, not last retained"
        );
    }
}
