---
produced_by: architect
phase: phase-1
workspace: 20260421-ab4b193b
created_at: "2026-04-21T03:09:05Z"
confidence: high
depends_on: []
token_estimate: 5600
---

# DESIGN-CHAT-UX-2

Follow-on spec to `plugins/dust/DESIGN-CHAT-UX.md` (TRK-584). That mission
shipped sticky-mode routing, history-prefix `turn_event`, the stale-thread
guard, the `fetchThreads` / `loadThreadMessages` banner, and the two-state
placeholder. Three residual UX gaps remain for TRK-6:

1. No **exit-chat** gesture — once `isChatActive` flips true, the only way
   out is `⌘N` (which opens a fresh thread, not "leave chat and go back to
   plugin search"). Esc today only steps through `ActionPalette → Detail
   → hide-window`; it does not touch chat state.
2. No **optimistic user-turn echo** — after Enter, the UI shows
   `Waiting for response…` for the `chat_subscribe + dispatch + first
   TextDelta` round-trip (measured ~180–420 ms on local models, multi-second
   on remote). The user can't see what they sent, which looks like the
   prompt was dropped.
3. **Composer mental model is wrong in chat.** The single top-bar input
   says `Search capabilities…` even when the active thread has four turns
   of conversation. Users report typing a message, looking at the bar, and
   re-checking they were in chat.

This doc is the spec consumed by `implement-frontend` for
`plugins/dust/src/App.tsx`. `implement-backend` is not invoked — all three
deliverables are frontend-only.

## Context

- Existing Esc step order (`App.tsx:871-887`):
  `ActionPalette → Detail → clear query + hideWindow`. Chat state
  (`chatMessages`, `activeThreadId`, `threadsVisible`, `dispatchStatus`)
  is never touched.
- Server `turn_event` user-turn component shape (inspected at
  `plugins/chat/src/plugin.rs:641-648`):

  ```rust
  Component::AgentTurn {
      role: "user".into(),
      content: text.to_string(),
      streaming: false,
      timestamp: Some(ts),
  }
  ```

  Serialized JSON (given `skip_serializing_if` on `streaming=false` per the
  TS-side type at `plugins/dust/src/types.ts:92-102` and the review note at
  `DESIGN-REVIEW-CHAT-UX.md:1366`):

  ```json
  {
    "type": "agent_turn",
    "role": "user",
    "content": "<verbatim user text>",
    "timestamp": <epoch ms>
  }
  ```

  `streaming` is omitted (false is elided). `content` is the verbatim
  user text passed through `stream_ask` — this is the string `dispatchChat`
  forwards unchanged.

- The chat listener replaces `chatMessages` wholesale on every
  `data_updated` payload (`App.tsx:~655`, single-slice contract from
  `DESIGN-CHAT-SUBSCRIPTION.md §b`).
- Current baseline bundle: **154.51 KB gzipped** (`DESIGN-REVIEW-CHAT-UX.md`
  row 26, `+0.87 KB` above the pre-TRK-584 `153.64 KB` reference).
- Cap for this mission: **+2 KB gzipped** vs 154.51 (i.e., ≤ 156.51 KB).

## Non-goals

- No backend changes. `turn_event` stays as-is; `stream_ask` stays as-is.
- No new Tauri commands. `chat_unsubscribe` already exists and is invoked on
  window blur (`App.tsx:482`); exit-chat reuses it.
- No change to subscription lifetime on exit — see decision in §(a).
- No thread-deletion flow. "Leaving" a chat keeps the thread on disk.

---

## (a) Exit-chat gesture — state-transition table

### Trigger

Esc pressed in `default` `windowMode`, when:
- Action palette is closed (`!showActionPalette`), AND
- No detail pane loaded (`detail === null`), AND
- Chat is active (`isChatActive === true`).

This slot sits **between** the existing "close detail" and "clear query +
hide window" branches of the Esc handler (`App.tsx:871-887`). The full
step-back order becomes:

```
ActionPalette open    →  close palette
Detail loaded         →  close detail (existing)
isChatActive          →  EXIT CHAT (new — this §)
otherwise             →  clear query + hideWindow (existing)
```

Rationale for ordering: Esc's user model is "step one ceremony back." Closing
a modal/detail is one step back from the chat surface; exiting chat is one
step back from a thread; hiding is one step back from the shell. Interleaving
exit-chat between detail-close and hide-window preserves that gradient.

### State-transition table

| Slice                     | Before exit          | After exit           | Why                                                                                              |
|---------------------------|----------------------|----------------------|--------------------------------------------------------------------------------------------------|
| `chatMessages`            | `Component[]` (N>0)  | `[]`                 | Close the visible thread. Persisted messages remain server-side via `store.thread_messages`.     |
| `activeThreadId`          | `"<tid>"`            | `null`               | Drops `isChatActive` to false (see derivation, `App.tsx:218`). This is the single load-bearing flip that unmounts `ChatPane`. |
| `subscribedThreadRef.current` | `"<tid>"`        | `null`               | Keeps the §(c)-guard invariant (`DESIGN-CHAT-UX.md §c`) — every write to `activeThreadId` routes through `setActiveThread`. |
| `threadsVisible`          | `true` or `false`    | `false`              | Collapse the rail if open — the rail is a chat-mode affordance; leaving chat closes it.          |
| `threadCursor`            | `number`             | `0`                  | Reset to avoid stale highlight on next ⌘T (matches existing ⌘T behaviour at `App.tsx:281`).      |
| `dispatchStatus`          | `'idle' \| 'in_flight'` | `'idle'`          | Placeholder-copy invariant. On re-entry, `ChatPane` must re-mount in the "no dispatch" copy.     |
| `provisionalUserIdRef.current` (new, §b) | `"<id>" \| null` | `null`   | Drop any in-flight echo bookkeeping.                                                             |
| `query`                   | any                  | `""`                 | Clear the composer. Without this the user is left staring at their sent text after Esc.          |
| `slashError`              | any                  | `null`               | Banner is chat-context; clear it on exit.                                                        |
| `selectedIndex`           | any                  | `0`                  | ResultsList remounts; reset to first row.                                                        |

### What does **NOT** reset

| Slice                            | Retained because                                                                                  |
|----------------------------------|---------------------------------------------------------------------------------------------------|
| Rust-side `chat_subscribe` task  | The in-flight stream keeps running server-side. See "Subscription lifetime" below.                |
| `threads` (rail list)            | The thread list survives — re-entering via ⌘T finds the same threads with no extra fetch.         |
| `threadsRef.current`             | Mirrors `threads`; same reason.                                                                   |
| `pluginInfo` / `detail`          | Not chat state. (They are `null` at this slot anyway per the Esc ordering above.)                 |
| `windowMode`                     | Exit-chat does not resize the window. User may still be in `expanded` for a code-diff review.    |
| Persisted messages on disk       | No deletion. Thread is recoverable via ⌘T.                                                       |

### Subscription lifetime

**Decision: keep the in-flight stream subscription alive.** Do NOT call
`chat_unsubscribe` on exit-chat.

Rationale:

| Candidate                                   | Impl effort | Risk if the stream was "almost done" | Re-entry UX        |
|---------------------------------------------|-------------|--------------------------------------|--------------------|
| **Leave subscription live** *(chosen)*      | zero        | none — stream finishes, events are dropped by the React listener because `subscribedThreadRef === null` | Clean — re-entering refetches message list fresh |
| Call `chat_unsubscribe` on exit             | +2 lines    | aborts a response the user paid tokens for; next re-entry sees truncated history | Cleaner logs, worse UX |
| Cancel stream server-side                   | backend work out-of-scope | loses the response | same               |

The stale-event guard at `App.tsx:~635` (the `subscribedThreadRef` check
introduced by TRK-584) already drops events whose `thread_id !== null &&
subscribed === null`. Exit-chat sets `subscribedThreadRef.current = null`
synchronously via the `setActiveThread(null)` helper, so any in-flight
`data_updated` events are dropped without mutating React state. This is
exactly the symmetric case the guard was designed for.

### Re-entry paths

Once exited, the user reaches chat again via one of:

1. **⌘T** — opens rail; rail row click → `loadThreadMessages(tid)` → fresh
   `chat_subscribe` + `list_messages` dispatch. History-on-disk is replayed.
2. **⌘N** — `handleNewThread` → fresh `chat_subscribe({threadId: null})`.
3. **Typing `/ask <text>`** — slash short-circuits, Enter runs
   `dispatchChat` which `invoke('chat_subscribe', { threadId: null })`
   before dispatching. New thread id is pinned on the first delta via the
   `subscribedThreadRef === null` branch.

All three paths work without any new plumbing — exit-chat resets state to
the pre-entry shape, and re-entry is identical to first entry. This is the
reason we do **not** need a dedicated "resume last thread" affordance.

### Implementation

Add a single `exitChat` helper, used only by the Esc handler:

```ts
const exitChat = useCallback(() => {
  setChatMessages([])
  setActiveThread(null)           // helper from TRK-584; updates the ref
  setThreadsVisible(false)
  setThreadCursor(0)
  setDispatchStatus('idle')
  provisionalUserIdRef.current = null   // §(b)
  setQuery('')
  setSlashError(null)
  setSelectedIndex(0)
  // Do NOT call chat_unsubscribe — see "Subscription lifetime" above.
}, [setActiveThread])
```

Then the Esc handler (`App.tsx:871-887`) grows one branch:

```ts
case 'Escape':
  if (windowModeRef.current !== 'default') return
  e.preventDefault()
  if (showActionPalette) {
    setShowActionPalette(false)
  } else if (detail !== null) {
    setDetail(null)
    setPluginInfo(null)
    detailLoadId.current++
  } else if (isChatActive) {         // NEW branch
    exitChat()
  } else {
    setQuery('')
    hideWindow()
  }
  break
```

Note the `isChatActive` check, not `chatMessages.length > 0`. This catches
the "thread opened via ⌘T but no delta yet" case (Case 9 in §(a) of
DESIGN-CHAT-UX.md).

### Visual feedback

None. Exit-chat is instant — `ChatPane` unmounts, `ResultsList` mounts with
whatever `displayResults` evaluates to for the now-empty query. No
animation, no toast, no banner. The rationale: Esc is a reversal gesture;
users already expect it to be immediate and silent (matches the existing
detail-close behaviour).

### Re-entry races

Concern: user hits Esc mid-stream at t=0; user presses ⌘T at t=50ms and
selects thread A at t=100ms. Meanwhile a `data_updated` event for thread A
from the *original* pre-Esc stream arrives at t=120ms.

Sequence of state writes:

| t (ms) | Action                          | `subscribedThreadRef` after | `activeThreadId` after |
|--------|---------------------------------|-----------------------------|------------------------|
| 0      | Esc → `exitChat`                | `null`                      | `null`                 |
| 50     | ⌘T → `setThreadsVisible(true)`  | `null`                      | `null`                 |
| 100    | click thread A → `loadThreadMessages('A')` → `setActiveThread('A')` synchronously | `'A'` | `'A'` |
| 100+   | `chat_subscribe({threadId: 'A'})` dispatched (async) | `'A'` | `'A'` |
| 120    | late `data_updated` for A from pre-Esc stream arrives | `'A'` | `'A'` |

At t=120 the guard sees `payload.thread_id === 'A' && subscribed === 'A'`,
so it passes — and overwrites `chatMessages` with whatever shape the old
stream emitted. **This is acceptable**: the old stream's `turn_event`
payload is already the thread-A full-history shape (history + current
turn), and the new `list_messages` dispatch (`loadThreadMessages`) will
overwrite it again within a few hundred ms. The worst visible artefact is a
brief flash of the prior-stream state. The alternative (epoch-numbering
subscriptions) is tracked in `DESIGN-REVIEW-CHAT-UX.md` W3 and is
explicitly out of scope for this mission.

---

## (b) Optimistic user-turn echo

### Goal

On Enter, show the user's submitted text in the chat pane immediately — no
`Waiting for response…` gap — while the subscribe + dispatch round-trip is
in flight. The first real `data_updated` event replaces the whole slice
(history + current user turn + current agent turn), naturally supplanting
the provisional.

### Provisional component shape

Exact JSON shape, chosen to match the server's first-delta user-turn
component bit-for-bit so the natural wholesale replacement is a no-op to
the DOM diff:

```ts
// matches plugins/chat/src/plugin.rs::user_turn_component → AgentTurn
// with role="user", streaming=false (omitted on wire, explicit false in TS).
const provisional: AgentTurnComponent & { provisional_id?: string } = {
  type: 'agent_turn',
  role: 'user',
  content: text,                        // verbatim user text — same string passed to dispatch_action
  streaming: false,                     // explicit; TS type lists it optional, concrete false is a valid match
  timestamp: Date.now(),                // client epoch ms; server uses its own ts on the real turn
  provisional_id: pid,                  // UI-only; see reconciliation rule below
}
```

Field-by-field justification:

| Field            | Server value                             | Provisional value                   | Compatible? |
|------------------|------------------------------------------|-------------------------------------|-------------|
| `type`           | `"agent_turn"`                           | `"agent_turn"`                      | yes         |
| `role`           | `"user"`                                 | `"user"`                            | yes         |
| `content`        | verbatim user text (no prefix)           | `text` (same string)                | yes         |
| `streaming`      | `false`, serialised as `null`/omitted    | `false` (explicit)                  | yes — `ComponentRenderer` treats `streaming !== true` as terminal |
| `timestamp`      | server epoch ms (ts of turn start)       | client `Date.now()`                 | near-identical on local dev; drift of ≤1 s acceptable — not rendered as a load-bearing id |
| `provisional_id` | *(absent)*                               | short UUID (e.g. `"prov_7f3a1c"`)   | UI-only; passes through `ComponentRenderer` as an unknown property which `AgentTurnRenderer` ignores (verify in row 9 of checklist) |

`provisional_id` is generated fresh per `dispatchChat` call and stored in
a ref for rollback:

```ts
const provisionalUserIdRef = useRef<string | null>(null)
```

### Reconciliation rule

**Replace-wholesale is the rule.** Verified correct:

1. `dispatchChat(text)` pushes `provisional` via
   `setChatMessages(prev => [...prev, provisional])`.
2. `chat_subscribe` resolves.
3. `dispatch_action { action: 'ask' }` runs. Backend `stream_ask` appends
   user message to store, computes `history = full[..full.len()-1]` (strips
   the just-appended user), starts stream.
4. First `TextDelta` (or, if empty, first `MessageStop`) fires
   `turn_event(history, text, accumulated, true, ts, beats)`.
5. Listener receives `data_updated`; runs `setChatMessages(payload.data)`.
6. Provisional is replaced — the new slice contains the real server-side
   user turn at index `history.len()` with `content === text`. DOM diff:
   `timestamp` changes by ≤1s, `provisional_id` disappears; React's key-less
   list reconciler sees both positions as "agent_turn / role=user / same
   content" and no visible flicker occurs.

**Why replace-wholesale is sufficient and no provisional-id matching is
needed at reconciliation time:** the server's first `data_updated` is
guaranteed to contain a user-turn component whose `content === text` at
position `history.len()`. Because the wholesale-replace contract in
`DESIGN-CHAT-SUBSCRIPTION.md §b` is already load-bearing, there is no
append-mode codepath we could diverge from — the provisional is atomically
evicted by the first event.

**`provisional_id` is for rollback only, not reconciliation.** The rule is:
between push and first `data_updated`, the provisional is the *only*
component in `chatMessages` whose `provisional_id` is set. Rollback can
identify it by that field without false positives.

### Failure rollback

`dispatchChat` chains `chat_subscribe → dispatch_action`. Either can reject.
Current code (`App.tsx:309-320`) appends a terracotta text component on the
`.catch` branch but does not remove the optimistic echo — leaving the user
staring at their sent text plus a "Failed to connect:" line, which reads as
"your message was sent but the response failed" (wrong — nothing was sent).

Updated failure path:

```ts
const pid = `prov_${Math.random().toString(36).slice(2, 8)}`
provisionalUserIdRef.current = pid
setChatMessages(prev => [
  ...prev,
  { type: 'agent_turn', role: 'user', content: text,
    streaming: false, timestamp: Date.now(), provisional_id: pid,
  } as AgentTurnComponent & { provisional_id: string },
])
setDispatchStatus('in_flight')
setQuery('')

invoke('chat_subscribe', { threadId: tid })
  .then(() => invoke('dispatch_action', {
    pluginId: 'chat', capabilityId: 'ask', actionId: 'ask',
    params: { text, thread_id: tid },
  }))
  .catch((err: unknown) => {
    setDispatchStatus('idle')
    const msg = err instanceof Error ? err.message : String(err)
    // Remove provisional by id; leave non-provisional turns untouched.
    setChatMessages(prev =>
      prev.filter(c =>
        !(c && typeof c === 'object' && 'provisional_id' in c
          && (c as { provisional_id?: string }).provisional_id === pid)
      ),
    )
    provisionalUserIdRef.current = null
    // Surface via slashError (banner), NOT by appending to chat.
    setSlashError(`Failed to dispatch /ask: ${msg.slice(0, 140)}`)
  })
```

Two differences from today's code:

1. **Remove the provisional** on the catch branch. `provisional_id` match
   avoids the "what if a real user turn with the same content slipped in
   between push and reject" edge — impossible in practice (we own the
   state transitions) but defensive and cheap.
2. **Surface the error via `slashError`** (banner), not by appending a
   terracotta `text` component to `chatMessages`. This reverts the
   implementation choice flagged in `DESIGN-REVIEW-CHAT-UX.md` W1 — the
   review recommended consolidating to chat-pane errors, but the
   consolidation breaks the semantic that `chatMessages` contains *server
   state only*. Putting a client-side error into `chatMessages` means the
   next `data_updated` replaces it away, hiding the error the moment the
   *next* dispatch succeeds. Banner surface is the right channel.

### Success-path cleanup of provisional bookkeeping

On the first `data_updated` that includes the real user turn, the listener
already calls `setChatMessages(payload.data)` — this drops the provisional
slot. The ref must be cleared too:

```ts
// inside the listener, after setChatMessages(payload.data):
if (provisionalUserIdRef.current !== null) {
  provisionalUserIdRef.current = null
}
```

Placing this inside the listener (rather than on every `data_updated`
event, regardless of whether the provisional was outstanding) ensures the
ref is always cleared exactly once per dispatch — on the first real event.

### Interaction with stale-thread guard (§c of DESIGN-CHAT-UX.md)

If the user dispatches to thread A, Esc-exits, ⌘T to thread B before the
first data_updated arrives: `subscribedThreadRef.current` is now `'B'`
(or `null` if mid-exit). The late `data_updated` for A is dropped by the
existing guard. The provisional from the A dispatch was cleared by
`exitChat()` (§(a) state-transition table). No rollback needed. Covered
by row 11 of the reviewer checklist.

### Empty-text guard

Already enforced in `handleEnter` at `App.tsx:422` (`if
(selected.query.trim() === '') return`) for the ask-claude path, and by
the `if (isChatActive && query.trim() !== '')` guard at
`App.tsx:410` for the sticky path. No optimistic echo is generated for
empty dispatches — `dispatchChat` is never called with empty text. No
change needed.

### Guard against double-echo on IME commit

The existing `isComposing` / `keyCode === 229` guard at
`App.tsx:805-806` already prevents a double Enter during IME candidate
commit. Reconfirmed applicable here — the optimistic echo lives inside
`dispatchChat`, which is called from `handleEnter`, which is gated by
that guard. No additional plumbing.

---

## (c) Chat-mode composer decision

### Options

**Option A — Top-bar placeholder/icon swap.** Keep the single
`SearchBar` top-level input. Conditionally swap:
- `placeholder`: `"Search capabilities…"` → `"Message the thread…"`
  when `isChatActive`.
- Leading icon: magnifier (`<circle>`+`<line>` at `App.tsx:1084-1095`) →
  a chat-bubble SVG (same 16×16 viewBox, 14px render) when
  `isChatActive`.
- Possibly a subtle left-border accent in chat mode — out of scope;
  defer to a visual-design mission.

**Option B — Move input into `ChatPane`.** Hide the top-bar input
when `isChatActive` (or keep as a disabled breadcrumb), mount a
composer `<input>` at the bottom of `ChatPane`. Own its own
`onChange`/`onKeyDown` handlers, plumb focus from
`transitionTo`/mount-effect.

### Comparison

| Axis                               | A — top-bar swap            | B — input-in-ChatPane                      |
|------------------------------------|-----------------------------|--------------------------------------------|
| Impl effort                        | 4–6 lines                   | ~80 lines (new handler wiring + focus management + keyboard routing) |
| Focus/keyboard management          | zero change (reuses existing `inputRef`, `handleKeyDown`, IME guard) | duplicated — need `chatInputRef`, Esc routing, Tab-between-inputs decision |
| Slash grammar (`/ask`, `/tracker`) | still typed in the same spot, same behaviour, no mental mode-switch | composer must either also accept slashes (duplicates slash grammar) or refuse them (regresses cross-mode dispatch) |
| Sticky-mode non-slash Enter        | unchanged — `App.tsx:410` routes non-slash text to chat when `isChatActive` | still works but requires composer to reuse `handleEnter` |
| Empty-state placeholder copy       | composer's own prompt doubles as the cue | the `ChatPane` two-state placeholder (TRK-584) still renders above the composer — two empty-state copies compete |
| Visual clarity of mode             | medium — placeholder + icon cue, but the shell still looks like a launcher | high — input position in the conversation area is unambiguous |
| Bundle delta (gzipped)             | **≈ +80 B** (one conditional placeholder string + inline icon swap + optional ternary on a single className) | **≈ +1.6–2.0 KB** (second `<input>`, separate `onKeyDown`, focus-management effect, prop drilling for `dispatchChat` / `handleKeyDown` into `ChatPane`) |
| Risk of regressing TRK-584 work    | low — same input path, same Esc/Enter plumbing, same debug instrumentation | medium — `handleKeyDown`, the §c stale-thread guard, and the §e placeholder all assume a single input with known focus. Duplicating the input means auditing all three. |
| Risk of regressing FileRef chip    | zero                        | medium — FileRef lives at the top bar; moving input away from FileRef orphans the `⌘⇧E`-opened editor's affordance |
| Discoverability of exit-chat (§a)  | neutral — Esc still clears/hides from anywhere | worse — focus-in-ChatPane + Esc may route through `ChatPane`-local handler first if not wired carefully |

Bundle-size estimates derived from:

- Option A: one extra conditional string literal + one extra `<svg>` inlined
  (share the existing 14×14 frame). Strings and inline SVGs gzip to ≤ 100 B.
  Conservative budget: 120 B.
- Option B: a full `<input>` JSX block (≈ 320 B minified), a second
  `handleChatComposerKeyDown` useCallback (≈ 180 B), an auto-focus useEffect
  (≈ 120 B), prop-drilling (≈ 60 B), and an additional ref + ref-forwarding
  boilerplate (≈ 200 B). Gzipped at the repeat-heavy ratio dust builds have
  been measuring (≈ 0.45 of minified for JSX-heavy additions), that's
  ~1.6–2.0 KB gzipped. Additive because `ChatPane` is in the main bundle
  (no lazy-chunk split today).

### Decision: **Option A — top-bar placeholder + icon swap**

Primary reason: the single-input invariant is load-bearing for every
TRK-584 deliverable (slash routing, sticky predicate, `isComposing` guard,
`handleEnter` branch tree, stale-thread guard). Duplicating the input
surface would force a re-audit of all four without a commensurate UX gain
— the composer-in-ChatPane model is *clearer* but not *better enough* to
repay the ~1.5 KB bundle and the regression surface.

What we're giving up: the strong visual cue that "you are in a
conversation, not a launcher." Mitigation: the placeholder change
("Message the thread…" vs "Search capabilities…") plus the icon swap
carries enough signal for the MVP user base (1 — the author). If
telemetry / user reports surface confusion, we revisit with Option B as
a future mission, scoped with proper focus/keyboard plumbing instead of
piggybacking on TRK-6.

Rejected: Option B. Reason: cost disproportionate to benefit, regression
surface too wide given TRK-584's invariants are one day old.

### Implementation — Option A

Extend `SearchBarProps` with a `mode` discriminator:

```ts
type SearchBarProps = {
  value: string
  onChange: (v: string) => void
  onKeyDown: (e: React.KeyboardEvent) => void
  mode: 'search' | 'chat'                         // NEW
}
```

`SearchBar` body:

```tsx
<svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none"
     className="shrink-0" style={{ color: 'var(--text-secondary)' }}>
  {mode === 'chat' ? (
    // Chat bubble — single-path equivalent to hero-icons "chat-bubble-oval"
    <path d="M8 2a6 6 0 0 0-5.2 9l-.8 3 3-.8A6 6 0 1 0 8 2z"
          stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" />
  ) : (
    <>
      <circle cx="6.5" cy="6.5" r="5" stroke="currentColor" strokeWidth="1.5" />
      <line x1="10.5" y1="10.5" x2="14.5" y2="14.5"
            stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
    </>
  )}
</svg>
…
<input …
  placeholder={mode === 'chat' ? 'Message the thread…' : 'Search capabilities…'}
  …
/>
```

Callsite at `App.tsx:957`:

```tsx
<SearchBar
  ref={inputRef}
  value={query}
  onChange={…}
  onKeyDown={handleKeyDown}
  mode={isChatActive ? 'chat' : 'search'}     // NEW
/>
```

No other behaviour changes. Slash grammar still fires at the top bar
(desirable — `/tracker create foo` from inside a chat works today and
stays working). Sticky-mode non-slash Enter routes to `dispatchChat`
unchanged.

---

## (d) Decision-table updates

Rows added to the §(a) case table of `DESIGN-CHAT-UX.md` for the new
states introduced here:

| # | State                               | `query`                | `chatMessages` | `activeThreadId` | `dispatchStatus` | `isChatActive` | `isAskClaude` | Composer mode | Left column                   | Right pane        | Notes                                                      |
|---|-------------------------------------|------------------------|----------------|------------------|------------------|----------------|---------------|---------------|-------------------------------|-------------------|------------------------------------------------------------|
| 10 | **chat-active**, idle, with history | `""`                   | non-empty      | non-null         | `'idle'`         | `true`         | `true`        | `chat`        | none (`ThreadRail` iff ⌘T)    | `ChatPane`        | `SearchBar` placeholder reads `Message the thread…`        |
| 11 | **chat-active**, typing non-slash   | `"follow up"`          | non-empty      | non-null         | `'idle'`         | `true`         | `true`        | `chat`        | none                          | `ChatPane`        | Sticky path (§a case 8 updated). Enter → `dispatchChat`.  |
| 12 | **chat-active**, typing `/tracker`  | `"/tracker create x"`  | non-empty      | non-null         | `'idle'`         | `true`         | `true`        | `chat`        | none                          | `ChatPane`        | Composer stays in `'chat'` mode, but Enter still routes to the tracker plugin — cross-mode dispatch preserved |
| 13 | **chat-exiting** (transient, 1 tick) | `""`                  | was non-empty; post-`exitChat` → `[]` | was non-null → `null` | `'idle'` | `false` (post) | `false` (post) | `search`      | `ResultsList` (fresh mount)   | `DetailPane`      | Triggered by Esc in default mode with `isChatActive=true`. Step order: after detail-close, before hide-window |
| 14 | **chat-active**, dispatch-in-flight | `""` (cleared on Enter) | has provisional echo + prior turns | non-null | `'in_flight'` | `true`    | `true`        | `chat`        | none                          | `ChatPane` (no placeholder copy — messages non-empty) | Optimistic echo visible; `Waiting for response…` never shows because `messages.length > 0` path runs |
| 15 | **chat-active**, first dispatch ever | `""` (cleared)         | `[provisional]` only | `null` (pins on first delta) | `'in_flight'` | `true` | `true`  | `chat`        | none                          | `ChatPane`        | Provisional alone — server-side turn not yet received. Placeholder copy is NOT shown because `messages.length > 0` |
| 16 | **chat dispatch failed**            | `""` (cleared)         | provisional removed → prior state | non-null (unchanged) | `'idle'` (reset on catch) | depends on prior | depends | `chat` or `search` | depends       | depends           | `slashError` banner shows `Failed to dispatch /ask: …`; `chatMessages` is back to pre-dispatch shape |

These rows **supersede** the original §(a) cases 2, 8, 9 of DESIGN-CHAT-UX.md
only where the composer-mode column is new; the `displayResults` /
`isAskClaude` derivations are unchanged. Rows 13 and 16 are net-new state
shapes introduced by this mission.

### `displayResults` derivation (unchanged)

No change to the `displayResults` derivation from DESIGN-CHAT-UX.md §(a).
Adding a composer mode is orthogonal to the results-array shape. Both
modes share the same `slash → isChatActive → results` precedence.

### `isAskClaude` derivation (unchanged)

Still `displayResults[selectedIndex]?.kind === 'ask_claude'`. Unchanged.

### `ChatPane` prop surface (extended by one, not breaking)

Unchanged props (`messages`, `dispatchStatus`). The empty-message branch
still renders the §(e) two-state placeholder copy. Optimistic echo lives
*inside* `chatMessages` (as an `agent_turn` component), so the existing
`ComponentRenderer` path at `App.tsx:1522` handles it without change.

---

## (e) Reviewer checklist

Each row is a discrete behaviour the review phase runs live and records as
pass/fail in `plugins/dust/DESIGN-REVIEW-CHAT-UX-2.md`. The reproduction
column names the exact user action — a reviewer can mark each row without
re-reading the App.tsx diff.

### Exit-chat (§a)

| #  | Deliverable                                                  | Reproduction step                                                                                                                                  | Pass signal                                                                                                                           |
|----|--------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| 1  | Esc exits chat when no modal/detail is open                  | Start a chat (`/ask hello`, wait for response). Press `Esc` once.                                                                                  | `ChatPane` unmounts; `ResultsList` mounts with empty-state copy. The window stays open.                                               |
| 2  | Esc does NOT exit chat when a detail pane is loaded          | Start a chat. Without leaving, open another plugin's detail (type `tracker`, Enter). Press `Esc`.                                                  | Detail pane closes; chat state remains (`chatMessages` still populated, `isChatActive` still true). Second `Esc` exits chat.          |
| 3  | Esc does NOT exit chat when the action palette is open       | Start a chat. `Ctrl+K` to open action palette. Press `Esc`.                                                                                        | Palette closes; chat state remains. Second `Esc` exits chat.                                                                          |
| 4  | Exit-chat clears all documented slices                       | Open devtools React inspector, start a chat, press `Esc`.                                                                                          | Inspector shows `chatMessages = []`, `activeThreadId = null`, `threadsVisible = false`, `dispatchStatus = 'idle'`, `query = ''`.     |
| 5  | Exit-chat does NOT cancel the in-flight stream               | Dispatch a long prompt (`/ask write a 500-word essay`). Within 200ms of Enter, press `Esc`. Wait 10s. Press `⌘T`, click the thread.                | The thread's message-list shows the full response — the stream completed server-side even though the UI was exited.                  |
| 6  | Re-entering via ⌘T restores messages from store              | After row 5's Esc, `⌘T`, click the streamed thread.                                                                                                | Thread's full message history loads (server-side replay via `list_messages`). No UI flicker from the earlier-dropped late events.     |
| 7  | Rail closes on exit                                          | Open rail (`⌘T`) while in a chat. Press `Esc`.                                                                                                     | Rail is gone in addition to the chat.                                                                                                 |
| 8  | Step-back order is preserved                                 | Sequence: palette open + detail loaded + chat active. Press `Esc` three times.                                                                     | 1st closes palette, 2nd closes detail, 3rd exits chat. 4th press hides the window.                                                    |

### Optimistic echo (§b)

| #  | Deliverable                                                  | Reproduction step                                                                                                                                  | Pass signal                                                                                                                           |
|----|--------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| 9  | Provisional appears synchronously on Enter                   | With `DUST_DEBUG_CHAT=1`, type `/ask hello`, press Enter. In devtools Components tab, inspect `chatMessages` within the same frame as the Enter.   | `chatMessages` contains one `agent_turn` with `role:"user"`, `content:"hello"`, `provisional_id:"prov_…"`, `streaming:false`, `timestamp:<client ms>`. No `Waiting for response…` placeholder shown. |
| 10 | Provisional is replaced by server turn without flicker       | Keep row 9 setup. Observe the first `data_updated` arrival (React Profiler or console log).                                                        | `chatMessages` length either stays at 1 (server user-turn replaces provisional at the same index) or jumps to 2+ (history + current user + current agent). In neither case does the user-turn flash/unmount. `provisional_id` is gone. |
| 11 | Exit during in-flight drops provisional cleanly              | Type `/ask give me a haiku`, Enter, then immediately press `Esc`.                                                                                  | `chatMessages` is `[]` after Esc. No stray provisional left behind. Re-entering the thread (⌘T) shows the full response from the store (passed row 5). |
| 12 | Failure on dispatch removes provisional AND banners via slashError | Kill the chat plugin process. Press `/ask hi` + Enter.                                                                                        | `slashError` banner reads `Failed to dispatch /ask: {msg}`. `chatMessages` reverts to pre-dispatch shape (no provisional remains). `dispatchStatus === 'idle'`. No terracotta text component is appended to the chat. |
| 13 | Provisional shape matches server user-turn component shape   | Static inspection — open `plugins/chat/src/plugin.rs::user_turn_component` and `plugins/dust/src/App.tsx::dispatchChat`.                          | Fields `type`, `role`, `content`, `timestamp` are identical; `streaming` is `false` on both sides; no extra load-bearing field on the server-side turn. Only addition in provisional is `provisional_id`. |
| 14 | Empty query dispatches no provisional                        | In an active chat, clear the input. Press Enter.                                                                                                   | `chatMessages` unchanged; no new provisional. (Existing empty-guard at `App.tsx:410,422`.)                                            |
| 15 | IME commit Enter does not double-echo                        | Switch to a CJK IME. Type romaji, commit with Enter. Press Enter again (no composition).                                                           | Only one provisional is pushed (on the second, non-compose Enter). No dangling from the IME Enter.                                    |

### Composer mode (§c)

| #  | Deliverable                                                  | Reproduction step                                                                                                                                  | Pass signal                                                                                                                           |
|----|--------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| 16 | Placeholder swaps on chat entry                              | Summon window (search mode), note placeholder. Dispatch `/ask hi` and wait for response.                                                           | Placeholder was `Search capabilities…` before; is `Message the thread…` after first delta pins `activeThreadId`.                      |
| 17 | Placeholder swaps on chat exit                               | Immediately after row 16, press Esc.                                                                                                               | Placeholder reverts to `Search capabilities…`.                                                                                        |
| 18 | Icon swaps in sync with placeholder                          | Same as row 16/17.                                                                                                                                 | Magnifier SVG in search mode; chat-bubble SVG in chat mode. No mid-state where one is updated but not the other.                      |
| 19 | Slash still dispatches from chat mode                        | In active chat (placeholder says `Message the thread…`), type `/tracker create test`, Enter.                                                       | Dispatches to tracker plugin (no chat dispatch, no provisional echo, no `/tracker` sent to Claude). Composer still reads `Message the thread…` throughout typing. |
| 20 | Non-slash Enter in chat mode dispatches chat                 | In active chat, type `follow up`, Enter.                                                                                                           | Provisional appears; dispatch runs via sticky path (§a-case-11 in the updated table).                                                 |
| 21 | Bundle size delta is within budget                           | `cd plugins/dust && npm run build` (or `yarn build`). Read the gzip line from the build output.                                                    | Gzipped main bundle ≤ 156.51 KB (baseline 154.51 KB + 2.0 KB cap). Reported delta and absolute size in the review doc.                |

### Decision-table (§d)

| #  | Deliverable                                                  | Reproduction step                                                                                                                                  | Pass signal                                                                                                                           |
|----|--------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| 22 | Rows 13 and 16 of §(d) are spec'd as new shapes              | Inspect `DESIGN-CHAT-UX-2.md §(d)`.                                                                                                                | Rows for `chat-exiting` (13) and `dispatch-failed` (16) are present; each names all of `query`, `chatMessages`, `activeThreadId`, `dispatchStatus`, `isChatActive`, `isAskClaude`, composer mode. |
| 23 | Rows 10–12 and 14–15 clarify the `chat-active` composer mode | Inspect §(d).                                                                                                                                      | Each has a concrete `composer mode: chat` entry consistent with §(c) decision. Cross-mode slash (`/tracker` in chat) is explicitly row 12. |

### Regression

| #  | Deliverable                                                  | Reproduction step                                                                                                                                  | Pass signal                                                                                                                           |
|----|--------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| 24 | TRK-584 reviewer checklist rows 1–13, 17–20 still pass       | Re-run the DESIGN-REVIEW-CHAT-UX.md §Slash/sticky, §Stale-thread, §Placeholder rows.                                                               | All pass. No regression in sticky-mode, stale-thread guard, or placeholder two-state.                                                 |
| 25 | `cargo test -p chat -p dust-core -p dust-tauri` green        | Command as written.                                                                                                                                | Exit code 0.                                                                                                                          |
| 26 | `cd plugins/dust && npm run build && npm test` green         | Command as written.                                                                                                                                | Build OK; vitest all green.                                                                                                           |
| 27 | `tracker update TRK-6 --status done`                         | After verification.                                                                                                                                | Recorded in review.                                                                                                                   |

---

## Risks

1. **Provisional timestamp drift.** Client `Date.now()` may differ from
   server ts by up to 1 s on first run after clock sync. Because timestamp
   isn't rendered as a sort key (server-sent order is authoritative), this
   doesn't cause reordering. Mitigation: none needed.
2. **`provisional_id` leaks into renderer.** `ComponentRenderer`'s
   `AgentTurnRenderer` reads `role`, `content`, `streaming`, `timestamp`
   only. An extra property passes through harmlessly (verified by row 9).
   Still, an ESLint/TS lint could flag the cast as unsafe. Mitigation:
   defined as an intersection type at the provisional's push site only;
   the `AgentTurnComponent` export doesn't change.
3. **Late server turn after exit races with re-entered thread.** Covered in
   §(a) "Re-entry races" — accepted as a brief flash, alternative is epoch
   numbering (W3 in the TRK-584 review; out of scope).
4. **Composer-mode change is cosmetic only.** Doesn't solve "users don't
   know they're in chat" structurally — if confusion persists, Option B
   becomes a follow-up. Mitigation: telemetry is out of scope for this
   MVP; watch for user feedback.
5. **Esc-exit depth confusion.** A user with palette + detail + chat
   stacked needs 3 Esc presses before the window hides. The step-back
   order table (§a) documents this — reviewer row 8 verifies it, and the
   gradient is consistent with macOS app conventions (modal-dismiss >
   view-dismiss > app-hide).
6. **`slashError` banner copy conflates slash failure and chat-dispatch
   failure.** Both end up in the same banner; a user reading the banner
   must tell them apart by the message prefix. Accepted — same trade-off
   accepted in TRK-584 §(d) "Banner reuse over toast."

## Trade-offs accepted

- **Single-input composer over dedicated ChatPane input.** Loses some
  visual clarity; gains ~1.5 KB bundle and zero new regression surface
  against the TRK-584 invariants.
- **No subscription cancel on exit.** Finishing the token payload is
  strictly better for the user than aborting it; the stale-event guard
  makes the late events silent. The only cost is the server does work
  whose result is dropped client-side.
- **Banner, not chat-pane, for `/ask` failure.** Reverts the
  DESIGN-REVIEW-CHAT-UX.md W1 implementation choice. Reason: client-side
  errors don't belong in `chatMessages` because the next `data_updated`
  wholesale-replaces them away.
- **`provisional_id` is a short random string, not a UUID v4.**
  `Math.random().toString(36).slice(2, 8)` yields ~6 base-36 chars → ~30
  bits of entropy. Only needs to be unique within the N=1 in-flight
  dispatches per tab. Collision probability is effectively zero; saves
  pulling `crypto.randomUUID()` branches for older WebViews.

---

<!-- scratch -->
Implementation notes for `implement-frontend`:

1. Add `exitChat` as a single `useCallback` near `handleNewThread`; its
   dep array is `[setActiveThread]` only (the setters are stable).
2. Wire the Esc branch at `App.tsx:871-887` — insert the `isChatActive`
   case between `detail !== null` and the `else`.
3. `provisionalUserIdRef` is a `useRef<string | null>(null)`. Keep it
   near the `subscribedThreadRef` declaration for visual symmetry.
4. `dispatchChat` changes in two places: push the provisional + stash its
   id before the first `invoke`; on catch, filter `chatMessages` by id
   AND surface via `setSlashError` (revert the W1 regression).
5. In the chat-event listener, clear `provisionalUserIdRef.current` on
   `data_updated` — a one-line addition immediately after
   `setChatMessages(payload.data)`.
6. `SearchBar` gets one new prop `mode: 'search' | 'chat'`. Derive at
   the callsite as `isChatActive ? 'chat' : 'search'`. The icon and
   placeholder swap is the entire delta inside `SearchBar`.
7. Do NOT move input into `ChatPane`. Option B is explicitly rejected
   in §(c); revisit only if a follow-up tracker issue gets filed.
8. Bundle-size verification: run `npm run build`, read the
   `Main (gzip)` line, compare to 154.51 KB baseline, record delta in
   DESIGN-REVIEW-CHAT-UX-2.md row 21.
<!-- /scratch -->

LEARNING: when an optimistic UI lives under a "replace-wholesale on next
event" contract, reconciliation is free — the provisional dies on the
first real payload without any id-matching at reconcile time. IDs become
rollback-only bookkeeping.

PATTERN: Esc step-back as a gradient (modal → view → pane → app-hide) is
the right mental model for keyboard-first launcher UIs. Each Esc
undoes exactly one level of commitment. Inserting a new level means
choosing where in the gradient it belongs, not whether it belongs.

DECISION: top-bar mode swap over ChatPane-local input. Primary axis:
preserving the single-input invariant that every TRK-584 deliverable
(slash routing, sticky, stale-thread guard, IME guard, `handleEnter`
branch tree) depends on. ~1.5 KB saved and zero regression-surface
expansion are the proximate gains; clarity loss is accepted and watched.

DECISION: banner (`slashError`) for `/ask` dispatch failures, not
chat-pane terracotta text. Reason: chatMessages is server state; client
errors there are wholesale-replaced away by the next event. Reverts
TRK-584 W1 consolidation.

DECISION: do not call `chat_unsubscribe` on exit. Finishing the
response server-side is strictly better UX than aborting it, and the
stale-event guard already drops the dropped-on-floor events.
