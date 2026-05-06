---
produced_by: staff-code-reviewer
phase: phase-3
workspace: 20260421-ab4b193b
created_at: "2026-04-21T03:30:00Z"
confidence: high
depends_on:
  - phase-2
token_estimate: 4200
---

# DESIGN-REVIEW-CHAT-UX-2

Live review of `plugins/dust/src/App.tsx` against the 27-row checklist in
`plugins/dust/DESIGN-CHAT-UX-2.md §(e)` and the task MUST list for TRK-600.

## Summary

Phase 2 implements all three deliverables from DESIGN-CHAT-UX-2.md —
exit-chat gesture (`exitChat` helper wired into both Esc handlers),
optimistic user-turn echo with provisional-id rollback, and
`SearchBar` mode discriminator for chat-composer affordance.
Cargo tests for `chat` (29) and `dust-core` (107 + 1 doc) are green;
`dust-tauri` has no tests (0 pass). Vitest 32/32 passed.
Gzipped main bundle: **154.82 KB**, delta **+0.31 KB** vs 154.51 KB
baseline — well inside the +3 KB cap.

**Overall assessment: one BLOCKER, two WARNINGS, one SUGGESTION.**
The implementation is tight, matches the design doc's intent on 23 of 27
checklist rows, and keeps the existing TRK-584 invariants intact. The
blocker is an *unintended regression of the exit gesture* caused by the
design spec's inaccurate claim about the existing stale-event guard —
the implementer faithfully followed the spec, but the guard does not
actually drop post-exit late events the way the design assumes.

## Blockers

### B1. Late `data_updated` after `exitChat` re-opens the chat pane

**File:** `plugins/dust/src/App.tsx:669-745` (listener) combined with
`plugins/dust/src/App.tsx:627-637` (`exitChat`).

**What breaks:** After `exitChat()` runs, `subscribedThreadRef.current`
and `activeThreadId` are both `null`. The rust-side `chat_subscribe`
task is intentionally left alive (design §(a) "Subscription lifetime").
When the next `data_updated` event arrives for the thread the user just
exited, the stale-thread guard at `App.tsx:682-694` does NOT drop it
because its condition requires **both** `subscribed !== null` and
`payload.thread_id !== subscribed`:

```ts
if (
  payload.thread_id !== null &&
  subscribed !== null &&                   // ← falsy post-exit
  payload.thread_id !== subscribed
) { return }
```

Execution falls through to the pinning branch at `App.tsx:698-700`,
which *re-pins* `setActiveThread(payload.thread_id)`, then the
`data_updated` branch at `App.tsx:702-703` calls
`setChatMessages(payload.data)`. Net effect: `isChatActive` flips back
to `true`, `ChatPane` re-mounts, and the user's Esc is silently
reverted on the very next stream tick.

**Reproduction (checklist row 5 + row 11):**
1. `/ask write a 500-word essay`, Enter.
2. Within ~200 ms, press `Esc`.
3. *Expected (design):* `ChatPane` unmounts; user sees empty-state
   launcher surface for 10 s until `⌘T`.
4. *Actual:* `ChatPane` unmounts briefly, then re-mounts within the
   next server tick (≤ a few hundred ms during active streaming) with
   history + the current partial agent turn. Subsequent `data_updated`
   events continue to re-populate.

**Why the design spec is misleading here:** `DESIGN-CHAT-UX-2.md:152-157`
claims *"the guard at `App.tsx:~635` already drops events whose
`thread_id !== null && subscribed === null`."* The guard as written
does NOT drop that case — it has the opposite asymmetry. This is a
latent defect in the spec that the implementer should have surfaced
before shipping, but the root cause is real code behavior.

**Impact on checklist:**
- Row 5 fails (stream response in UI after exit).
- Row 11 fails if any in-flight event arrives before the user has a
  chance to observe `chatMessages === []`.
- Row 8 fails on the final `Esc` press if chat was mid-stream (the
  exit-step appears to work but immediately unwinds).

**Suggested fix (pick one):**

*Option 1 — extend the guard (recommended, ~3 lines):*

```ts
// At the top of the listener, after reading `subscribed`:
if (payload.thread_id !== null && subscribed === null
    && !newThreadPendingRef.current) {
  return    // late event after exitChat — drop it
}
```

This needs a new `newThreadPendingRef` that `dispatchChat` /
`handleNewThread` set to `true` before their `invoke('chat_subscribe',
{threadId: null})` call and the listener clears after the first pin.
Preserves the "pin on first delta" semantic for new-thread dispatches
without re-opening after exit.

*Option 2 — call `chat_unsubscribe` on exit (simpler, ~2 lines):*

Revise `exitChat` to add `invoke('chat_unsubscribe').catch(...)`. Design
§(a) argued against this to preserve the server-side response payload,
but the alternative is worse UX (exit doesn't stick). The blur-handler
path already does this — applying it to exit is symmetric.

Either fix is < 5 lines and bundle-neutral.

## Warnings

### W1. Placeholder copy differs from DESIGN-CHAT-UX-2.md §(c) spec

**File:** `plugins/dust/src/App.tsx:1186`.

Design doc §(c) explicitly specifies `placeholder: "Message the thread…"`
and checklist row 16/17 uses that string as the pass signal. The
implementation uses `"Message Claude…"`. The TRK-600 ticket description
uses `"Message Claude…"` too, so the implementer had a conflicting
source.

**Recommendation:** Pick one string and make the design doc, TRK
description, and code agree. If "Message Claude…" is the preferred
copy, update `DESIGN-CHAT-UX-2.md §(c)` and checklist rows 16/17 to
match. Otherwise, change the code string. Current state leaves future
reviewers chasing which source won.

### W2. `ShortcutHint` label omits the preposition specified by the MUST list

**File:** `plugins/dust/src/App.tsx:1772`.

Task MUST (e) requires the visible text to read `"esc to leave chat"`.
The rendered DOM is `<kbd>esc</kbd><span>leave chat</span>`, which
displays as `esc leave chat` — missing the "to". Design doc itself
doesn't mandate exact wording, but the MUST list does.

**Fix:** Change `label="leave chat"` to `label="to leave chat"` (or
introduce a two-word label render; a one-char edit is sufficient).

## Suggestions

### S1. `/ask` with empty body dispatches a provisional echo with empty content

**File:** `plugins/dust/src/App.tsx:377-380`.

```ts
if (cmd.prefix === 'ask') {
  dispatchChat(cmd.args)     // cmd.args may be "" for bare "/ask"
  return
}
```

`dispatchChat` has no empty-text guard (design §(b) "Empty-text guard"
asserts the upstream callers guard, which is true for the sticky path
and the ask-claude selection path but not for the slash `/ask` path).
Bare `/ask` + Enter pushes a provisional with `content: ""`, then
invokes `dispatch_action` with an empty text. Backend likely no-ops or
errors but the provisional visibly appears.

**Fix:** Add an early return in `dispatchChat`:

```ts
if (text.trim() === '') return
```

Cheap defensive guard; aligns with the invariant stated in design §(b)
"Empty-text guard".

## Checklist Result (per DESIGN-CHAT-UX-2.md §(e))

### Exit-chat (§a)

| # | Row                                              | Result  | Line refs / notes                                     |
|---|--------------------------------------------------|---------|-------------------------------------------------------|
| 1 | Esc exits chat when no modal/detail open         | PASS*   | `App.tsx:945-946` (component) + `App.tsx:847-850` (global). Provisional caveat: immediate post-Esc event re-opens (see B1). |
| 2 | Esc does NOT exit chat when detail loaded        | PASS    | `App.tsx:940-944` — detail-close branches first.     |
| 3 | Esc does NOT exit chat when palette open         | PASS    | `App.tsx:938-939` — palette-close branches first.    |
| 4 | Exit clears all documented slices                | PASS    | `App.tsx:627-637` — 9 slices match design table.     |
| 5 | Exit does NOT cancel in-flight stream            | **FAIL**| See **B1** — stream continues AND re-pops UI via listener pinning. |
| 6 | Re-entering via ⌘T restores messages from store  | PASS    | `App.tsx:587-620` (`loadThreadMessages`) — independent of exit path. |
| 7 | Rail closes on exit                              | PASS    | `App.tsx:630` — `setThreadsVisible(false)`.          |
| 8 | Step-back order preserved                        | PASS*   | `App.tsx:938-950` — palette → detail → exitChat → hide. 4th press hides. *Caveat ties to B1 under streaming. |

### Optimistic echo (§b)

| # | Row                                              | Result  | Line refs / notes                                     |
|---|--------------------------------------------------|---------|-------------------------------------------------------|
| 9 | Provisional appears synchronously on Enter       | PASS    | `App.tsx:310-320` — synchronous setChatMessages before invoke chain. |
| 10| Provisional replaced by server turn without flicker | PASS | `App.tsx:702-709` — wholesale replace + ref cleared. Shape matches `plugin.rs::user_turn_component`. |
| 11| Exit during in-flight drops provisional cleanly  | PARTIAL | `App.tsx:633` clears `provisionalUserIdRef`; `App.tsx:628` sets `chatMessages=[]`. But late events re-populate per **B1**. |
| 12| Failure on dispatch removes provisional via slashError | PASS | `App.tsx:337-351` — filter by `provisional_id`, `setSlashError('Failed to dispatch /ask: …')`, no terracotta append to `chatMessages`. |
| 13| Provisional shape matches server user-turn shape | PASS    | `App.tsx:312-319` vs `plugins/chat/src/plugin.rs:641-648` — `type/role/content/timestamp` match; `streaming: false` explicit; only extra field is `provisional_id`. |
| 14| Empty query dispatches no provisional            | PARTIAL | Sticky path (`App.tsx:441`) and ask-claude path (`App.tsx:453`) guard. Slash `/ask` path (`App.tsx:377-380`) does not — see **S1**. |
| 15| IME commit Enter does not double-echo            | PASS    | `App.tsx:867-868` guard (`isComposing` + `keyCode === 229`) short-circuits before `dispatchChat`. |

### Composer mode (§c)

| # | Row                                              | Result  | Line refs / notes                                     |
|---|--------------------------------------------------|---------|-------------------------------------------------------|
| 16| Placeholder swaps on chat entry                  | PASS†   | `App.tsx:1186` — swaps on `mode`. Copy `"Message Claude…"` ≠ design's `"Message the thread…"` — see **W1**. |
| 17| Placeholder swaps on chat exit                   | PASS†   | Same; reverts when `isChatActive` flips false. Same copy caveat. |
| 18| Icon swaps in sync with placeholder              | PASS    | `App.tsx:1162-1174` — single `mode` ternary governs both icon and placeholder, atomic render. |
| 19| Slash still dispatches from chat mode            | PASS    | `App.tsx:429-434` — slash parse runs before sticky branch; composer mode irrelevant to routing. |
| 20| Non-slash Enter in chat mode dispatches chat     | PASS    | `App.tsx:441-445` — sticky path unchanged. |
| 21| Bundle size delta within budget                  | PASS    | Main bundle: 154.51 KB → **154.82 KB** = **+0.31 KB**. Cap +3 KB (task) / +2 KB (design). Under both. |

### Decision-table (§d)

| # | Row                                              | Result  | Line refs / notes                                     |
|---|--------------------------------------------------|---------|-------------------------------------------------------|
| 22| Rows 13 & 16 of §(d) spec'd as new shapes        | PASS    | `DESIGN-CHAT-UX-2.md:566-579` — chat-exiting (13) and dispatch-failed (16) rows present with full column coverage. |
| 23| Rows 10–12 & 14–15 clarify composer mode         | PASS    | `DESIGN-CHAT-UX-2.md:566-579` — each has concrete `composer mode: chat`; row 12 documents cross-mode `/tracker`. |

### Regression (§e)

| # | Row                                              | Result  | Line refs / notes                                     |
|---|--------------------------------------------------|---------|-------------------------------------------------------|
| 24| TRK-584 checklist rows 1–13, 17–20 still pass    | PASS    | Static inspection: sticky predicate (`App.tsx:226`), stale-thread guard (`App.tsx:682-694`), two-state placeholder (`App.tsx:1595-1600`), FileRef chip (`App.tsx:1023-1040`) all intact. |
| 25| `cargo test -p chat -p dust-core -p dust-tauri` green | PASS | chat: 29 passed; dust-core: 107 + 1 doc passed; dust-tauri: 0 tests (no failures). Full log in worker stdout. |
| 26| `cd plugins/dust && npm run build && npm test` green | PASS | Build OK (154.82 KB gzipped main); vitest 32 passed across `ComponentRenderer.test.tsx` + `slashGrammar.test.ts`. |
| 27| `tracker update TRK-6 --status done` after verify | DEFERRED | Tracker for this mission is **TRK-600** (see ticket title "Chat UX polish 2"). Per task: `tracker update TRK-600 --status done` on success. Blocker B1 holds this open. |

† = functional PASS with cosmetic deviation documented in Warnings.

## Test Command Output Summary

```
# cargo test -p chat      → 29 passed; 0 failed
# cargo test -p dust-core → 107 passed + 1 doc; 0 failed
# cargo test -p dust-tauri → 0 tests; 0 failed
# cd plugins/dust && npm run build → 154.82 KB gzipped main; no errors
# cd plugins/dust && npm test → 32 passed; 0 failed
```

## Bundle Delta

| Metric              | Baseline (post-TRK-584) | Post-impl | Delta     |
|---------------------|-------------------------|-----------|-----------|
| Main JS (gzipped)   | 154.51 KB               | 154.82 KB | **+0.31 KB** |
| Cap (task MUST h)   | +3.00 KB                | —         | **10× under** |
| Cap (design §)      | +2.00 KB                | —         | **6× under** |

## What's Good

- **Provisional shape is bit-identical to `plugin.rs::user_turn_component`.**
  `App.tsx:312-319` vs `plugins/chat/src/plugin.rs:641-648` — field names, types,
  and `streaming: false` value all align. This makes the wholesale-replace
  reconciliation truly a no-op for the DOM, exactly as design §(b) planned.
- **Rollback via `provisional_id` filter is defensive and precise.**
  `App.tsx:343-348` — filters by exact id match, so unrelated turns (e.g., if
  some other dispatch raced in) survive. Cleaner than position-based removal.
- **Dual-handler Esc wiring is thoughtful.** Component-level handler
  (`App.tsx:933-951`) handles focus-on-input path; global listener
  (`App.tsx:847-850`) handles the other path with an `isChatActiveRef` guard
  to prevent double-exit. The comment at `App.tsx:843-846` correctly explains
  why both paths are needed.
- **`exitChat` is a true single-responsibility helper.** Nine state slices
  cleared in one function, exactly the table in design §(a). Single
  `setActiveThread(null)` entry point keeps the ref mirror honest.
- **Bundle discipline.** +0.31 KB for a three-deliverable UX pass is an
  excellent result — tight implementation of Option A (top-bar swap) vindicates
  the rejection of Option B (ChatPane-local input).

## Scratchpad

<!-- scratch -->
Follow-up work for the implementer before closing TRK-600:

1. Fix B1 (stale-event guard post-exit). Option 2 is the simpler path:
   call `invoke('chat_unsubscribe').catch(console.error)` inside `exitChat`
   (add line after `setSelectedIndex(0)` at App.tsx:636). This reverts the
   design's "keep subscription alive" choice — the design's rationale was
   "don't waste server tokens", but the alternative is that Esc silently
   unwinds. Server token cost of a cancelled stream is trivial; UX cost of
   Esc-not-sticking is severe. If we want to preserve the response on
   server disk, verify the chat plugin still persists what it has computed
   so far when `chat_unsubscribe` fires. If not, go with Option 1.

2. Fix W1 (placeholder copy). Two sources disagree. TRK-600 ticket says
   "Message Claude…"; DESIGN-CHAT-UX-2.md §(c) says "Message the thread…".
   Pick one, update the other + checklist rows 16/17 to match. Lowest-effort:
   change design doc to match code (code & ticket agree against design).

3. Fix W2 (ShortcutHint label). One-char edit at App.tsx:1772:
   `label="leave chat"` → `label="to leave chat"`.

4. Fix S1 (empty-text slash path). Add `if (text.trim() === '') return` at
   the top of `dispatchChat` (App.tsx:302). Guards the /ask with bare body
   case that slips past the upstream callers.

After fixes, re-run:
  - cargo test -p chat -p dust-core -p dust-tauri
  - cd plugins/dust && npm run build && npm test
  - Verify row 5 manually: /ask + long prompt, Esc within 200ms, stay in
    window, confirm ChatPane does NOT re-mount for 10s.
  - `tracker update TRK-600 --status done`
<!-- /scratch -->

LEARNING: Asymmetric guards that check `A !== null && B !== null` assume
symmetry of null-state, but null is often a distinct regime. When writing
guard conditions, enumerate all four quadrants (A null / B null ×) and
verify each explicitly. The post-exit-late-event case in this review was
a quadrant the design spec author missed by relying on prose rather than
walking the truth table.

FINDING: Design doc verbal claims about existing code behavior should be
treated as hypotheses, not facts. The `DESIGN-CHAT-UX-2.md:152-157` claim
*"the guard already drops these events"* was wrong, but the implementer
trusted it and built on top of the false premise. Spec authors should
cite code with line-refs AND tests when making behavioral claims; spec
reviewers should grep the referenced code to verify.

PATTERN: Optimistic echo that lives under a "wholesale replace on next
event" contract is cheap to implement — the provisional dies atomically
on the first real payload, no reconciliation needed. Provisional-id is
rollback-only bookkeeping, not a reconcile key. This is the right shape
for any server-authoritative stream with periodic snapshot events.
