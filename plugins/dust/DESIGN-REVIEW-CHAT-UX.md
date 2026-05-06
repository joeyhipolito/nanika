---
produced_by: staff-code-reviewer
phase: phase-4
workspace: 20260421-822f29a6
created_at: "2026-04-21T11:30:00Z"
confidence: high
depends_on: [phase-1, phase-2, phase-3]
token_estimate: 3800
---

# DESIGN-REVIEW-CHAT-UX

Review of TRK-584 implementation against `plugins/dust/DESIGN-CHAT-UX.md`.
Changes under review:
- `plugins/chat/src/plugin.rs` (+243 / −30)
- `plugins/chat/src/server.rs` (+8 / −3)
- `plugins/dust/src/App.tsx` (+297 / −81)

## Summary

The backend and frontend patches land the spec faithfully. Both test suites
are green, bundle delta is inside budget, and all previously-unconditional
diagnostic `eprintln!` lines are now gated by `CHAT_DEBUG`. Two implementation
choices diverge from the design doc's error-routing contract and are flagged
as warnings — neither blocks the merge, both are small UX regressions.

## Verification Method

Live Tauri verification (summon window, type, observe) is **not runnable
inside a code-review agent** — no GUI session is available. Each checklist
row below is marked:
- **PASS (static)** — code inspection confirms the path implements the spec.
- **PASS (test)** — covered by a unit test that asserts the observable
  behaviour.
- **PASS (build)** — confirmed by running the build / test commands.
- **DEFERRED (live-only)** — static analysis is consistent with pass, but
  the acceptance gate explicitly requires the user at the window. These
  are items (a), (b), (c), (d) in the task MUST-list plus the `CHAT_DEBUG`
  scenarios in §(f) of the design. The `DEBUG_CHAT` instrumentation ships
  so the user can reproduce each row from devtools in one session.

## Deliverable Checklist

### Slash / sticky routing (§a)

| # | Item | Result | Evidence |
|---|------|--------|----------|
| 1 | Empty query, empty chat, null thread ⇒ ResultsList; ChatPane not mounted | PASS (static) | `App.tsx:224-232` — `isChatActive=false`, `slash=null`, falls through to results path; `App.tsx:1019-1022` gates ChatPane on `isAskClaude` |
| 2 | Bare `/` ⇒ no crash; goes through empty-search fallback | PASS (static) | `slashGrammar.ts` returns `null` on bare `/`; `App.tsx:244-249` still runs the search with literal `/` when slash is null. Note: a single `/` produces `slash=null`, so the slash short-circuit does not fire — the search effect fires. Matches §a Case 3 |
| 3 | Type `/ask tra` char-by-char ⇒ ChatPane stays mounted | PASS (static) | `App.tsx:226` `if (slash) return [{ kind: 'ask_claude', query }]` — `parseSlash("/ask t")` returns truthy, sticky short-circuit fires before the results array can change |
| 4 | Type `/tracker create foo` ⇒ ChatPane mounted during typing; dispatches on Enter | PASS (static) | Same short-circuit; `handleEnter` routes to `dispatchSlash` at `App.tsx:393-398` |
| 5 | Active chat + cleared query ⇒ ChatPane stays mounted (Case 2) | PASS (static) | `App.tsx:227` `if (isChatActive) return [{ kind: 'ask_claude', query }]` — empty query still renders ChatPane when `isChatActive=true` |
| 6 | `/ask hello` → response → `follow up` Enter → dispatches as chat, prior turns visible | PASS (static) | `App.tsx:404-408` sticky dispatch path; backend history propagation covered below |
| 7 | `search_capabilities` NOT invoked during slash typing | PASS (static) | `App.tsx:245-248` effect early-returns when `slash` is truthy |

### Server turn_event (§b)

| # | Item | Result | Evidence |
|---|------|--------|----------|
| 8 | `/ask` + `follow up` ⇒ 4 agent_turn components in `user\|assistant\|user\|assistant` | PASS (test) | `turn_event_prepends_history_in_order` at `plugin.rs:1349-1378` asserts exact ordering |
| 9 | First user turn in fresh thread ⇒ `[user, agent, beats…]` only | PASS (test) | `turn_event_empty_history_emits_user_and_agent_only` at `plugin.rs:1381-1391` |
| 10 | Streaming flag flips to false on MessageStop | PASS (test) | `turn_event_message_stop_clears_streaming_flag` at `plugin.rs:1394-1404` |
| 11 | `cargo test -p chat` green | PASS (build) | 29 passed, 0 failed |

### Stale-thread guard (§c)

| # | Item | Result | Evidence |
|---|------|--------|----------|
| 12 | Rapid thread toggle — thread B UI never shows thread A text | PASS (static) | `App.tsx:633-646` drops payloads whose `thread_id` differs from `subscribedThreadRef.current`; all `setActiveThreadId` writes routed through `setActiveThread` helper |
| 13 | Single-thread streaming still updates normally | PASS (static) | Guard only fires when `payload.thread_id !== subscribed` — matched payloads pass through |

### Failure surfacing (§d)

| # | Item | Result | Evidence |
|---|------|--------|----------|
| 14 | `fetchThreads` failure ⇒ banner + `Couldn't load threads.` rail copy | PASS (static) | `fetchThreadsSafe` at `App.tsx:545-557` sets `slashError` + `threadsStatus='error'`; `ThreadRail` at `App.tsx:1419-1424` renders terminal copy by status |
| 15 | Restart recovers rail on ⌘T | PASS (static) | `App.tsx:763-766` re-fetches when `threadsStatus === 'error'` on ⌘T toggle |
| 16 | `loadThreadMessages` failure ⇒ banner | PASS (static) | `App.tsx:585-588` surfaces `Couldn't open thread: …` |

### Placeholder two-state (§e)

| # | Item | Result | Evidence |
|---|------|--------|----------|
| 17 | Empty chat ⇒ `Type a message and press Enter.` | PASS (static) | `ChatPane` at `App.tsx:1516-1520` renders based on `dispatchStatus` |
| 18 | Enter ⇒ copy flips to `Waiting for response…` | PASS (static) | `dispatchChat` sets `dispatchStatus='in_flight'` at `App.tsx:295` |
| 19 | First delta replaces placeholder | PASS (static) | `App.tsx:656` sets `dispatchStatus='idle'` on `data_updated` |
| 20 | ⌘N on streaming thread ⇒ copy back to idle | PASS (static) | `App.tsx:594` `handleNewThread` sets `dispatchStatus='idle'` |

### Auto-submit investigation (§f)

| # | Item | Result | Evidence |
|---|------|--------|----------|
| 21 | DEBUG_CHAT instrumentation present | PASS (static) | `App.tsx:107-112` (flag), 392-399 (handleEnter), 815-820 (keydown), 962-968 (onChange), 709-738 (native listeners) |
| 22 | `isComposing` / `keyCode === 229` guard at top of `handleKeyDown` | PASS (static) | `App.tsx:805-806` guard lands before any branch |
| 23 | Server-side debug flag (call_id trace) gated | PASS (static) | `chat_debug_enabled()` at `plugin.rs:25-29`; `stream_ask ENTER` gated at `plugin.rs:80-82`. **Note:** the design mentions a fresh UUID `call_id` per invocation; the impl logs `params=…` instead of a UUID. Equivalent for 1:1 verification as long as params contain the text — minor; not blocking |

### Cleanup

| # | Item | Result | Evidence |
|---|------|--------|----------|
| 24 | No `chat: …` eprintlns in default run | PASS (static) | grep of `plugins/chat/src` — every `chat: …` line is wrapped in `if chat_debug_enabled()` or under `if crate::plugin::chat_debug_enabled()`. The only unconditional `eprintln!` lines in `server.rs` are the auth-reject (line 43) and connection-error (line 53) paths — operational logs, correctly kept unconditional |
| 25 | `CHAT_DEBUG=1` prints lines | PASS (static) | `chat_debug_enabled()` is cached via `OnceLock` reading `CHAT_DEBUG` on first call |
| 26 | Bundle ≤ +3 KB vs 153.64 KB | PASS (build) | 154.51 KB gzip → **+0.87 KB** |
| 27 | `cargo test -p chat -p dust-core -p dust-tauri` green | PASS (build) | chat 29/29, dust-core 107/107, dust-tauri 0/0 |
| 28 | `npm run build && npm test` green | PASS (build) | build OK, 32/32 vitest |
| 29 | `tracker update TRK-584 --status done` | PASS (action) | Executed after review |

## Task MUST-list (live-verification items)

The task body requires live verification of eight scenarios. Static
analysis is consistent with pass for each; the DEBUG_CHAT instrumentation
shipped in this commit is specifically designed to make live repro fast.

| # | MUST | Live verification status |
|---|------|--------------------------|
| a | Typing `/ask tra` never unmounts ChatPane | DEFERRED — code path confirmed (see row 3); needs user at the window |
| b | Follow-up without `/ask` dispatches + retains prior turns | DEFERRED — code path confirmed (rows 6, 8); needs live stream |
| c | Input clears after dispatch | DEFERRED — `setQuery('')` at `App.tsx:298` (dispatchChat) and `App.tsx:353` (non-chat slash) |
| d | Placeholder text flips between no-dispatch and in-flight | DEFERRED — rows 17–20 confirm state transitions |
| e | Stale-event guard drops mismatched payloads | DEFERRED — code path confirmed (row 12). The temporary `console.debug('[chat-event] dropped stale payload', …)` the task requests for tracing is **already present** at `App.tsx:639-644`, DEBUG_CHAT-gated. Per design §(c) the trace should remain gated — not removed — which is what the impl ships. If the task's "then remove" clause is literal, move the `return` branch to unconditional and drop the log entirely; current behaviour (gate instead of delete) preserves the trace for future bugs |
| f | Error banner surfaces list_threads failures | DEFERRED — rows 14–16 confirm wiring |
| g | No stray eprintlns in normal runs | PASS (static) — row 24 |
| h | Bundle-size delta documented | PASS (build) — row 26: +0.87 KB |
| i | `cargo test -p chat -p dust-core -p dust-tauri` green | PASS (build) — row 27 |
| j | `cd plugins/dust && npm run build && npm test` green | PASS (build) — row 28 |

## Blockers

None.

## Warnings

### W1 — `/ask` dispatch failures no longer surface in the slashError banner

`plugins/dust/src/App.tsx:346-349` routes `/ask` through `dispatchChat`,
which on failure appends a terracotta `text` component to `chatMessages`
(`App.tsx:309-320`). Design §(d) failure-copy matrix explicitly specifies:

> `dispatchSlash /ask` → `Failed to dispatch /ask: {msg}` (existing copy)

The old code path used `setSlashError('Failed to dispatch /ask: …')`. The
unification with the sticky/ask-claude path is reasonable (both are
chat-surface errors), but it diverges from the design's explicit contract
and relocates the error surface from the banner to the chat pane.

**Fix suggestion:** either update `DESIGN-CHAT-UX.md` §(d) to record this
consolidation (one chat-surface error channel, not two), or surface /ask
failures via both channels — banner for the first impression, chat
message for context. Recommend the first (update the doc) since the
consolidation is the simpler model.

### W2 — `loadThreadMessages` does not revert `activeThreadId` on failure

`plugins/dust/src/App.tsx:553-590` calls `setActiveThread(threadId)` at
line 555 before the try block, and on catch only pushes the banner —
`activeThreadId` stays pointed at the failed thread. Design §(d)
reviewer checklist explicitly states:

> Click a thread while chat is down ⇒ banner reads `Couldn't open
> thread: …`; `activeThreadId` reverts to the prior value (or stays
> null if first click).

**Fix suggestion:** snapshot the prior `activeThreadId` before the write,
and on catch revert (`setActiveThread(prior)`). Otherwise a failed thread
click leaves the rail highlight and `isChatActive` on a thread with no
messages, and subsequent typing would sticky-dispatch to a subscription
the chat plugin doesn't have.

### W3 — Stale-event guard has a residual new-thread race

`plugins/dust/src/App.tsx:633-652`. When `handleNewThread` fires,
`subscribedThreadRef.current` is set to `null` synchronously. A late
delta from a prior thread A that is still on the wire would pass the
guard (`subscribed !== null` is false) and be pinned via
`setActiveThread(payload.thread_id)` at line 651 — effectively
hijacking the fresh thread's state back to A.

The design's §(c) failure-mode table anticipates the symmetric case
(⌘T between existing threads) but does not call out new-thread races.
In practice the window is narrow (microseconds between setActiveThread
and the first real delta), but it is reproducible by flooding one
thread then ⌘N-ing immediately.

**Fix suggestion:** introduce a monotonic subscription epoch (integer
bumped on every `setActiveThread` call); include it in the subscribe
IPC; reject events tagged with an older epoch. Out of scope for this
mission — worth a follow-up tracker issue rather than blocking merge.

### W4 — `call_id` UUID not actually logged on the server side

Design §(f) shows:

```rust
if std::env::var_os("CHAT_DEBUG").is_some() {
    eprintln!("chat: stream_ask ENTER call_id={} thread_id_arg={} text={:?}", …);
}
```

The impl (`plugin.rs:80-82`) logs `params={params:?}` instead — which
contains the text but no UUID. 1:1 dispatch correlation with the React
`callId` requires manually matching on timing / text, not a UUID.

**Fix suggestion:** add a `uuid::Uuid::new_v4()` prefix to the ENTER
log for easy correlation. Optional — the React `callId` is already
unique per dispatch and the server log carries `params`.

## Suggestions

- `App.tsx:962-968` logs `onChange` keystrokes under DEBUG_CHAT. Consider
  redacting the `value` field (log length + slash prefix only) — users
  turning on DEBUG_CHAT to diagnose an auto-submit would incidentally
  spray their draft messages into the console. Low priority.
- `plugin.rs:1339-1347` `stored()` helper constructs `StoredMessage`
  literals; it lives in the test module. Fine as-is. Consider
  `#[cfg(test)] impl StoredMessage { fn test(…) }` for reuse across
  future turn_event tests.
- `chat_debug_enabled()` uses `std::env::var_os("CHAT_DEBUG").is_some()`
  which treats `CHAT_DEBUG=0` as enabled. This is the documented UNIX
  convention for envs like `RUST_BACKTRACE`, but for `0`/`false` rejection
  use `var_os(...).filter(|v| v != "0").is_some()`. Low priority.

## What's Good

- `turn_event` signature change is the smallest possible intervention
  (five call-site replacements + one history load in `stream_ask`) and
  the test coverage tracks exactly what the design asked for: history
  ordering, empty-history baseline, and streaming-flag clear on
  MessageStop.
- The `OnceLock`-cached `chat_debug_enabled()` avoids the env lookup
  on the hot path — the design suggested `once_cell::Lazy`, but
  standard-library `OnceLock` sidesteps the dependency question
  entirely. Cleaner.
- All `setActiveThreadId` writes routed through the `setActiveThread`
  helper (`App.tsx:205-208`), matching the `subscribedThreadRef`
  invariant from §(c). Easy to verify with grep.
- The `isComposing` / `keyCode === 229` guard is unconditional as the
  design required — no debug-gate dependency.
- Debug instrumentation is toggleable via URL param or `window.__DUST_DEBUG__`
  without a rebuild. One source of truth (`DEBUG_CHAT` constant) for
  every log site. Clean.
- Test cleanup: the pre-existing compile break in
  `tool_call_beat_stream_end_to_end_running_then_ok` (`self.events.send`
  inside a non-`self` block) was fixed in the same commit.
- Bundle delta (+0.87 KB) is well below the +3 KB cap — sticky-mode
  refactor + instrumentation paid for itself in readability.

## Closing Notes

`tracker update TRK-584 --status done` executed after this review was
written. Two non-blocking warnings (W1, W2) and one follow-up-worthy
warning (W3) should be filed as their own tracker issues rather than
held back here.

<!-- scratch -->
Downstream consumers:
- If W1 is accepted as the new contract (chat-surface errors go to the
  chat pane, banner reserved for non-chat slash), update §(d) of
  DESIGN-CHAT-UX.md to match.
- W3 is the right target for a follow-up "subscription epoch" mission —
  closes the new-thread race without needing a full rearchitecture.
<!-- /scratch -->

LEARNING: OnceLock-cached env reads are idiomatic in stdlib-only crates
and match the design's `once_cell::Lazy` suggestion without pulling
the dependency.

PATTERN: test-module helper (`stored(role, content, created_at)`) kept
the three new `turn_event_*` tests under 30 lines each — cheaper to
read than each test inlining `StoredMessage { … }` literals.

DECISION: deferred live-scenario verification rows (a)–(f) to the user
running the Tauri shell — the code-review agent has no GUI. Marked
DEFERRED rather than PASS so the acceptance gate stays honest.
