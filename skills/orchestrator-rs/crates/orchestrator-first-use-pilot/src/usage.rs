//! Read-only, bounded Claude usage replay. Observational evidence, not execution acceptance.
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LINE: usize = 4 * 1024 * 1024;
const MAX_IDENTITIES: usize = 4096;
const FIELDS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];
type Result<T> = std::result::Result<T, String>;
type Identity = (String, String);

#[derive(Default)]
struct Message {
    counters: [Option<u64>; 4],
    ambiguous: bool,
    snapshots: u64,
    tools: BTreeSet<String>,
}

// Reject duplicate keys before any counters or identities are interpreted.
pub(crate) struct Unique(pub(crate) Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("unambiguous JSON")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(Value::Number(n)))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::String(v)))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(Unique(v)) = a.next_element()? {
                    values.push(v);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = Map::new();
                while let Some(key) = a.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate JSON key"));
                    }
                    let Unique(value) = a.next_value()?;
                    values.insert(key, value);
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Default)]
struct StreamUsage {
    usage: Message,
    final_usage: bool,
}

fn label(value: &Value) -> Result<String> {
    let text = value.as_str().ok_or("invalid identity or label")?;
    if text.is_empty()
        || text.len() > 160
        || !text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
    {
        return Err("unsafe or oversized identity or label".into());
    }
    Ok(text.to_owned())
}

fn optional_label(object: &Value, key: &str) -> Result<Option<String>> {
    object.get(key).map(label).transpose()
}

fn counters(usage: &Value) -> Result<[Option<u64>; 4]> {
    let object = usage.as_object().ok_or("invalid usage object")?;
    let mut result = [None; 4];
    for (index, name) in FIELDS.iter().enumerate() {
        if let Some(value) = object.get(*name) {
            result[index] = Some(value.as_u64().ok_or("invalid usage counter")?);
        }
    }
    Ok(result)
}

fn total(values: &[Option<u64>]) -> Result<Option<u64>> {
    let mut sum = 0_u64;
    let mut complete = true;
    for value in values {
        if let Some(value) = value {
            sum = sum.checked_add(*value).ok_or("usage arithmetic overflow")?;
        } else {
            complete = false;
        }
    }
    Ok(complete.then_some(sum))
}

impl Message {
    fn observe(&mut self, values: [Option<u64>; 4]) {
        for (index, incoming) in values.iter().enumerate() {
            if let Some(incoming) = incoming {
                if let Some(previous) = self.counters[index] {
                    if (index == 1 && *incoming < previous) || (index != 1 && *incoming != previous)
                    {
                        self.ambiguous = true;
                    }
                }
                self.counters[index] = Some(*incoming);
            }
        }
        self.snapshots += 1;
    }

    fn context(&self) -> Result<Option<u64>> {
        let value = total(&[self.counters[0], self.counters[2], self.counters[3]])?;
        Ok(if self.ambiguous { None } else { value })
    }
}

fn result_evidence(value: &Value) -> Result<Value> {
    let turns = value
        .get("num_turns")
        .map(|v| v.as_u64().ok_or("invalid reported turn count"))
        .transpose()?;
    let cost = value
        .get("total_cost_usd")
        .map(|v| {
            v.as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .ok_or("invalid reported cost")
        })
        .transpose()?;
    let is_error = value
        .get("is_error")
        .map(|v| v.as_bool().ok_or("invalid result error flag"))
        .transpose()?;
    let subtype = optional_label(value, "subtype")?;
    Ok(json!({"num_turns":turns,"total_cost_usd":cost,"is_error":is_error,"subtype":subtype}))
}

pub(crate) fn report(bytes: &[u8]) -> Result<Value> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("source byte limit exceeded".into());
    }
    let mut messages: BTreeMap<Identity, Message> = BTreeMap::new();
    let mut order = Vec::new();
    let mut tools: BTreeMap<Identity, (Option<Identity>, String)> = BTreeMap::new();
    let mut active = BTreeMap::<String, Identity>::new();
    let mut streamed = BTreeMap::<Identity, StreamUsage>::new();
    let mut terminals = BTreeMap::new();
    let (mut frames, mut uncorrelated, mut unknown) = (0_u64, 0_u64, 0_u64);
    for line in bytes.split(|b| *b == b'\n') {
        if line.len() > MAX_LINE {
            return Err("record byte limit exceeded".into());
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Unique(row) =
            serde_json::from_slice(line).map_err(|_| "malformed or ambiguous JSON record")?;
        if !row.is_object() {
            return Err("record must be an object".into());
        }
        match row.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                frames += 1;
                let m = row
                    .get("message")
                    .filter(|m| m.is_object())
                    .ok_or("invalid assistant envelope")?;
                let session = optional_label(&row, "session_id")?;
                let id = optional_label(m, "id")?;
                let identity = session.clone().zip(id);
                let values = m.get("usage").map(counters).transpose()?;
                if let Some(identity) = &identity {
                    if !messages.contains_key(identity) {
                        if messages.len() == MAX_IDENTITIES {
                            return Err("message identity limit exceeded".into());
                        }
                        order.push(identity.clone());
                    }
                    let message = messages.entry(identity.clone()).or_default();
                    if let Some(values) = values {
                        message.observe(values);
                    }
                } else if values.is_some() {
                    uncorrelated += 1;
                }
                if let Some(content) = m.get("content") {
                    for block in content.as_array().ok_or("invalid assistant content")? {
                        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                            continue;
                        }
                        let tool_id = label(block.get("id").ok_or("missing tool identity")?)?;
                        let name = label(block.get("name").ok_or("missing tool name")?)?;
                        let Some(session) = &session else {
                            uncorrelated += 1;
                            continue;
                        };
                        let key = (session.clone(), tool_id);
                        if let Some((previous_message, previous_name)) = tools.get_mut(&key) {
                            if previous_name != &name
                                || (previous_message.is_some()
                                    && identity.is_some()
                                    && previous_message != &identity)
                            {
                                return Err("conflicting tool identity".into());
                            }
                            if previous_message.is_none() {
                                *previous_message = identity.clone();
                            }
                        } else {
                            if tools.len() == MAX_IDENTITIES {
                                return Err("tool identity limit exceeded".into());
                            }
                            tools.insert(key, (identity.clone(), name.clone()));
                        }
                        if let Some(message) = identity.as_ref().and_then(|id| messages.get_mut(id))
                        {
                            message.tools.insert(name);
                        }
                    }
                }
            }
            Some("stream_event") => {
                let event = row
                    .get("event")
                    .filter(|v| v.is_object())
                    .ok_or("invalid stream event")?;
                let session = optional_label(&row, "session_id")?;
                match event.get("type").and_then(Value::as_str) {
                    Some("message_start") => {
                        let m = event
                            .get("message")
                            .filter(|v| v.is_object())
                            .ok_or("invalid stream message")?;
                        let id = optional_label(m, "id")?;
                        let values = m.get("usage").map(counters).transpose()?;
                        let previous = session.as_ref().and_then(|session| active.remove(session));
                        let Some(identity) = session.zip(id) else {
                            uncorrelated += 1;
                            continue;
                        };
                        if !streamed.contains_key(&identity) && streamed.len() == MAX_IDENTITIES {
                            return Err("stream identity limit exceeded".into());
                        }
                        if previous.is_some_and(|previous| previous != identity) {
                            return Err("overlapping stream messages in one session".into());
                        }
                        active.insert(identity.0.clone(), identity.clone());
                        let stream = streamed.entry(identity).or_default();
                        if let Some(values) = values {
                            stream.usage.observe(values);
                        }
                    }
                    Some("message_delta") => {
                        let values = event.get("usage").map(counters).transpose()?;
                        if let Some(values) = values {
                            let stream = session
                                .as_ref()
                                .and_then(|s| active.get(s))
                                .and_then(|id| streamed.get_mut(id));
                            if let Some(stream) = stream {
                                stream.usage.observe(values);
                                if values[1].is_some()
                                    && event
                                        .pointer("/delta/stop_reason")
                                        .is_some_and(Value::is_string)
                                {
                                    stream.final_usage = true;
                                }
                            } else {
                                uncorrelated += 1;
                            }
                        }
                    }
                    Some("message_stop") => {
                        if let Some(session) = session {
                            active.remove(&session);
                        }
                    }
                    _ => {}
                }
            }
            Some("result") => {
                let evidence = result_evidence(&row)?;
                let Some(session) = optional_label(&row, "session_id")? else {
                    uncorrelated += 1;
                    continue;
                };
                if let Some(previous) = terminals.get(&session) {
                    if previous != &evidence {
                        return Err("conflicting terminal evidence".into());
                    }
                } else {
                    if terminals.len() == MAX_IDENTITIES {
                        return Err("session identity limit exceeded".into());
                    }
                    terminals.insert(session, evidence);
                }
            }
            Some("user" | "system" | "rate_limit_event") => {}
            _ => unknown += 1,
        }
    }
    let mut orphan_streams = 0_u64;
    for (id, stream) in &streamed {
        if let Some(message) = messages.get_mut(id) {
            message.ambiguous |= stream.usage.ambiguous;
            for i in 0..4 {
                if let Some(value) = stream.usage.counters[i] {
                    if i == 1 {
                        message.counters[i] =
                            Some(message.counters[i].map_or(value, |old| old.max(value)));
                    } else {
                        if message.counters[i].is_some_and(|old| old != value) {
                            message.ambiguous = true;
                        }
                        message.counters[i] = Some(value);
                    }
                }
            }
        } else {
            orphan_streams += 1;
        }
    }
    let mut rows = Vec::new();
    let (mut first, mut last, mut peak, mut peak_ordinal) = (None, None, None, None);
    let (mut ambiguous, mut incomplete) = (0_u64, 0_u64);
    let mut sums = [Some(0_u64); 4];
    for (index, identity) in order.iter().enumerate() {
        let message = messages.get(identity).ok_or("lost message identity")?;
        let context = message.context()?;
        if message.ambiguous {
            ambiguous += 1;
        }
        if message.counters.iter().any(Option::is_none) {
            incomplete += 1;
        }
        if let Some(context) = context {
            if first.is_none() {
                first = Some(context);
            }
            last = Some(context);
            if peak.is_none_or(|n| context > n) {
                peak = Some(context);
                peak_ordinal = Some(index + 1);
            }
        }
        let mut usage = Map::new();
        for i in 0..4 {
            usage.insert(FIELDS[i].into(), json!(message.counters[i]));
            sums[i] = if message.ambiguous {
                None
            } else {
                total(&[sums[i], message.counters[i]])?
            };
        }
        rows.push(
            json!({"ordinal":index+1,"session_id":identity.0,"message_id":identity.1,
            "usage_snapshots":message.snapshots,"usage":usage,"ambiguous":message.ambiguous,
            "observed_input_tokens":context,"own_tool_names":message.tools,
            "final_stream_output_observed":streamed.get(identity).is_some_and(|s|s.final_usage)}),
        );
    }
    if order.is_empty() {
        sums = [None; 4];
    }
    let usage_totals: Map<String, Value> = FIELDS
        .iter()
        .zip(sums)
        .map(|(name, value)| ((*name).into(), json!(value)))
        .collect();
    let mut tool_names = BTreeMap::<String, u64>::new();
    for (_, name) in tools.values() {
        *tool_names.entry(name.clone()).or_default() += 1;
    }
    Ok(
        json!({"schema":"nanika.portal.usage-replay.v1", "evidence_kind":"saved-stream observation; not execution acceptance or canonical accounting",
        "portal_mode":"historical-unrecorded", "source_bytes":bytes.len(), "source_sha256":format!("{:x}",Sha256::digest(bytes)),
        "assistant_envelopes":frames,"assistant_message_count":messages.len(),"tool_call_count":tools.len(),"tools":tool_names,
        "messages":rows,"provider_results":terminals,
        "summary":{"first_available_observed_input_tokens":first,"last_available_observed_input_tokens":last,
            "peak_observed_input_tokens":peak,"peak_message_ordinal":peak_ordinal,"usage_totals":usage_totals},
        "quality":{"ambiguous_messages":ambiguous,"incomplete_messages":incomplete,"uncorrelated_records":uncorrelated,"unknown_records":unknown,"orphan_stream_messages":orphan_streams,
            "tools_without_message_identity":tools.values().filter(|(id,_)|id.is_none()).count(),
            "messages_without_final_stream_output":order.iter().filter(|id|!streamed.get(*id).is_some_and(|s|s.final_usage)).count()},
        "limitations":["Output is provisional unless a correlated final stream output snapshot was observed.","Input usage is not exact context-window occupancy.","Assistant messages, tool calls and provider num_turns are distinct.","Null totals mean incomplete or ambiguous evidence.","No Portal ON/OFF comparison has been performed."]}),
    )
}

fn stamp(m: &fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}

pub(crate) fn execute(input: &Path, output: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(input)
        .map_err(|_| "cannot open regular source")?;
    let before = file.metadata().map_err(|_| "cannot inspect source")?;
    if !before.is_file() || before.len() > MAX_BYTES {
        return Err("source must be a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read source")?;
    let after = file.metadata().map_err(|_| "cannot reinspect source")?;
    let path_after = fs::symlink_metadata(input).map_err(|_| "source path changed")?;
    if stamp(&before) != stamp(&after)
        || stamp(&before) != stamp(&path_after)
        || bytes.len() as u64 != before.len()
        || path_after.file_type().is_symlink()
    {
        return Err("source changed during read".into());
    }
    let value = report(&bytes)?;
    let mut encoded = serde_json::to_vec_pretty(&value).map_err(|_| "cannot encode report")?;
    encoded.push(b'\n');
    if stamp(&file.metadata().map_err(|_| "cannot reinspect source")?) != stamp(&before)
        || stamp(&fs::symlink_metadata(input).map_err(|_| "source path changed")?) != stamp(&before)
    {
        return Err("source changed during parsing".into());
    }
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)
        .map_err(|_| "output must be a fresh writable path")?;
    destination
        .write_all(&encoded)
        .and_then(|()| destination.sync_all())
        .map_err(|_| "cannot write complete report")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(id: &str, usage: Value) -> Value {
        json!({"type":"assistant","session_id":"s","message":{"id":id,"usage":usage,"content":[]}})
    }
    fn usage() -> Value {
        json!({"input_tokens":2,"output_tokens":4,"cache_read_input_tokens":10,"cache_creation_input_tokens":3})
    }
    fn parse(rows: &[Value]) -> Result<Value> {
        report(
            rows.iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                .as_bytes(),
        )
    }
    #[test]
    fn duplicate_snapshots_and_fragments_are_not_summed() -> Result<()> {
        let a = row("m", usage());
        let mut b = a.clone();
        b["message"]["usage"]["output_tokens"] = json!(8);
        let r = parse(&[
            a.clone(),
            a,
            b,
            json!({"type":"stream_event","event":{"type":"message_delta","usage":{"output_tokens":8}}}),
        ])?;
        assert_eq!(r["assistant_message_count"], 1);
        assert_eq!(r["summary"]["usage_totals"]["output_tokens"], 8);
        assert_eq!(r["summary"]["peak_observed_input_tokens"], 15);
        Ok(())
    }
    #[test]
    fn missing_counters_are_unavailable() -> Result<()> {
        let r = parse(&[row("m", json!({"input_tokens":2}))])?;
        assert!(r["messages"][0]["observed_input_tokens"].is_null());
        assert!(r["summary"]["usage_totals"]["output_tokens"].is_null());
        Ok(())
    }
    #[test]
    fn conflicting_input_and_regressing_output_are_ambiguous() -> Result<()> {
        for field in ["input_tokens", "output_tokens"] {
            let a = row("m", usage());
            let mut b = a.clone();
            b["message"]["usage"][field] = json!(1);
            let r = parse(&[a, b])?;
            assert_eq!(r["quality"]["ambiguous_messages"], 1);
            assert!(r["messages"][0]["observed_input_tokens"].is_null());
            assert!(r["summary"]["usage_totals"][field].is_null());
        }
        Ok(())
    }
    #[test]
    fn sessions_separate_messages_and_parallel_tools_are_deduplicated() -> Result<()> {
        let mut a = row("m", usage());
        a["message"]["content"] = json!([{"type":"tool_use","id":"t1","name":"Read"},{"type":"tool_use","id":"t2","name":"Edit"}]);
        let mut b = a.clone();
        b["session_id"] = json!("other");
        let r = parse(&[a.clone(), a, b])?;
        assert_eq!(r["assistant_message_count"], 2);
        assert_eq!(r["tool_call_count"], 4);
        Ok(())
    }
    #[test]
    fn invalid_counters_overflow_and_reused_tools_fail() -> Result<()> {
        for value in [json!(-1), json!(0.1), json!("2"), Value::Null] {
            let mut u = usage();
            u["input_tokens"] = value;
            assert!(parse(&[row("m", u)]).is_err());
        }
        let mut u = usage();
        u["input_tokens"] = json!(u64::MAX);
        assert!(parse(&[row("m", u)]).is_err());
        let mut a = row("m", usage());
        a["message"]["content"] = json!([{"type":"tool_use","id":"t","name":"Read"}]);
        let mut b = a.clone();
        b["message"]["id"] = json!("other");
        assert!(parse(&[a, b]).is_err());
        Ok(())
    }
    #[test]
    fn uncorrelated_usage_and_unknown_records_are_counted() -> Result<()> {
        let mut a = row("m", usage());
        a["message"].as_object_mut().ok_or("object")?.remove("id");
        let r = parse(&[a, json!({"type":"new_protocol"})])?;
        assert_eq!(r["assistant_message_count"], 0);
        assert_eq!(r["quality"]["uncorrelated_records"], 1);
        assert_eq!(r["quality"]["unknown_records"], 1);
        Ok(())
    }
    #[test]
    fn raw_content_never_enters_report_and_bad_labels_fail() -> Result<()> {
        let mut a = row("m", usage());
        a["message"]["content"] = json!([{"type":"text","text":"SECRET_RAW"},{"type":"tool_use","id":"t","name":"Read","input":{"password":"SECRET_RAW"}}]);
        assert!(!parse(&[a.clone()])?.to_string().contains("SECRET_RAW"));
        a["message"]["id"] = json!("unsafe\nlabel");
        assert!(parse(&[a]).is_err());
        Ok(())
    }
    #[test]
    fn terminal_evidence_remains_distinct_and_duplicate_idempotent() -> Result<()> {
        let t = json!({"type":"result","session_id":"s","num_turns":45,"total_cost_usd":2.0,"subtype":"success","is_error":false,"result":"SECRET_RAW"});
        let r = parse(&[row("m", usage()), t.clone(), t])?;
        assert_eq!(r["provider_results"]["s"]["num_turns"], 45);
        assert_eq!(r["assistant_message_count"], 1);
        assert!(!r.to_string().contains("SECRET_RAW"));
        Ok(())
    }
    #[test]
    fn malformed_oversized_and_existing_output_are_refused() -> Result<()> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "usage-replay-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).map_err(|_| "mkdir")?;
        let input = dir.join("input");
        let output = dir.join("output");
        let check = (|| {
            fs::write(&input, b"broken").map_err(|_| "write")?;
            assert!(execute(&input, &output).is_err());
            assert!(!output.exists());
            fs::write(&input, vec![b' '; MAX_LINE + 1]).map_err(|_| "write")?;
            assert!(execute(&input, &output).is_err());
            assert!(!output.exists());
            fs::write(&input, row("m", usage()).to_string()).map_err(|_| "write")?;
            fs::write(&output, b"preserve").map_err(|_| "write")?;
            assert!(execute(&input, &output).is_err());
            assert_eq!(fs::read(&output).map_err(|_| "read")?, b"preserve");
            Ok(())
        })();
        fs::remove_dir_all(&dir).map_err(|_| "cleanup")?;
        check
    }
    #[test]
    fn stream_final_usage_updates_output_without_counting_an_extra_message() -> Result<()> {
        let start = json!({"type":"stream_event","session_id":"s","event":{"type":"message_start","message":{"id":"m","usage":usage()}}});
        let delta = json!({"type":"stream_event","session_id":"s","event":{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":100}}});
        let r = parse(&[start, row("m", usage()), delta])?;
        assert_eq!(r["assistant_message_count"], 1);
        assert_eq!(r["summary"]["usage_totals"]["output_tokens"], 100);
        assert_eq!(r["quality"]["messages_without_final_stream_output"], 0);
        Ok(())
    }
    #[test]
    fn identifiable_tools_survive_missing_message_identity() -> Result<()> {
        let mut a = row("m", usage());
        a["message"].as_object_mut().ok_or("object")?.remove("id");
        a["message"]["content"] = json!([{"type":"tool_use","id":"t","name":"Read"}]);
        let r = parse(&[a.clone(), a])?;
        assert_eq!(r["tool_call_count"], 1);
        assert_eq!(r["assistant_message_count"], 0);
        assert_eq!(r["quality"]["tools_without_message_identity"], 1);
        Ok(())
    }
    #[test]
    fn duplicate_keys_are_rejected_at_every_depth() {
        for wire in [
            r#"{"type":"assistant","session_id":"a","session_id":"b"}"#,
            r#"{"type":"assistant","session_id":"s","message":{"id":"m","usage":{"input_tokens":-1,"input_tokens":2}}}"#,
            r#"{"type":"assistant","session_id":"s","message":{"id":"m","content":[{"type":"tool_use","id":"a","id":"b","name":"Read"}]}}"#,
        ] {
            assert!(report(wire.as_bytes()).is_err());
        }
    }
    #[test]
    fn stream_sessions_are_independent_and_orphan_deltas_are_explicit() -> Result<()> {
        let mut rows = Vec::new();
        for session in ["a", "b"] {
            rows.push(json!({"type":"stream_event","session_id":session,"event":{"type":"message_start","message":{"id":"m","usage":usage()}}}));
            let mut m = row("m", usage());
            m["session_id"] = json!(session);
            rows.push(m);
        }
        for (session, n) in [("a", 20), ("b", 30), ("unknown", 90)] {
            rows.push(json!({"type":"stream_event","session_id":session,"event":{"type":"message_delta","usage":{"output_tokens":n}}}));
        }
        let r = parse(&rows)?;
        assert_eq!(r["summary"]["usage_totals"]["output_tokens"], 50);
        assert_eq!(r["quality"]["uncorrelated_records"], 1);
        Ok(())
    }
    #[test]
    fn overlapping_messages_in_one_session_are_not_guessed() {
        let start = |id| json!({"type":"stream_event","session_id":"s","event":{"type":"message_start","message":{"id":id}}});
        assert!(parse(&[start("first"), start("second")]).is_err());
    }
}
