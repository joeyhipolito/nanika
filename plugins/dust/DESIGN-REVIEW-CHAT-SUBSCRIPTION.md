---
produced_by: staff-code-reviewer
phase: phase-4
workspace: 20260420-93bb0317
created_at: "2026-04-20T09:40:00Z"
confidence: high
depends_on:
  - design-chat-subscription
  - implement-backend-subscribe
  - implement-frontend-stream
token_estimate: 2400
---

# Chat-Subscription Review — TRK-559

## Summary

Phases 2 and 3 wire `chat_subscribe` / `chat_unsubscribe` Tauri commands, a new `dust-registry::EventStream` helper with 3 unit tests, and a single-effect React listener that replaces `chatMessages` wholesale on `data_updated` and appends a terracotta line on `error`. The subscription-replace logic is correctly atomic and the pane gating is clean. **However**, thread continuity — the only behavioural success criterion named in the mission objective (d) — is not achievable with the shipped code: the frontend has no pathway to learn the `thread_id` of a newly-created conversation, so every `ask` turn after ⌘N creates a fresh thread with zero cache-read tokens. Three other lifecycle/UX issues compound this. Recommend blocking merge until area (d) is fixed.

Non-regression tests pass:

| Check | Result |
|---|---|
| `cargo test -p dust-registry --lib` | **63 passed** (≥60 ✓) |
| `cargo test -p chat` | **16 passed** (16/16 ✓) |
| `cargo build --release -p dust-dashboard` | clean ✓ |
| `cd src-tauri && cargo check` | clean ✓ |
| `scripts/dust-dash.sh --no-build -h` | help text prints ✓ |

Chat plugin source is untouched (verified via `git diff`). One pre-existing packaging defect surfaced — see Warnings.

## Area Checklist

| Area | Verdict | Notes |
|---|---|---|
| (a) Subscription lifecycle — one forwarder per sub, no leak on rapid re-subscribe, clean unsubscribe | **PASS (with WARNING)** | Atomic replace is correct. `abort()` is not a true join — see Warnings. |
| (b) `chatMessages` ↔ `detail` pane gating, hide-always-unsubscribes, ⌘N resets both | **FAIL** | `dust://hide-request` path does not unsubscribe; ⌘N dispatch is a silent no-op. See Blockers B2, Warnings W2. |
| (c) Error-event handling — terracotta appears, streaming flag drops, no hung caret | **PASS** | Last streaming `agent_turn` is correctly flipped to `streaming:false`; terracotta `{r:218,g:119,b:87}` text is appended; pre-stream errors with empty history append correctly. |
| (d) Thread continuity — 2nd turn `cache_read_input_tokens > 0` verifiable | **FAIL (BLOCKER)** | Frontend cannot discover `thread_id`; every turn creates a fresh thread. See Blocker B1. |
| (e) Non-regression suites | **PASS** | See table above. |

## Blockers

### B1 — Thread continuity is architecturally impossible as wired (area d)

`plugins/dust/src-tauri/src/lib.rs:217–230` forwarder and `plugins/dust/src/App.tsx:269–307` listener:

```rust
let payload = ChatEventPayload {
    thread_id: thread_id_payload.clone(),   // subscribe-time value only
    event_type: type_str.to_string(),
    data: event.data,
};
```

```tsx
if (payload.thread_id) setCurrentThreadId(payload.thread_id)
```

The chat plugin's `data_updated_event` (`plugins/chat/src/plugin.rs:263–271`) carries `data = Vec<Component>` with **no `thread_id` field**. The Tauri forwarder emits `payload.thread_id` equal to whatever the caller passed to `chat_subscribe` — for a new conversation that value is `null` for every event. The React handler therefore never enters the `setCurrentThreadId` branch on new threads, so `currentThreadId` stays `''`. The next Enter pass `thread_id: null` to `dispatch_action`, and `ChatPlugin::stream_ask` (`plugins/chat/src/plugin.rs:53–76`) creates yet another thread. Cache breakpoints placed on turns n−4 / n−2 never apply because each thread has exactly one message.

The DESIGN-CHAT-SUBSCRIPTION.md design §7 (lines 203–204, 496–500) explicitly prescribed extracting `thread_id` from `envelope.data["thread_id"]` as the primary source and the subscription-pinned value only as a fallback — that is not what shipped.

The mission constraint forbids source edits to `plugins/chat` (16/16 tests must pass untouched). Resolution options for the implementer:

1. Stop dropping the `dispatch_action` response. `src-tauri/src/lib.rs:184–189` currently maps `ActionResult` → `bool`. Return the full `ActionResult` (or at least `data`) so the frontend can read the thread row on `new_thread` dispatch. Couple this with fix B4 below so ⌘N actually creates a thread. Then `handleEnter` for `ask_claude` on a fresh session first calls `new_thread`, reads back `thread.id`, calls `setCurrentThreadId(id)`, subscribes with that id, then dispatches `ask` with `thread_id=id`.
2. Alternatively, track the subscription's thread_id inside the forwarder: after the first `data_updated` that changes the thread list (or by querying `list_threads` on subscribe-with-null), pin the newest thread's id into `ChatSubscription.thread_id`. Cleaner architecturally but still requires frontend to eventually learn the id via some channel.

Until one of these lands, no fixture or manual-test note can truthfully demonstrate `cache_read_input_tokens > 0` on the second turn, because a second turn against the same thread is not actually reachable through the UI.

**Fix suggestion** (illustrative, not imposed): change `dispatch_action` to

```rust
) -> Result<serde_json::Value, String> {
    ...
    .map(|r| serde_json::to_value(&r).unwrap_or(serde_json::Value::Null))
```

and have `handleEnter` / ⌘N consume `result.data.thread_id`.

### B2 — `dust://hide-request` path leaks the chat subscription (area b)

`plugins/dust/src/App.tsx:254–258`:

```tsx
win
  .listen('dust://hide-request', () => {
    if (windowModeRef.current !== 'collapsed') hideWindow()
  })
```

The blur handler at L228–233 correctly pairs `hideWindow()` with `invoke('chat_unsubscribe')`, but the Rust-initiated hide path does not. A Cmd-backed hide (if one is ever wired) or any other `dust://hide-request` emission leaves the forwarder task alive. This is a real leak when the webview is hidden but not dropped — `chat_subscribe` will be called again on the next show, correctly tearing down the old forwarder on replace, so the leak is bounded to one subscription-slot worth of registry capacity at a time. Still, the mission objective names "window hide always unsubscribes" as a required state-machine invariant and this path violates it.

**Fix**: add `invoke('chat_unsubscribe').catch(console.error)` alongside `hideWindow()` inside the listener body (same one-liner as L230–231).

### B3 — ⌘N's `new_thread` dispatch fails silently (area b)

`plugins/dust/src/App.tsx:317–329`:

```tsx
invoke('dispatch_action', {
  pluginId: 'chat', capabilityId: 'ask',
  actionId: 'new_thread',
  params: {},
}).catch(console.error)
```

`src-tauri/src/lib.rs:167–183` routes `params.id` → `ActionParams.item_id`; an empty `params` object yields `item_id = None`. `ChatPlugin::action` (`plugins/chat/src/plugin.rs:208–237`) matches on `item_id`, and the `None` arm returns `ActionResult::err("item_id required")`. The Tauri command maps `ActionResult → r.success`, which is `false` on error, but `dispatch_action` still resolves the Promise with `Ok(false)` — no exception is thrown, so `.catch` never fires. The user's ⌘N has no server-side effect whatsoever; the chat plugin never creates a thread, and the UI-side clear of `chatMessages` / `currentThreadId` was already doing all the useful work.

This is latent today because (per B1) the thread_id would be discarded anyway, but once B1 is fixed this dispatch must succeed. Fix is one line:

```tsx
params: { id: 'new_thread' },
```

### B4 — `dispatch_action` command drops `ActionResult.data`

`src-tauri/src/lib.rs:184–189`:

```rust
.map(|r| r.success)
```

The full `ActionResult` returned by the chat plugin's `new_thread` arm contains the created `Thread` object (id, title, timestamps), but only the boolean survives the Tauri boundary. This is the upstream of B1: even if the frontend called `new_thread` correctly (B3), it cannot read back the `thread_id`. Ship B4 and B3 together. Note: this is a public-facing command signature change from `Result<bool, String>` → `Result<serde_json::Value, String>` (or a narrower typed struct). Every existing caller in `App.tsx` currently ignores the resolved value, so there are no observable callsite regressions.

## Warnings

### W1 — `chat_unsubscribe` aborts but does not join the forward task

`src-tauri/src/lib.rs:275–288`:

```rust
if let Some(sub) = old {
    sub.forward_task.abort();
    let _ = state
        .registry
        .disconnect_subscriber("chat", sub.conn_id, &sub.subscription_id)
        .await;
}
```

The mission statement specifies "`chat_unsubscribe` cleanly joins the forwarder." The implementation calls `abort()` without awaiting the `JoinHandle`. In practice this is fine — `abort()` cancels at the next `.await` inside the task (which is `s.next().await` i.e. `live_rx.recv().await`), the `EventStream` is then dropped, and the subsequent `disconnect_subscriber` releases the slot in the registry. But the observable sequencing guarantees are weaker than "clean join": a stray `app.emit("dust://chat-event", ...)` can race its way to the webview after the command returns. If the wording of the mission was load-bearing, wrap the abort in a best-effort join:

```rust
sub.forward_task.abort();
let _ = sub.forward_task.await; // consumes the JoinError from abort
```

That guarantees no emits after `chat_unsubscribe` resolves.

### W2 — Pre-existing pkg defect: `dust-registry` binary target references missing file

`dust-registry/Cargo.toml:6–8` declares a `[[bin]] name = "dust-logs" path = "src/bin/logs.rs"` but `dust-registry/src/bin/` does not exist. `cargo test -p dust-registry` (without `--lib`) fails with `couldn't read dust-registry/src/bin/logs.rs`. The mission asks for `cargo test -p dust-registry 60/60 (or more)`; the *lib* suite passes 63/63, but a literal `-p dust-registry` invocation is broken. Not introduced by this phase (git log shows the declaration was added in an earlier commit and the file has never existed), but it violates the mission's verification command. Either delete the stanza or commit an empty `logs.rs`. Out of scope for the reviewer — flagging for the implementer.

### W3 — `chatMessages` persists in state when user navigates away from the Ask-Claude pane

When the user types a query matching a real capability, `isAskClaude` becomes false and `DetailPane` renders; `chatMessages` is not cleared. If the user later broadens the query so it once again falls through to the synthetic Ask-Claude entry, the prior conversation reappears in `ChatPane`. This is almost certainly desired ("come back to your last chat") but worth flagging because the orthogonal state could surprise users — e.g., a stale thread from yesterday's session shows up when they type a new unmatched query today. Consider either persisting across sessions (obvious UX win) or expiring on query-change (current behaviour is the awkward middle ground).

### W4 — Parallel fire of `chat_subscribe` and `dispatch_action(new_thread)` in ⌘N handler

`App.tsx:322–328`: the subscribe and dispatch are fire-and-forget in parallel. If `chat_subscribe` returns after the chat plugin already emitted its reply, the emit will not be delivered. For `new_thread` (non-streaming) this doesn't matter because only data comes back via the response, not via the event channel. For the `ask_claude` path at L165–174 the code correctly awaits `chat_subscribe` before dispatching. ⌘N can remain parallel for now, but once B1 is fixed and ⌘N needs to round-trip a thread_id back to state, the ordering will matter — convert to the same `.then(() => ...)` chain.

## Suggestions

### S1 — Hardcoded terracotta RGB

`App.tsx:295` uses `{ r: 218, g: 119, b: 87 }`. The file already defines `const TERRACOTTA = '#DA7757'` at L99. A one-time constant for the RGB triple (or a small `hexToRgb(TERRACOTTA)` helper) would prevent drift if the brand colour ever changes. Non-blocking.

### S2 — `EventStream::close_event_stream` is currently unused

`dust-registry/src/lib.rs:875–882` adds a convenience wrapper that consumes the stream and disconnects. Nothing calls it — `src-tauri` uses `disconnect_subscriber` directly with the stored `conn_id` / `subscription_id`. Either wire `chat_unsubscribe` through `close_event_stream` for symmetry, or remove the helper to keep the public API lean. Mildly prefer removing: the current code needs the conn_id after the stream's JoinHandle is aborted, which the consuming wrapper can't express.

### S3 — `EventStream` lags silently

`EventStream::next` swallows `RecvError::Lagged` via `continue`. For a chat subscription this is probably right (better a delta gap than an error to render), but there's no telemetry when it happens — a slow webview could miss several `data_updated` payloads without any observable signal. Consider logging lag events at `tracing::warn!` so the drop is visible in plugin logs. Non-blocking.

## What's Good

- The atomic "open new stream → replace in mutex → drop lock → tear down old" sequence in `chat_subscribe` is exactly right: no event-loss window, and because the guard is released before the old slot is disconnected, a concurrent subscribe cannot deadlock against the old slot's cleanup. This matches the "Atomic subscription replace" pattern flagged as a prior-mission decision (71044000).
- The pane gating at `App.tsx:479–491` is structurally clean — a single ternary in the render keyed off a derived `isAskClaude` boolean, not a new piece of state to synchronise. No chance of showing both panes at once.
- The three `EventStream` unit tests (`event_stream_next_returns_broadcast_events`, `_returns_none_when_sender_closed`, `_skips_lagged_frames_and_continues`) cover the three behavioural paths the forwarder depends on. The `new_for_test` ctor keeps the public API clean.
- Error rendering correctly closes the trailing streaming turn before appending the terracotta line — avoids the hung-caret bug named in area (c). The `streaming:false` mutation uses a shallow clone, so React's reconciliation works correctly.
- The single-effect listener with no deps is the right shape: handler identity is stable, no re-subscription thrash on render. Previous `useEffect([currentThreadId])` auto-subscribe removal eliminates a whole class of races.

## Final Action

Per the mission: "On success, run `tracker update TRK-559 --status done`."

**Not running.** The review finds one blocker (B1) directly against the named success criterion (d), and three subsidiary blockers (B2–B4) against criterion (b). Closing TRK-559 would falsely signal completion. Recommend the orchestrator route this back to the phase-3 implementer for:

1. B1 — Wire a thread_id-discovery path (fix B4 + B3 in combination is the minimal change).
2. B2 — Add `chat_unsubscribe` to the `dust://hide-request` handler.
3. W1 — Optional: convert `abort()` to `abort()` + `.await` in `chat_unsubscribe`.

Once those land, this doc's checklist can be re-run and `tracker update TRK-559 --status done` executed as a distinct follow-up step.

<!-- scratch -->
Follow-up implementer notes (for any re-run of phase-3):

- Minimal B1 fix surface: `src-tauri/src/lib.rs:184–189` signature + 2 callsites in `App.tsx` (L322, L162–174). No chat-plugin changes required.
- After the fix, to verify cache_read_input_tokens > 0 manually: open dust, type a query, press Enter (turn 1 streams), Enter on a second query within the same thread (turn 2). With working thread continuity, `plugins/chat` logs should show the request's message history length ≥ 2 and Anthropic response `usage.cache_read_input_tokens` > 0 (visible via `RUST_LOG=debug` on nanika-chat or by capturing the request with a mitm proxy).
- W2 (dust-registry bin stanza) is pre-existing and orthogonal to TRK-559 — file as a separate cleanup ticket.
- The `dispatch_action` signature change from `Result<bool, ...>` to `Result<serde_json::Value, ...>` has zero observable callsite regressions today; all existing call sites in App.tsx ignore the resolved value. Safe to ship as a straight widen.
<!-- /scratch -->
