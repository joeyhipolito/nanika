---
produced_by: staff-code-reviewer
phase: phase-3
workspace: 20260421-70b515e6
created_at: "2026-04-21T09:32:00Z"
confidence: high
depends_on:
  - phase-2
token_estimate: 1600
---

# Thread Rail — Code Review

## Summary

The ThreadRail feature adds a 180 px thread-history column toggleable via `⌘T` in chat mode, re-uses the chat plugin's subscription lifecycle for thread switching, and extends `dispatch_action` on the Tauri side to forward the full `ActionResult` envelope (`{success, message?, data?}`) so `list_threads` / `list_messages` / `new_thread` can read response payloads. All five acceptance areas pass. Two low-severity warnings and two suggestions are captured below. One acknowledgment: the ordering in `loadThreadMessages` — clear → setActive → subscribe → read — is exactly right and the comment on lines 457–459 spells out the invariant so a future maintainer can't accidentally reorder it.

## Area Checklist

| Area | Verdict | Evidence |
| --- | --- | --- |
| (a) Layout: rail ⊥ results column; `⌘T` only in chat | PASS | `plugins/dust/src/App.tsx:787` + `:794` + `:617`; `railActive = isAskClaude && threadsVisible`; `isAskClaude` gates ResultsList; global `⌘T` early-returns on `!isAskClaudeRef.current` at `:580`; flip-out effect at `:220-225` closes rail when chat deactivates. |
| (b) Keybinding hygiene | PASS | `⌘N` `:572`, `⌘T` `:577`, `⌘E` `:590`, Esc global `:603`; local `ctrlKey+K` palette `:620`; meta-K entry is commented out `:598-602` — no collision. |
| (c) Thread-switch correctness | PASS | `loadThreadMessages` `:456-483` clears `chatMessages` synchronously before any `await`, sets `activeThreadId`, re-subscribes, then `list_messages`; listener `:518` updates `activeThreadId` on every payload. |
| (d) Debounce ≤ 1 refresh / 500 ms | PASS | `refreshTimerRef` trailing-edge 500 ms at `:522-528`; self-gated by `!threadsRef.current.some(t => t.id === newTid)` so known-thread streams never schedule a refresh; cleared on unmount `:555-558`. |
| (e) Non-regression | PASS | `cargo test -p chat` 26/26; `cd plugins/dust/dust-dashboard && cargo test` 14/14; `npm run build` clean; `npm test` 32/32. Main JS chunk gzipped `152.41 → 153.64 KB` (+1.23 KB, budget +5 KB). |

## Blockers

_None._

## Warnings

### WARNING: stale chat-event can flip `activeThreadId` back to an old thread

`plugins/dust/src/App.tsx:518` unconditionally does `if (payload.thread_id) setActiveThreadId(payload.thread_id)`. `chat_subscribe` in `plugins/dust/src-tauri/src/lib.rs:220-281` opens the new stream before aborting the old forward task, so for a brief window both tasks can emit to the same `dust://chat-event` channel. A `data_updated` from the old subscription still in the JS event-loop queue can overwrite the just-set `activeThreadId`. It is self-correcting on the next fresh event, but until that happens the rail selection highlight and next `/ask` dispatch use the wrong thread.

**Fix suggestion:** In the listener, reject payloads whose `thread_id` differs from `activeThreadId` when we just initiated a switch — e.g. keep a `switchingRef` set by `loadThreadMessages` for ~one frame, or compare `payload.thread_id` against a ref holding the subscribed thread. Tight scope; the subscription side already has the zero-loss replace invariant so the JS side is the only place this can leak.

### WARNING: `fetchThreads` / `loadThreadMessages` failures leave the rail silently broken

`App.tsx:584` (`⌘T` path) and `:643` (Enter-on-row) both swallow errors into `console.error`. If `dispatch_action list_threads` fails, the rail renders "Loading…" forever (`:1205-1207`). If `list_messages` fails, the user sees an empty chat pane with no feedback.

**Fix suggestion:** Promote these to the existing `slashError`-style terracotta banner, or add an error state to `ThreadRail` props (`error: string | null`). Reuses the pattern already in `dispatchSlash` (`:258`, `:277`, `:299`).

## Suggestions

### SUGGESTION: clicking `+` (new thread) should close the rail

`App.tsx:804-807` calls `handleNewThread()` then `setThreadCursor(0)` but leaves the rail open. Keyboard flow already closes the rail on Enter-on-row (`:642`); the click path should match for symmetry and so the newly-opened empty chat pane isn't hidden behind a 180 px rail.

### SUGGESTION: document the mirror-ref pattern for `isAskClaudeRef`

`App.tsx:184` + `:220-225` + `:580`: the ref exists because the global `keydown` effect's deps are `[transitionTo, handleNewThread, fetchThreads, applyThreads]` — adding `isAskClaude` would re-register the listener on every search selection change. The inline comment at `:183-184` hints at it but the rationale ("avoid resubscribing the global shortcut on every arrow-key press") is worth one more line so a future maintainer doesn't "simplify" it by adding `isAskClaude` to deps.

## What's Good

- `loadThreadMessages` `:456-483` gets the ordering exactly right (clear → setActive → subscribe → read) and the comment on lines 457-459 explicitly names the `handleEnter` invariant being reused. This is the single most fragile piece of the feature; the author recognised it and wrote the comment that makes it maintainable.
- The `unwrapData` shim `:127-134` is a clean migration path — old call sites that ignore the return value are unaffected, data-bearing callers opt in. This is the right shape for the Rust contract change at `lib.rs:187-196`.
- `applyThreads` `:441-444` keeps `threads` state and `threadsRef` in lockstep and the comment `:437-440` documents *why* (load-bearing for the unknown-thread-id debounce gate). Good defensive commenting.
- Bundle growth (+1.23 KB gzipped) stayed well under the +5 KB budget. Pure-JS `agoLabel` (avoiding `Intl.RelativeTimeFormat`) is the right call at this size budget.

## Verification Log

```
cd plugins/chat            && cargo test            → 26 passed; 0 failed
cd plugins/dust/dust-dashboard && cargo test        → 14 passed; 0 failed
cd plugins/dust            && npm run build         → clean; main chunk 153.64 KB gzipped (+1.23 KB)
cd plugins/dust            && npm test              → 32 passed; 0 failed
grep -R currentThreadId plugins/dust/src            → no matches (rename complete)
```

<!-- scratch -->
No implementation handoff — all findings are non-blocking. The two warnings are worth a follow-up ticket but don't gate this phase; the suggestions are nice-to-have polish. Tracker updated to TRK-569=done on approval of this review.
<!-- /scratch -->
