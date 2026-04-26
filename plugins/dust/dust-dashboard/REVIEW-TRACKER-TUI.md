---
produced_by: staff-code-reviewer
phase: phase-6
workspace: 20260417-bc6d9c82
created_at: "2026-04-17T12:15:00Z"
confidence: high
depends_on:
  - implement-rich-widget
  - implement-palette-and-live
  - implement-detail-view
  - add-id-conformance-test
token_estimate: 2400
---

# Tracker TUI Review

## Summary

Reviewed the tracker TUI stack end-to-end: the tracker plugin's `dust_serve.rs` (handshake dispatch + IPC path + mutation broadcast), the dashboard's `app.rs` / `component_renderer.rs` / `ui.rs` (state machine, selection, detail pane, live subscription), and the registry's `connect_and_subscribe` / `disconnect_subscriber` helpers. The changes are cohesive: protocol handshake is preserved on the one true lifecycle connection, frame I/O is size-capped and `read_exact`-based, and state transitions are uniformly reachable with Esc always exiting one level. No blockers; a handful of warnings and suggestions below.

## File verdicts

| File | Verdict | Notes |
| --- | --- | --- |
| `plugins/tracker/src/dust_serve.rs` | **PASS** | Split into `run_lifecycle` + `handle_ipc_request` is sound; `LIFECYCLE_DONE` is process-wide but reset in `run()`. |
| `plugins/tracker/src/lib.rs` (new) + `Cargo.toml` + `tests/plugin_id.rs` | **PASS** | Clean minimal surface exporting `PLUGIN_ID`; conformance test correctly asserts parity with `plugin.json`. |
| `plugins/dust/dust-dashboard/src/app.rs` | **PASS (with warnings)** | State machine is closed; `table_cursor` not clamped on live refresh (see W-1). |
| `plugins/dust/dust-dashboard/src/component_renderer.rs` | **PASS** | `render_with_selection` correctly scopes highlight to the first `Table` only. |
| `plugins/dust/dust-dashboard/src/ui.rs` | **PASS** | Split-pane layout and detail rendering are straightforward; relies on `cached_issues` being refreshed in app. |
| `plugins/dust/dust-dashboard/src/main.rs` | **PASS (with warning)** | No explicit `close_event_stream` on abnormal exit (see W-2). |
| `plugins/dust/dust-registry/src/lib.rs` | **PASS** | `connect_and_subscribe` closes the slot on subscribe failure; `disconnect_subscriber` unsubscribes then closes. |

## Blockers

No blockers.

## Warnings

### W-1: `table_cursor` is not clamped after component refresh

`plugins/dust/dust-dashboard/src/app.rs:291-317` — `handle_plugin_event` replaces `self.components` with the result of `render_ui`, which can return fewer table rows than before (an issue was closed, filtered out, etc.). `table_cursor` is not re-validated, so after a live event the cursor may point past `table_row_count()`. Renderer survives (`rows.iter().enumerate()` just skips non-matches and `selected_issue_detail` returns `None`), but the UX goes stale: the detail pane shows "No issue selected" until the user presses Down or Up.

Fix: after updating `self.components` in both `handle_plugin_event` and `handle_key_ui_loaded`'s `r` branch, clamp with

```rust
let count = self.table_row_count();
self.table_cursor = self.table_cursor.min(count.saturating_sub(1));
```

### W-2: abnormal exit leaks subscriber slot

`plugins/dust/dust-dashboard/src/main.rs:26-69` — on the happy path (`Esc` from UILoaded) `handle_key_ui_loaded` calls `close_event_stream` before state transition. But if `run_app` returns `Err` (draw error, panic hook fires, etc.), the event stream is never closed. The spawned subscription task is aborted implicitly when the tokio runtime shuts down, but `Registry::disconnect_subscriber` is never called, leaving the subscriber slot stale on the registry.

Fix: in `main` after `run_app` returns — regardless of `Ok`/`Err` — invoke `app.close_event_stream().await`. Alternatively, expose a synchronous `close_event_stream_blocking` that can be called in a `Drop` impl on `App`.

Process exit cleans the slot in practice (the registry lives in the same process), so the severity is low, but a multi-subscriber future would expose this.

### W-3: `close_event_stream` aborts without joining

`plugins/dust/dust-dashboard/src/app.rs:273-285` — calls `stream.task.abort()` then `disconnect_subscriber(...)` without awaiting the aborted handle. The spawned task's `live_rx.recv()` loop will terminate cleanly when the broadcast channel is dropped by the registry, so this is not a hazard. But semantically, `abort` + no `.await` leaves the join handle detached. For symmetry and easier reasoning about shutdown ordering, consider:

```rust
stream.task.abort();
let _ = stream.task.await;  // swallow JoinError(Cancelled)
```

### W-4: `LIFECYCLE_DONE` is a process-wide flag reset only on `run()` entry

`plugins/tracker/src/dust_serve.rs:65-67, 992` — `LIFECYCLE_DONE` is a static `AtomicBool`. `run()` resets it at startup, but any code path that invokes `run()` twice in the same process (unlikely in production, plausible in tests) would race with in-flight `handle_connection` calls from the previous listener. The current binary only calls `run()` once, so this is latent.

Fix (deferred): promote the flag to a `Arc<AtomicBool>` held by the listener task and cloned into each `handle_connection` spawn. Safe as-is for production.

### W-5: misleading comment in `dispatch_op`

`plugins/dust/dust-dashboard/src/app.rs:620-621` — the comment reads "Return to UILoaded before dispatching so Esc during the await still works." The main loop `await`s `handle_key` serially; no key events are polled while `dispatch_action` is in flight, so Esc cannot arrive mid-dispatch. The state transition is still worth keeping (it's what the `ActionDispatched` → `UILoaded` fallback relies on), but the rationale is incorrect.

Fix: replace the comment with "Transition optimistically so a later `dispatch_action` failure leaves the UI in UILoaded, not a transient pre-dispatch state."

## Suggestions

### S-1: `dispatch_op` sets both `item_id` and `args["item_id"]` for the `issue` prompt

`plugins/dust/dust-dashboard/src/app.rs:606-611` — harmless redundancy. `tracker::dispatch_action` reads only the envelope `item_id`. The extra `args["item_id"]` entry is dead. Consider dropping the `args.insert` in the `issue` branch.

### S-2: Cargo.lock committed for the dashboard binary

Good practice — keep it. Just noting so the directive "no unrelated files" isn't flagged.

### S-3: `render_issue_detail` derives Blockers from `cached_issues`

`plugins/dust/dust-dashboard/src/ui.rs:446` — the pane explicitly does not render blockers because `dispatch_action("tree")` doesn't expose the Link table. Scratch note from phase 4 acknowledges this. If blockers matter for the TUI later, extend the tracker `tree` op to include `blockers: [...]` in the per-issue JSON, or add a dedicated `blockers` op.

### S-4: `render_issue_detail` assumes `labels` is a string

`plugins/dust/dust-dashboard/src/ui.rs:380-388` — `issue.get("labels").and_then(|v| v.as_str())` silently treats any non-string shape as empty. Matches the current tracker serialization but would hide a future schema change (e.g., `Vec<String>`). Consider logging via `status_msg` or adding a fallback for array-shaped labels.

## What's Good

1. **Frame I/O is safe.** `read_frame_ex` (dust_serve.rs:121) uses `read_exact` for both the 4-byte length prefix and the payload, treats `UnexpectedEof` as a clean close, and caps frame size at `MAX_FRAME_SIZE` (1 MiB). No partial reads, no OOM on malformed length, no spin on zero-length frames.
2. **Heartbeat handling is preserved after the refactor.** `run_lifecycle` keeps `last_registry_hb`, `miss_deadline = interval * HEARTBEAT_MISS_COUNT`, proactive `HeartbeatEnvelope` on timeout, and returns cleanly if the registry goes silent.
3. **Handshake is preserved where it matters.** The registry's new `connect_and_subscribe` is pure in-process bookkeeping on the existing broadcast channel — it does NOT open a second unix socket, so the tracker's `LIFECYCLE_DONE` heuristic is not fighting it. Subscribe/unsubscribe flow over the same lifecycle stream that completed the ready/host_info handshake.
4. **`biased` select in the lifecycle loop** (dust_serve.rs:643) prioritizes the inbound frame path, preventing mutation broadcasts from starving heartbeat or request handling.
5. **`connect_and_subscribe` error path is correct** — `plugins/dust/dust-registry/src/lib.rs:774-776` closes the subscriber connection slot on subscribe failure, preventing slot leaks under backpressure (`-33005 Busy`).
6. **State machine is closed.** Every `AppState` has an Esc path that either pops a level (`detail_open` → closed → `SelectedCapability` → query cleared → quit) or returns to `UILoaded` (palettes, ActionDispatched). The `unreachable!()` arm in `handle_key` is genuinely unreachable thanks to the palette early-return.
7. **`render_with_selection` scoping is correct.** `component_renderer.rs:50-62` increments `table_seen` and only passes `Some(row)` to the first Table, matching the Table the cursor indexes into. A plugin returning multiple tables won't have its secondary tables spuriously highlighted.
8. **Plugin-id conformance test** (`plugins/tracker/tests/plugin_id.rs`) is the right shape: `include_str!("../plugin.json")` + serde_json parse + equality against `PLUGIN_ID` — mutating either side produces a descriptive failure message.

## Methodology trace

1. Read `git log` for all files in scope; reviewed the three implementer diffs against `8136780b` (protocol freeze baseline).
2. Full read of `app.rs` (662 LOC), `component_renderer.rs` (357 LOC), `ui.rs` (601 LOC), `main.rs` (111 LOC).
3. Diff read of `dust_serve.rs` (+~450 LOC) and registry (`connect_and_subscribe`, `disconnect_subscriber`, `SubscriptionHandle`).
4. Confirmed `ipc_call` opens a fresh socket per call (`plugins/dust/dust-registry/src/lib.rs:1783`) — matches the tracker's non-lifecycle IPC branch expecting a single request + response.
5. Confirmed `open_subscriber_connection` / `subscribe_plugin_events` do NOT hit the socket — they mutate in-process subscriber state on the existing broadcast channel.
6. Confirmed the cancel-handler simplification is consistent with synchronous action dispatch (no `pending_actions` → every cancel legitimately observes `already_complete: true`), not a regression.

## Sign-off

**No blockers — APPROVE with warnings above for follow-up.**

The detail-view / palette / live-event / rich-widget stack is ready to ship. W-1 (cursor clamp) is the only finding likely to produce a visible UX glitch and should be fixed before heavy dogfooding. W-2 through W-5 are defensive polish.
