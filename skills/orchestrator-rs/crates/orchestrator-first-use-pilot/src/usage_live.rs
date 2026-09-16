//! Bounded, best-effort live Claude usage diagnostics. Not execution authority or billing.
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

const MAX_MESSAGES: usize = 512;
const MAX_SESSIONS: usize = 64;
const FIELDS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];
type Identity = (String, String);

fn label(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    if text.is_empty()
        || text.len() > 160
        || !text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
    {
        return None;
    }
    Some(text.to_owned())
}

fn optional_label(object: &Value, key: &str) -> std::result::Result<Option<String>, ()> {
    match object.get(key) {
        None => Ok(None),
        Some(value) => label(value).map(Some).ok_or(()),
    }
}

fn counters(usage: &Value) -> std::result::Result<[Option<u64>; 4], ()> {
    let object = usage.as_object().ok_or(())?;
    let mut result = [None; 4];
    for (index, name) in FIELDS.iter().enumerate() {
        if let Some(value) = object.get(*name) {
            result[index] = Some(value.as_u64().ok_or(())?);
        }
    }
    Ok(result)
}

fn total(values: &[Option<u64>]) -> Option<u64> {
    let mut sum = 0_u64;
    for value in values {
        let value = (*value)?;
        sum = sum.checked_add(value)?;
    }
    Some(sum)
}

fn unavailable(reason: &'static str) -> Value {
    json!({"kind":"worker.usage_unavailable","reason":reason})
}

#[derive(Clone, Copy)]
enum Source {
    Assistant = 0,
    Stream = 1,
}

#[derive(Clone, PartialEq)]
struct Fingerprint {
    counters: [Option<u64>; 4],
    ambiguous: bool,
    final_stream_output_observed: bool,
}

#[derive(Default, Clone)]
struct MessageEntry {
    ordinal: usize,
    counters: [Option<u64>; 4],
    output_last_by_source: [Option<u64>; 2],
    ambiguous: bool,
    final_stream_output_observed: bool,
    last_emitted: Option<Fingerprint>,
}

impl MessageEntry {
    fn new(ordinal: usize) -> Self {
        Self {
            ordinal,
            ..Default::default()
        }
    }

    // Immutable input counters conflict forever; cumulative output takes the
    // max across representations but regresses ambiguously within one source.
    fn observe_counters(&mut self, values: [Option<u64>; 4], source: Source) {
        for index in [0usize, 2, 3] {
            if let Some(incoming) = values[index] {
                if let Some(previous) = self.counters[index] {
                    if previous != incoming {
                        self.ambiguous = true;
                    }
                }
                self.counters[index] = Some(incoming);
            }
        }
        if let Some(incoming) = values[1] {
            let slot = source as usize;
            if let Some(previous) = self.output_last_by_source[slot] {
                if incoming < previous {
                    self.ambiguous = true;
                }
            }
            self.output_last_by_source[slot] = Some(incoming);
            self.counters[1] = Some(self.counters[1].map_or(incoming, |old| old.max(incoming)));
        }
    }
}

#[derive(Default)]
pub(crate) struct LiveUsage {
    messages: BTreeMap<Identity, MessageEntry>,
    order_next: usize,
    active: BTreeMap<String, Identity>,
}

impl LiveUsage {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    fn reserve_message(&mut self, identity: &Identity) -> std::result::Result<(), Value> {
        if self.messages.contains_key(identity) {
            return Ok(());
        }
        if self.messages.len() >= MAX_MESSAGES {
            return Err(unavailable("message identity limit exceeded"));
        }
        self.order_next += 1;
        self.messages
            .insert(identity.clone(), MessageEntry::new(self.order_next));
        Ok(())
    }

    fn snapshot(&mut self, identity: &Identity) -> Vec<Value> {
        let Some(entry) = self.messages.get_mut(identity) else {
            return Vec::new();
        };
        let observed_input_tokens = if entry.ambiguous {
            None
        } else {
            total(&[entry.counters[0], entry.counters[2], entry.counters[3]])
        };
        let fingerprint = Fingerprint {
            counters: entry.counters,
            ambiguous: entry.ambiguous,
            final_stream_output_observed: entry.final_stream_output_observed,
        };
        if entry.last_emitted.as_ref() == Some(&fingerprint) {
            return Vec::new();
        }
        entry.last_emitted = Some(fingerprint);
        let mut usage = Map::new();
        for (index, name) in FIELDS.iter().enumerate() {
            usage.insert((*name).into(), json!(entry.counters[index]));
        }
        vec![json!({
            "kind":"worker.usage",
            "delivery":"live-snapshot",
            "aggregation":"replace-by-session-message",
            "runtime":"claude",
            "portal_effective":"off",
            "mode_source":"runtime-default",
            "message_ordinal": entry.ordinal,
            "session_id": identity.0,
            "message_id": identity.1,
            "usage": usage,
            "observed_input_tokens": observed_input_tokens,
            "ambiguous": entry.ambiguous,
            "final_stream_output_observed": entry.final_stream_output_observed,
        })]
    }

    fn observe_assistant(&mut self, row: &Value) -> Vec<Value> {
        let Some(message) = row.get("message").filter(|m| m.is_object()) else {
            return vec![unavailable("invalid assistant envelope")];
        };
        let session = match optional_label(row, "session_id") {
            Ok(session) => session,
            Err(()) => return vec![unavailable("invalid session identity")],
        };
        let id = match optional_label(message, "id") {
            Ok(id) => id,
            Err(()) => return vec![unavailable("invalid message identity")],
        };
        let Some(identity) = session.zip(id) else {
            return Vec::new();
        };
        let Some(usage) = message.get("usage") else {
            return Vec::new();
        };
        let values = match counters(usage) {
            Ok(values) => values,
            Err(()) => return vec![unavailable("invalid usage counters")],
        };
        if let Err(reason) = self.reserve_message(&identity) {
            return vec![reason];
        }
        if let Some(entry) = self.messages.get_mut(&identity) {
            entry.observe_counters(values, Source::Assistant);
        }
        self.snapshot(&identity)
    }

    fn observe_message_start(&mut self, row: &Value, event: &Value) -> Vec<Value> {
        let Some(message) = event.get("message").filter(|m| m.is_object()) else {
            return vec![unavailable("invalid stream message")];
        };
        let session = match optional_label(row, "session_id") {
            Ok(session) => session,
            Err(()) => return vec![unavailable("invalid session identity")],
        };
        let id = match optional_label(message, "id") {
            Ok(id) => id,
            Err(()) => return vec![unavailable("invalid message identity")],
        };
        let (Some(session), Some(id)) = (session, id) else {
            return vec![unavailable("missing stream identity")];
        };
        let identity: Identity = (session.clone(), id);
        match self.active.get(&session) {
            Some(previous) if previous == &identity => {}
            Some(_) => {
                // Multiple active starts in one session: never guess which
                // message a later delta belongs to.
                self.active.remove(&session);
                return vec![unavailable("conflicting stream messages in session")];
            }
            None => {
                if self.active.len() >= MAX_SESSIONS {
                    return vec![unavailable("active session limit exceeded")];
                }
            }
        }
        self.active.insert(session, identity.clone());
        if let Err(reason) = self.reserve_message(&identity) {
            return vec![reason];
        }
        if let Some(usage) = message.get("usage") {
            let values = match counters(usage) {
                Ok(values) => values,
                Err(()) => return vec![unavailable("invalid usage counters")],
            };
            if let Some(entry) = self.messages.get_mut(&identity) {
                entry.observe_counters(values, Source::Stream);
            }
        }
        self.snapshot(&identity)
    }

    fn observe_message_delta(&mut self, row: &Value, event: &Value) -> Vec<Value> {
        let session = match optional_label(row, "session_id") {
            Ok(session) => session,
            Err(()) => return vec![unavailable("invalid session identity")],
        };
        let Some(session) = session else {
            return vec![unavailable("missing stream session")];
        };
        let Some(usage) = event.get("usage") else {
            return Vec::new();
        };
        let values = match counters(usage) {
            Ok(values) => values,
            Err(()) => return vec![unavailable("invalid usage counters")],
        };
        let Some(identity) = self.active.get(&session).cloned() else {
            return vec![unavailable("uncorrelated stream delta")];
        };
        let has_output = values[1].is_some();
        let Some(entry) = self.messages.get_mut(&identity) else {
            return vec![unavailable("uncorrelated stream delta")];
        };
        entry.observe_counters(values, Source::Stream);
        if has_output
            && event
                .get("delta")
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str)
                .is_some()
        {
            entry.final_stream_output_observed = true;
        }
        self.snapshot(&identity)
    }

    fn observe_stream_event(&mut self, row: &Value) -> Vec<Value> {
        let Some(event) = row.get("event").filter(|v| v.is_object()) else {
            return vec![unavailable("invalid stream event")];
        };
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => self.observe_message_start(row, event),
            Some("message_delta") => self.observe_message_delta(row, event),
            Some("message_stop") => {
                if let Ok(Some(session)) = optional_label(row, "session_id") {
                    self.active.remove(&session);
                    Vec::new()
                } else {
                    vec![unavailable("missing stream session")]
                }
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn observe(&mut self, row: &Value) -> Vec<Value> {
        if !row.is_object() {
            self.active.clear();
            return vec![unavailable("record must be an object")];
        }
        let events = match row.get("type").and_then(Value::as_str) {
            Some("assistant") => self.observe_assistant(row),
            Some("stream_event") => self.observe_stream_event(row),
            _ => Vec::new(),
        };
        // A refused record may have contained a replacement start/stop. Never
        // associate later deltas with a message from before that uncertainty.
        if events
            .iter()
            .any(|event| event["kind"] == "worker.usage_unavailable")
        {
            self.active.clear();
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type Result<T> = std::result::Result<T, String>;

    fn assistant(session: &str, id: &str, usage: Value) -> Value {
        json!({"type":"assistant","session_id":session,"message":{"id":id,"usage":usage}})
    }
    fn usage() -> Value {
        json!({"input_tokens":2,"output_tokens":4,"cache_read_input_tokens":10,"cache_creation_input_tokens":3})
    }
    fn start(session: &str, id: &str, usage: Value) -> Value {
        json!({"type":"stream_event","session_id":session,"event":{"type":"message_start","message":{"id":id,"usage":usage}}})
    }
    fn delta(session: &str, output: u64) -> Value {
        json!({"type":"stream_event","session_id":session,"event":{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":output}}})
    }
    fn kind(v: &Value) -> Option<&str> {
        v.get("kind").and_then(Value::as_str)
    }

    #[test]
    fn non_object_refusal_clears_active_identity() -> Result<()> {
        let mut live = LiveUsage::default();
        live.observe(&start("s", "m", usage()));
        live.observe(&json!([]));
        assert_eq!(
            kind(
                live.observe(&delta("s", 9))
                    .first()
                    .ok_or("missing refusal")?
            ),
            Some("worker.usage_unavailable")
        );
        Ok(())
    }
    #[test]
    fn partial_message_delta_does_not_claim_final_output() -> Result<()> {
        let mut live = LiveUsage::default();
        live.observe(&start("s", "m", usage()));
        let provisional = json!({"type":"stream_event","session_id":"s","event":{"type":"message_delta","usage":{"output_tokens":9}}});
        let out = live.observe(&provisional);
        assert_eq!(
            out.first().ok_or("missing update")?["final_stream_output_observed"],
            false
        );
        assert_eq!(
            live.observe(&delta("s", 9))
                .first()
                .ok_or("missing final update")?["final_stream_output_observed"],
            true
        );
        Ok(())
    }

    #[test]
    fn invalid_start_drops_previous_correlation() -> Result<()> {
        let mut live = LiveUsage::default();
        live.observe(&start("s", "old", usage()));
        let invalid = json!({"type":"stream_event","session_id":"s","event":{"type":"message_start","message":{}}});
        assert_eq!(
            kind(live.observe(&invalid).first().ok_or("missing refusal")?),
            Some("worker.usage_unavailable")
        );
        assert_eq!(
            kind(
                live.observe(&delta("s", 99))
                    .first()
                    .ok_or("missing refusal")?
            ),
            Some("worker.usage_unavailable")
        );
        Ok(())
    }

    #[test]
    fn sessions_are_isolated() -> Result<()> {
        let mut live = LiveUsage::default();
        let a = live.observe(&assistant("a", "m", usage()));
        let b = live.observe(&assistant("b", "m", usage()));
        let a0 = a.first().ok_or("missing snapshot")?;
        let b0 = b.first().ok_or("missing snapshot")?;
        assert_eq!(kind(a0), Some("worker.usage"));
        assert_eq!(a0["session_id"], "a");
        assert_eq!(b0["session_id"], "b");
        assert_eq!(a0["message_ordinal"], 1);
        assert_eq!(b0["message_ordinal"], 2);
        Ok(())
    }

    #[test]
    fn delta_before_start_is_uncorrelated() -> Result<()> {
        let mut live = LiveUsage::default();
        let out = live.observe(&delta("s", 10));
        let event = out.first().ok_or("missing event")?;
        assert_eq!(kind(event), Some("worker.usage_unavailable"));
        // A later start for the same session must not inherit stale state.
        let started = live.observe(&start("s", "m", usage()));
        assert_eq!(
            kind(started.first().ok_or("missing")?),
            Some("worker.usage")
        );
        Ok(())
    }

    #[test]
    fn duplicate_snapshots_are_deduplicated() -> Result<()> {
        let mut live = LiveUsage::default();
        let row = assistant("s", "m", usage());
        assert!(!live.observe(&row).is_empty());
        assert!(live.observe(&row).is_empty());
        Ok(())
    }

    #[test]
    fn provisional_snapshot_updates_after_final_delta() -> Result<()> {
        let mut live = LiveUsage::default();
        live.observe(&assistant("s", "m", usage()));
        live.observe(&start("s", "m", usage()));
        let out = live.observe(&delta("s", 100));
        let snap = out.first().ok_or("missing snapshot")?;
        assert_eq!(snap["usage"]["output_tokens"], 100);
        assert_eq!(snap["final_stream_output_observed"], true);
        // The lower authoritative envelope value must not regress the max.
        let out = live.observe(&assistant("s", "m", usage()));
        assert!(out.is_empty());
        Ok(())
    }

    #[test]
    fn missing_and_invalid_counters_are_handled() -> Result<()> {
        let mut live = LiveUsage::default();
        let out = live.observe(&assistant("s", "m", json!({"input_tokens":2})));
        let snap = out.first().ok_or("missing snapshot")?;
        assert!(snap["usage"]["output_tokens"].is_null());
        let mut bad = assistant("s", "m2", usage());
        bad["message"]["usage"]["input_tokens"] = json!("nope");
        let out = live.observe(&bad);
        assert_eq!(
            kind(out.first().ok_or("missing")?),
            Some("worker.usage_unavailable")
        );
        Ok(())
    }

    #[test]
    fn conflicting_input_counters_are_ambiguous() -> Result<()> {
        let mut live = LiveUsage::default();
        live.observe(&assistant("s", "m", usage()));
        let mut other = assistant("s", "m", usage());
        other["message"]["usage"]["input_tokens"] = json!(999);
        let out = live.observe(&other);
        let snap = out.first().ok_or("missing snapshot")?;
        assert_eq!(snap["ambiguous"], true);
        assert!(snap["observed_input_tokens"].is_null());
        Ok(())
    }

    #[test]
    fn bounded_identity_overflow_is_reported() -> Result<()> {
        let mut live = LiveUsage::default();
        for i in 0..MAX_MESSAGES {
            let id = format!("m{i}");
            assert!(!live.observe(&assistant("s", &id, usage())).is_empty());
        }
        let out = live.observe(&assistant("s", "overflow", usage()));
        assert_eq!(
            kind(out.first().ok_or("missing")?),
            Some("worker.usage_unavailable")
        );

        let mut sessions_live = LiveUsage::default();
        for i in 0..MAX_SESSIONS {
            let session = format!("s{i}");
            sessions_live.observe(&start(&session, "m", usage()));
        }
        let out = sessions_live.observe(&start("overflow-session", "m", usage()));
        assert_eq!(
            kind(out.first().ok_or("missing")?),
            Some("worker.usage_unavailable")
        );
        Ok(())
    }

    #[test]
    fn reset_prevents_stale_association() -> Result<()> {
        let mut live = LiveUsage::default();
        live.observe(&start("s", "m", usage()));
        live.reset();
        let out = live.observe(&delta("s", 5));
        assert_eq!(
            kind(out.first().ok_or("missing")?),
            Some("worker.usage_unavailable")
        );
        Ok(())
    }
}
