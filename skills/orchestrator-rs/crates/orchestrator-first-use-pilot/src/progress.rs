//! Best-effort live diagnostics, separate from authoritative phase results.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use orchestrator_process::{ProcessOutputSender, ProcessOutputStream, output_channel};
use serde_json::{Value, json};

use crate::{PilotError, PilotSummary};

const QUEUE_EVENTS: usize = 32;
const WAIT: Duration = Duration::from_millis(100);
const HEARTBEAT: Duration = Duration::from_secs(5);

#[derive(Clone, Default)]
pub(crate) struct Progress(Option<Arc<Publisher>>);

struct Publisher {
    sender: SyncSender<Value>,
    dropped: AtomicU64,
}

impl Progress {
    pub(crate) fn enabled(&self) -> bool {
        self.0.is_some()
    }

    pub(crate) fn emit(&self, event: Value) {
        if let Some(publisher) = &self.0 {
            if publisher.sender.try_send(event).is_err() {
                let _ = publisher
                    .dropped
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                        Some(n.saturating_add(1))
                    });
            }
        }
    }

    pub(crate) fn stage(&self, stage: &str, phase: Option<&str>) {
        self.emit(json!({"kind": "stage", "stage": stage, "phase_id": phase}));
    }

    pub(crate) fn forward_observed<T: Send>(
        &self,
        phase: &str,
        runtime: Option<&str>,
        execute: impl FnOnce(ProcessOutputSender) -> T + Send,
    ) -> Result<T, PilotError> {
        let (sender, receiver) = output_channel();
        let mut usage = LiveUsageDecoder {
            codex: (runtime == Some("codex")).then(crate::usage_codex::CodexUsage::default),
            ..Default::default()
        };
        let usage_enabled = matches!(runtime, Some("claude" | "codex"));
        let mut observed_loss = 0;
        thread::scope(|scope| {
            let worker = scope.spawn(move || execute(sender));
            loop {
                let finished = worker.is_finished();
                match receiver.recv_timeout(if finished { Duration::ZERO } else { WAIT }) {
                    Ok(chunk) => {
                        let lost = receiver.dropped_bytes();
                        if usage_enabled && lost != observed_loss {
                            usage.disable_after_loss();
                            self.emit(json!({"kind":"worker.usage_unavailable","phase_id":phase,"reason":"live source output dropped","bytes":lost-observed_loss}));
                            observed_loss = lost;
                        }
                        if usage_enabled && matches!(chunk.stream, ProcessOutputStream::Stdout) {
                            for mut event in usage.push(&chunk.bytes) {
                                event["phase_id"] = json!(phase);
                                self.emit(event);
                            }
                        }
                        self.emit(json!({
                            "kind": "process_output",
                            "phase_id": phase,
                            "stream": match chunk.stream {
                                ProcessOutputStream::Stdout => "stdout",
                                ProcessOutputStream::Stderr => "stderr",
                            },
                            // A chunk can split UTF-8. Exact bytes remain in the captured artifact.
                            "text": String::from_utf8_lossy(&chunk.bytes),
                            "encoding": "utf8-lossy-chunk",
                        }));
                    }
                    Err(RecvTimeoutError::Timeout) if !finished => continue,
                    Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
                }
            }
            let dropped = receiver.dropped_bytes();
            if usage_enabled && dropped != observed_loss {
                usage.disable_after_loss();
                self.emit(json!({"kind":"worker.usage_unavailable","phase_id":phase,"reason":"live source output dropped","bytes":dropped-observed_loss}));
            }
            if usage_enabled && !usage.disabled && usage.framing.pending() {
                self.emit(json!({"kind":"worker.usage_unavailable","phase_id":phase,"reason":"unterminated final usage frame"}));
            }
            if dropped != 0 {
                self.emit(json!({"kind":"output_dropped", "phase_id":phase, "bytes":dropped}));
            }
            worker.join().map_err(|_| {
                PilotError::Composition("live-output execution worker panicked".into())
            })
        })
    }
}

#[derive(Default)]
struct LiveUsageDecoder {
    framing: crate::observe::framing::Framer,
    usage: crate::usage_live::LiveUsage,
    codex: Option<crate::usage_codex::CodexUsage>,
    disabled: bool,
}
impl LiveUsageDecoder {
    fn disable_after_loss(&mut self) {
        // Loss counts are not ordered with queued chunks. Even an old start
        // still waiting in the FIFO must never rebuild pre-gap correlation.
        self.disabled = true;
        self.framing.lose_sync();
        self.usage.reset();
    }
    fn push(&mut self, bytes: &[u8]) -> Vec<Value> {
        if self.disabled {
            return Vec::new();
        }
        let mut events = Vec::new();
        for byte in bytes {
            if self.disabled {
                break;
            }
            let Some(line) = self.framing.push(*byte) else {
                continue;
            };
            if !line.truncated && line.bytes.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if let Some(crate::usage::Unique(row)) = (!line.truncated)
                .then(|| serde_json::from_slice(&line.bytes).ok())
                .flatten()
            {
                if let Some(codex) = self.codex.as_mut() {
                    match codex.observe(&row) {
                        Ok(observed) => events.extend(observed),
                        Err(reason) => {
                            self.disabled = true;
                            events.push(json!({"kind":"worker.usage_unavailable","runtime":"codex","reason":reason}));
                        }
                    }
                } else {
                    events.extend(self.usage.observe(&row));
                }
            } else {
                self.usage.reset();
                if self.codex.is_some() {
                    self.disabled = true;
                }
                events.push(json!({"kind":"worker.usage_unavailable","reason":"invalid, ambiguous or oversized live frame"}));
            }
        }
        events
    }
}

fn channel() -> (Progress, Receiver<Value>) {
    let (sender, receiver) = sync_channel(QUEUE_EVENTS);
    (
        Progress(Some(Arc::new(Publisher {
            sender,
            dropped: AtomicU64::new(0),
        }))),
        receiver,
    )
}

fn write_event(output: &mut impl Write, mut event: Value, elapsed: Duration) -> bool {
    event["schema"] = json!("nanika.rust-pilot.progress.v1");
    event["elapsed_ms"] = json!(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
    writeln!(output, "{event}")
        .and_then(|()| output.flush())
        .is_ok()
}

/// Keeps display I/O outside the execution thread. A full queue or closed
/// viewer never changes process receipts, watchdog activity or gate outcomes.
pub(crate) fn run(
    output: &mut impl Write,
    execute: impl FnOnce(Progress) -> Result<PilotSummary, PilotError> + Send,
) -> Result<PilotSummary, PilotError> {
    let (progress, receiver) = channel();
    let counters = progress.clone();
    let started = Instant::now();
    thread::scope(|scope| {
        let worker = scope.spawn(move || execute(progress));
        let mut writable = true;
        let mut heartbeat_at = Instant::now();
        let mut last_output: Option<Instant> = None;
        loop {
            let finished = worker.is_finished();
            match receiver.recv_timeout(if finished { Duration::ZERO } else { WAIT }) {
                Ok(event) => {
                    if event["kind"] == "process_output" {
                        last_output = Some(Instant::now());
                    }
                    if writable {
                        writable = write_event(output, event, started.elapsed());
                    }
                }
                Err(RecvTimeoutError::Timeout) if !finished => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
            if writable && heartbeat_at.elapsed() >= HEARTBEAT {
                writable = write_event(
                    output,
                    json!({
                        "kind":"heartbeat",
                        "meaning":"pilot execution thread has not returned; not proof of provider progress",
                        "seconds_since_output":last_output.map(|at| at.elapsed().as_secs()),
                    }),
                    started.elapsed(),
                );
                heartbeat_at = Instant::now();
            }
        }
        if let Some(publisher) = &counters.0 {
            let dropped = publisher.dropped.load(Ordering::Relaxed);
            if writable && dropped != 0 {
                let _ = write_event(
                    output,
                    json!({"kind":"progress_dropped", "events":dropped}),
                    started.elapsed(),
                );
            }
        }
        worker
            .join()
            .map_err(|_| PilotError::Composition("pilot execution worker panicked".into()))?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_usage_is_live_at_terminal_and_disabled_after_malformed_or_lost_source()
    -> Result<(), String> {
        let mut decoder = LiveUsageDecoder {
            codex: Some(crate::usage_codex::CodexUsage::default()),
            ..Default::default()
        };
        let wire = include_bytes!("codex-success.jsonl");
        let split = wire.len() / 2;
        assert!(decoder.push(&wire[..split]).is_empty());
        let events = decoder.push(&wire[split..]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["granularity"], "provider-turn");
        assert_eq!(events[0]["usage"]["input_tokens"], 10430);
        let error = decoder.push(b"{invalid}\n");
        assert_eq!(error[0]["kind"], "worker.usage_unavailable");
        assert!(decoder.push(wire).is_empty());
        let mut decoder = LiveUsageDecoder {
            codex: Some(crate::usage_codex::CodexUsage::default()),
            ..Default::default()
        };
        decoder.disable_after_loss();
        assert!(decoder.push(wire).is_empty());
        Ok(())
    }

    #[test]
    fn detected_loss_prevents_queued_starts_from_rebuilding_correlation() {
        let mut decoder = LiveUsageDecoder::default();
        decoder.disable_after_loss();
        let queued_start = br#"{"type":"stream_event","session_id":"s","event":{"type":"message_start","message":{"id":"old","usage":{"input_tokens":1}}}}
"#;
        let post_gap_delta = br#"{"type":"stream_event","session_id":"s","event":{"type":"message_delta","usage":{"output_tokens":99}}}
"#;
        assert!(decoder.push(queued_start).is_empty());
        assert!(decoder.push(post_gap_delta).is_empty());
        assert!(decoder.disabled);
    }

    #[test]
    fn usage_decoder_handles_split_frames_and_duplicate_keys()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut decoder = LiveUsageDecoder::default();
        let wire = br#"{"type":"assistant","session_id":"s","message":{"id":"m","usage":{"input_tokens":1}}}
"#;
        assert!(decoder.push(&wire[..20]).is_empty());
        let events = decoder.push(&wire[20..]);
        assert_eq!(
            events.first().ok_or("missing event")?["kind"],
            "worker.usage"
        );
        let invalid = br#"{"type":"assistant","session_id":"s","message":{"id":"m","usage":{"input_tokens":1,"input_tokens":2}}}
"#;
        assert_eq!(
            decoder.push(invalid).first().ok_or("missing refusal")?["kind"],
            "worker.usage_unavailable"
        );
        Ok(())
    }

    #[test]
    fn usage_decoder_rejects_oversize_and_recovers_after_newline()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut decoder = LiveUsageDecoder::default();
        assert!(decoder.push(&vec![b'x'; 300_000]).is_empty());
        assert_eq!(
            decoder.push(b"\n").first().ok_or("missing refusal")?["kind"],
            "worker.usage_unavailable"
        );
        let wire = br#"{"type":"assistant","session_id":"s","message":{"id":"m","usage":{"input_tokens":1}}}
"#;
        assert_eq!(
            decoder.push(wire).first().ok_or("missing recovery")?["kind"],
            "worker.usage"
        );
        Ok(())
    }

    #[test]
    fn unread_progress_queue_is_bounded_and_counts_loss() {
        let (progress, receiver) = channel();
        for n in 0..100 {
            progress.emit(json!({"kind":"stage", "number":n}));
        }
        assert_eq!(receiver.try_iter().count(), QUEUE_EVENTS);
        assert_eq!(
            progress
                .0
                .as_ref()
                .map(|p| p.dropped.load(Ordering::Relaxed)),
            Some(68)
        );
    }

    #[test]
    fn terminal_control_characters_are_json_escaped() {
        let mut output = Vec::new();
        assert!(write_event(
            &mut output,
            json!({"kind":"process_output", "text":"\u{1b}[2J\n"}),
            Duration::ZERO
        ));
        assert!(!output.contains(&0x1b));
        assert_eq!(output.iter().filter(|&&byte| byte == b'\n').count(), 1);
    }

    #[test]
    fn broken_viewer_does_not_change_execution_result() -> Result<(), PilotError> {
        struct BrokenViewer;
        impl Write for BrokenViewer {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let result = run(&mut BrokenViewer, |progress| {
            for _ in 0..100 {
                progress.stage("working", Some("phase-1"));
            }
            Ok(PilotSummary {
                completed: true,
                status: "completed",
                reason: String::new(),
                result_path: "result.json".into(),
            })
        })?;
        assert!(result.completed);
        Ok(())
    }
}
