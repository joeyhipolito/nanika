use crate::usage::Unique;
use serde_json::{Map, Value, json};

const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_TURNS: u64 = 4096;
const MAX_THREAD_ID_LEN: usize = 160;

struct ActiveTurn {
    ordinal: u64,
}

#[derive(Clone, PartialEq)]
struct UsageCounters {
    input: Option<u64>,
    cached: Option<u64>,
    cache_write: Option<u64>,
    output: Option<u64>,
    reasoning: Option<u64>,
}

/// Observational telemetry collector for Codex exec JSONL streams.
/// Never used for acceptance or billing decisions.
#[derive(Default)]
pub(crate) struct CodexUsage {
    poisoned: Option<&'static str>,
    thread_id: Option<String>,
    active_turn: Option<ActiveTurn>,
    turn_ordinal: u64,
    events: Vec<Value>,
    last_completed_turn_ordinal: Option<u64>,
    last_completed_usage: Option<UsageCounters>,
    failed_count: u64,
    duplicate_terminal_count: u64,
}

impl CodexUsage {
    pub(crate) fn observe(&mut self, row: &Value) -> Result<Vec<Value>, &'static str> {
        if let Some(e) = self.poisoned {
            return Err(e);
        }
        match self.observe_inner(row) {
            Ok(events) => Ok(events),
            Err(e) => {
                self.poisoned = Some(e);
                Err(e)
            }
        }
    }

    fn observe_inner(&mut self, row: &Value) -> Result<Vec<Value>, &'static str> {
        let obj = row.as_object().ok_or("row must be a JSON object")?;
        let type_str = match obj.get("type") {
            Some(Value::String(s)) => s.as_str(),
            _ => return Err("event type must be a string"),
        };

        match type_str {
            "thread.started" => {
                let id = validate_thread_id(
                    obj.get("thread_id")
                        .ok_or("thread.started missing thread_id")?,
                )?;
                match &self.thread_id {
                    None => {
                        self.thread_id = Some(id);
                        Ok(vec![])
                    }
                    Some(existing) if *existing == id => Ok(vec![]),
                    Some(_) => Err("conflicting thread_id"),
                }
            }
            "turn.started" => {
                if self.thread_id.is_none() {
                    return Err("turn.started before thread.started");
                }
                if self.active_turn.is_some() {
                    return Err("overlapping turn.started");
                }
                if self.turn_ordinal >= MAX_TURNS {
                    return Err("turn limit exceeded");
                }
                self.turn_ordinal += 1;
                self.active_turn = Some(ActiveTurn {
                    ordinal: self.turn_ordinal,
                });
                Ok(vec![])
            }
            "turn.completed" => {
                let usage_value = obj.get("usage").ok_or("turn.completed missing usage")?;
                let usage = validate_usage(usage_value)?;
                if let Some(active) = self.active_turn.take() {
                    let event = self.build_event(active.ordinal, &usage)?;
                    self.last_completed_turn_ordinal = Some(active.ordinal);
                    self.last_completed_usage = Some(usage);
                    self.events.push(event.clone());
                    Ok(vec![event])
                } else if self.turn_ordinal == 0 {
                    Err("terminal event before any turn started")
                } else if self.last_completed_turn_ordinal == Some(self.turn_ordinal)
                    && self.last_completed_usage.as_ref() == Some(&usage)
                {
                    self.duplicate_terminal_count += 1;
                    Ok(vec![])
                } else {
                    Err("conflicting duplicate turn.completed")
                }
            }
            "turn.failed" | "error" => {
                if self.active_turn.take().is_some() {
                    self.failed_count += 1;
                    Ok(vec![])
                } else if self.turn_ordinal == 0 {
                    Err("terminal event before any turn started")
                } else {
                    Err("terminal event without active turn")
                }
            }
            // item.started / item.updated / item.completed and any unknown
            // event type are observed but never accounted.
            _ => Ok(vec![]),
        }
    }

    fn build_event(&self, ordinal: u64, usage: &UsageCounters) -> Result<Value, &'static str> {
        let thread_id = self.thread_id.clone().ok_or("missing thread id")?;
        let uncached = match (usage.input, usage.cached) {
            (Some(i), Some(c)) => Some(i - c),
            _ => None,
        };
        Ok(json!({
            "kind": "worker.usage",
            "runtime": "codex",
            "delivery": "turn-final-snapshot",
            "aggregation": "replace-by-thread-turn",
            "granularity": "provider-turn",
            "identity_source": "attempt-local-start-ordinal",
            "thread_id": thread_id,
            "turn_id": format!("turn-{}", ordinal),
            "turn_ordinal": ordinal,
            "usage": {
                "input_tokens": usage.input,
                "output_tokens": usage.output,
                "cache_read_input_tokens": usage.cached,
                "cache_creation_input_tokens": usage.cache_write,
                "reasoning_output_tokens": usage.reasoning,
            },
            "observed_input_tokens": usage.input,
            "uncached_input_tokens": uncached,
            "cost_usd": Value::Null,
            "message_id": Value::Null,
        }))
    }

    pub(crate) fn finish(&self) -> Result<Value, &'static str> {
        if let Some(e) = self.poisoned {
            return Err(e);
        }
        let input_tokens = sum_field(&self.events, "input_tokens")?;
        let output_tokens = sum_field(&self.events, "output_tokens")?;
        let cache_read_input_tokens = sum_field(&self.events, "cache_read_input_tokens")?;
        let cache_creation_input_tokens = sum_field(&self.events, "cache_creation_input_tokens")?;
        let reasoning_output_tokens = sum_field(&self.events, "reasoning_output_tokens")?;

        Ok(json!({
            "schema": "nanika.codex-usage-report.v1",
            "runtime": "codex",
            "granularity": "provider-turn",
            "provider_turn_count": self.events.len() as u64,
            "assistant_message_count": Value::Null,
            "tool_call_count": Value::Null,
            "turns": self.events.clone(),
            "summary": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "cache_read_input_tokens": cache_read_input_tokens,
                "cache_creation_input_tokens": cache_creation_input_tokens,
                "reasoning_output_tokens": reasoning_output_tokens,
                "cost_usd": Value::Null,
            },
            "quality": {
                "failed_turn_count": self.failed_count,
                "incomplete_turn_count": if self.active_turn.is_some() { 1u64 } else { 0u64 },
                "duplicate_terminal_count": self.duplicate_terminal_count,
            }
        }))
    }
}

pub(crate) fn report(bytes: &[u8]) -> Result<Value, &'static str> {
    if bytes.len() > MAX_TOTAL_BYTES {
        return Err("input exceeds 64MiB total limit");
    }
    let mut collector = CodexUsage::default();
    for line in bytes.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            return Err("line exceeds 4MiB limit");
        }
        let unique: Unique =
            serde_json::from_slice(line).map_err(|_| "invalid JSON or duplicate keys")?;
        let row = unique.0;
        if !row.is_object() {
            return Err("row must be a JSON object");
        }
        collector.observe(&row)?;
    }
    collector.finish()
}

fn validate_thread_id(value: &Value) -> Result<String, &'static str> {
    let s = value.as_str().ok_or("thread_id must be a string")?;
    if s.is_empty() || s.len() > MAX_THREAD_ID_LEN {
        return Err("thread_id length invalid");
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b':' || b == b'-')
    {
        return Err("thread_id contains invalid characters");
    }
    Ok(s.to_string())
}

fn validate_usage(usage: &Value) -> Result<UsageCounters, &'static str> {
    let obj = usage.as_object().ok_or("usage must be an object")?;
    let input = get_u64_opt(obj, "input_tokens")?;
    let cached = get_u64_opt(obj, "cached_input_tokens")?;
    let cache_write = get_u64_opt(obj, "cache_write_input_tokens")?;
    let output = get_u64_opt(obj, "output_tokens")?;
    let reasoning = get_u64_opt(obj, "reasoning_output_tokens")?;

    if let (Some(c), Some(i)) = (cached, input) {
        if c > i {
            return Err("cache_read exceeds input tokens");
        }
    }
    if let (Some(r), Some(o)) = (reasoning, output) {
        if r > o {
            return Err("reasoning tokens exceed output tokens");
        }
    }

    Ok(UsageCounters {
        input,
        cached,
        cache_write,
        output,
        reasoning,
    })
}

fn get_u64_opt(obj: &Map<String, Value>, key: &str) -> Result<Option<u64>, &'static str> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Number(n)) => match n.as_u64() {
            Some(u) => Ok(Some(u)),
            None => Err("counter must be a non-negative integer"),
        },
        _ => Err("counter must be a non-negative integer"),
    }
}

fn sum_field(events: &[Value], key: &str) -> Result<Option<u64>, &'static str> {
    let mut total: u64 = 0;
    for ev in events {
        match ev.get("usage").and_then(|u| u.get(key)) {
            Some(Value::Number(n)) => {
                let u = n.as_u64().ok_or("corrupt usage field")?;
                total = total.checked_add(u).ok_or("usage sum overflow")?;
            }
            Some(Value::Null) | None => return Ok(None),
            _ => return Err("corrupt usage field"),
        }
    }
    Ok(Some(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: Value) -> Vec<u8> {
        let mut s = v.to_string().into_bytes();
        s.push(b'\n');
        s
    }

    #[test]
    fn happy_path_single_turn() -> Result<(), String> {
        let mut bytes = Vec::new();
        bytes.extend(line(json!({"type":"thread.started","thread_id":"t-1"})));
        bytes.extend(line(json!({"type":"turn.started"})));
        bytes.extend(line(
            json!({"type":"item.started","item":{"id":"1","type":"x"}}),
        ));
        bytes.extend(line(json!({"type":"turn.completed","usage":{
            "input_tokens": 100,
            "cached_input_tokens": 40,
            "cache_write_input_tokens": 10,
            "output_tokens": 20,
            "reasoning_output_tokens": 5
        }})));
        let report = report(&bytes)?;
        assert_eq!(report["provider_turn_count"], json!(1));
        assert_eq!(report["turns"][0]["turn_id"], json!("turn-1"));
        assert_eq!(report["turns"][0]["uncached_input_tokens"], json!(60));
        assert_eq!(report["summary"]["input_tokens"], json!(100));
        assert_eq!(report["quality"]["failed_turn_count"], json!(0));
        Ok(())
    }

    #[test]
    fn overlapping_turn_started_poisons() -> Result<(), String> {
        let mut bytes = Vec::new();
        bytes.extend(line(json!({"type":"thread.started","thread_id":"t-1"})));
        bytes.extend(line(json!({"type":"turn.started"})));
        bytes.extend(line(json!({"type":"turn.started"})));
        assert!(report(&bytes).is_err());
        Ok(())
    }

    #[test]
    fn duplicate_completed_with_same_usage_is_deduped() -> Result<(), String> {
        let mut c = CodexUsage::default();
        c.observe(&json!({"type":"thread.started","thread_id":"t-1"}))?;
        c.observe(&json!({"type":"turn.started"}))?;
        let usage = json!({"input_tokens":10,"output_tokens":5});
        c.observe(&json!({"type":"turn.completed","usage": usage}))?;
        c.observe(&json!({"type":"turn.completed","usage": usage}))?;
        let report = c.finish()?;
        assert_eq!(report["quality"]["duplicate_terminal_count"], json!(1));
        assert_eq!(report["provider_turn_count"], json!(1));
        Ok(())
    }

    #[test]
    fn conflicting_duplicate_completed_errors() -> Result<(), String> {
        let mut c = CodexUsage::default();
        c.observe(&json!({"type":"thread.started","thread_id":"t-1"}))?;
        c.observe(&json!({"type":"turn.started"}))?;
        c.observe(&json!({"type":"turn.completed","usage":{"input_tokens":10}}))?;
        let res = c.observe(&json!({"type":"turn.completed","usage":{"input_tokens":11}}));
        assert!(res.is_err());
        assert!(c.finish().is_err());
        Ok(())
    }

    #[test]
    fn failed_turn_increments_quality_counter() -> Result<(), String> {
        let mut c = CodexUsage::default();
        c.observe(&json!({"type":"thread.started","thread_id":"t-1"}))?;
        c.observe(&json!({"type":"turn.started"}))?;
        c.observe(&json!({"type":"turn.failed"}))?;
        let report = c.finish()?;
        assert_eq!(report["quality"]["failed_turn_count"], json!(1));
        assert_eq!(report["provider_turn_count"], json!(0));
        Ok(())
    }

    #[test]
    fn invalid_thread_id_rejected() -> Result<(), String> {
        let mut c = CodexUsage::default();
        assert!(
            c.observe(&json!({"type":"thread.started","thread_id":"bad id!"}))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn cache_read_exceeds_input_rejected() -> Result<(), String> {
        let mut c = CodexUsage::default();
        c.observe(&json!({"type":"thread.started","thread_id":"t-1"}))?;
        c.observe(&json!({"type":"turn.started"}))?;
        let res = c.observe(&json!({"type":"turn.completed","usage":{
            "input_tokens": 5, "cached_input_tokens": 6
        }}));
        assert!(res.is_err());
        Ok(())
    }
}
