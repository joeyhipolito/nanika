---
produced_by: architect
phase: phase-1
workspace: 20260421-822f29a6
created_at: "2026-04-21T16:45:00Z"
confidence: high
depends_on: []
token_estimate: 7200
---

# DESIGN-CHAT-UX

Target mission: **TRK-584 — Chat UX polish (sticky mode, history, slash routing)**.
This doc is the single authoritative spec consumed by `implement-frontend`
(`plugins/dust/src/App.tsx`) and `implement-backend`
(`plugins/chat/src/{plugin.rs,server.rs}`). Neither worker should invent
defaults outside the decisions recorded here; gaps are flagged as
`DECISION:` markers for the orchestrator.

## Context

Eight compounding defects make the dust chat surface feel broken even though
each one is small. The pressure points:

1. `displayResults` is derived purely from `query` (via `search_capabilities`
   → fuzzy match). When the user types `/ask tr`, fuzzy search runs against
   that literal string, usually returns one non-chat match (tracker), and
   `isAskClaude` flips to `false` mid-typing — ChatPane unmounts.
2. `turn_event` on the server emits only the current `[user, agent]` pair,
   so the moment the agent turn closes and a *new* user turn starts, the
   React slice is replaced by a two-component list and the previous turns
   vanish. The chat reads as a one-shot prompt.
3. Follow-ups require re-typing `/ask` — the sticky predicate does not exist.
4. A suspected auto-submit while the user is still typing `/ask X`. May be
   real (double-fire) or a perception artefact (render sync); currently
   undiagnosed.
5. The `ChatPane` placeholder (`Waiting for response…`) is shown the instant
   `isAskClaude` becomes true, even before any dispatch — the text is wrong.
6. The chat-event listener subscribes once with `[]` deps and uses
   `threadsRef.current` only for the refresh-gating branch, not for the
   "is this my thread?" guard. A rapid ⌘T thread switch can leak events from
   the prior thread into the new slice.
7. `fetchThreads` / `loadThreadMessages` failures are swallowed by
   `console.error` — the rail stays on "Loading…" indefinitely.
8. Debug `eprintln!` noise in `server.rs` / `plugin.rs` leaks into tailed logs.

## Non-goals

- Tool registration (TRK-575 — stays commented out in `build_request`).
- New Tauri commands or new chat actions beyond what TRK-569 shipped.
- Persistence across webview restart — subscription behaviour is unchanged.
- Multi-thread simultaneous rendering — one active thread at a time.

---

## (a) Derivation of `displayResults` / `isAskClaude` / ChatPane mounting

### State inputs

| Name               | Source                                         | Kind                          |
|--------------------|------------------------------------------------|-------------------------------|
| `query`            | `SearchBar` onChange                           | `string`                      |
| `chatMessages`     | `dust://chat-event` `data_updated` payload     | `Component[]`                 |
| `activeThreadId`   | first delta carrying `thread_id` / rail click  | `string \| null`              |
| `parseSlash(query)`| pure fn over `query`                           | `SlashCommand \| null`        |

### Derived values

```ts
const slash = parseSlash(query)                                    // pure
const isChatActive =                                               // NEW
  chatMessages.length > 0 || activeThreadId !== null

const displayResults: DisplayResult[] = useMemo(() => {
  // Slash query never falls through to search results.
  if (slash) return [{ kind: 'ask_claude', query }]
  // Sticky mode: plain text while chat is active stays in ChatPane.
  if (isChatActive && query.trim() !== '') {
    return [{ kind: 'ask_claude', query }]
  }
  // Existing behaviour for everything else.
  if (results.length > 0 || query.trim() === '') {
    return results.map(m => ({ kind: 'match' as const, match: m }))
  }
  return [{ kind: 'ask_claude', query }]
}, [slash, query, results, isChatActive])

const activeResult = displayResults[selectedIndex]
const isAskClaude = activeResult?.kind === 'ask_claude'
```

`isAskClaude` is the single gate for ChatPane mounting (unchanged line-for-line
from `App.tsx:810-811`). The refactor is entirely in how `displayResults` is
computed above it.

### Case table — what the user sees for each input combination

Legend: `ChatPane` = right pane mounts as `<ChatPane>`; `DetailPane` = the
existing plugin detail view; `ResultsList` = left column when
`!isAskClaude`; `ThreadRail` = left column when `railActive`.

| # | `query`                | `chatMessages` | `activeThreadId` | `parseSlash` | `displayResults[0]`         | `isAskClaude` | Left column        | Right pane        | `search_capabilities` fires? |
|---|------------------------|----------------|------------------|--------------|-----------------------------|----------------|--------------------|-------------------|------------------------------|
| 1 | `""`                   | `[]`           | `null`           | `null`       | *(empty — no list row)*     | `false`        | `ResultsList` (empty-state copy) | `DetailPane` (empty)   | yes (empty string) |
| 2 | `""`                   | non-empty      | any              | `null`       | `ask_claude("")` **NEW**    | `true`         | `ThreadRail` iff `⌘T`, else none | `ChatPane`  | no (short-circuit) |
| 3 | `"/"`                  | any            | any              | `null` (grammar requires `/<word>`) | results of search for `/` (usually empty ⇒ ask_claude fallback) | `true`  | `ResultsList` or none | `ChatPane`       | yes (literal `/`) |
| 4 | `"/ask tra"`           | any            | any              | `{prefix:"ask", args:"tra"}` | `ask_claude("/ask tra")` | `true`         | none               | `ChatPane`        | **no** (slash short-circuit) |
| 5 | `"/tracker new foo"`   | any            | any              | `{prefix:"tracker", args:"new foo"}` | `ask_claude("/tracker …")` | `true` | none               | `ChatPane`        | **no** |
| 6 | `"/xyzzy"` (no plugin) | any            | any              | `{prefix:"xyzzy", args:""}` | `ask_claude("/xyzzy")` | `true` | none | `ChatPane` | **no** (error banner on Enter, not during typing) |
| 7 | `"hello"` plain text   | `[]`           | `null`           | `null`       | first match or `ask_claude("hello")` fallback | depends on results | `ResultsList` | `DetailPane` or `ChatPane` (fallback) | yes |
| 8 | `"hello"` plain text   | non-empty      | non-null         | `null`       | `ask_claude("hello")` **NEW** | `true`       | none               | `ChatPane`        | **no** (short-circuit) |
| 9 | `"hello"` plain text   | `[]`           | non-null         | `null`       | `ask_claude("hello")` **NEW** (thread open, no msgs yet) | `true` | none | `ChatPane` | **no** |

Notes:

- Case 2 is important: once a thread is live, an empty query still renders
  ChatPane. Rationale — the user may clear their input to scroll/read
  history. Today this flips back to ResultsList, which is jarring.
- Case 3 (`/`): `parseSlash` returns `null` until there is at least one
  non-whitespace char after `/` (grammar spec at
  `plugins/dust/src/slashGrammar.ts`). The single `/` is treated as raw
  text that rarely fuzzy-matches anything; the existing
  "no-results-⇒-ask_claude" fallback covers it. We do NOT add a special
  case for bare `/`.
- Cases 4–6 diverge from today's behaviour: `search_capabilities` must not
  fire for slash queries. Reason — it's wasted work, and on a slow search
  backend the result mutation can arrive *after* the slash short-circuit
  took effect, flickering the UI from ChatPane to DetailPane and back.
- **Skip the search effect for slash queries** (keep the effect, guard the
  body): see `App.tsx:199-212`. Add `if (slash) { setResults([]); return }`
  inside the 10ms debounce.

### Enter-key routing (once the user commits)

Enter handler (`handleEnter`) branches in this order:

1. If `parseSlash(query)` is truthy ⇒ `dispatchSlash(slash)`, clear input.
2. Else if `isChatActive` and `query.trim() !== ""` ⇒ dispatch to chat
   (new branch; text = `query`, thread = `activeThreadId`), clear input.
3. Else if `activeResult?.kind === 'ask_claude'` ⇒ existing Ask-Claude path,
   clear input.
4. Else (`activeResult.kind === 'match'`) ⇒ existing `render_ui` path;
   query is **not** cleared (matches today's detail-pane behaviour).

"Clear input" means `setQuery('')`. After a successful slash or chat
dispatch, the `chat_subscribe` happens first (invariant shared with
`handleEnter` today, `App.tsx:327`). `setQuery('')` is fired **synchronously**
before the IPC `.then` — we do not wait for the plugin ack before clearing,
because the ack has no semantic payload and blocking the input would hurt
typing flow.

### DECISION: no debounce on Enter

Enter always fires. If a delta is mid-stream from a prior dispatch, the
user's second Enter dispatches a new `ask` action — the chat plugin is
responsible for serialising those turns per thread (today it appends the
user message to the store then awaits the prior stream via `stream_ask`'s
single-task lifecycle). Dust does not gate Enter.

---

## (b) Server-side `turn_event` refactor

### Current shape (to be replaced)

```rust
// plugins/chat/src/plugin.rs:617
fn turn_event(user_text, agent_text, streaming, ts, beats) -> Envelope {
    let mut components = vec![
        user_turn_component(user_text, ts),        // just "this turn's user"
        agent_turn_component(agent_text, streaming, ts),
    ];
    components.extend_from_slice(beats);
    data_updated_event(components)
}
```

Problem: each `data_updated` replaces `chatMessages` wholesale on the React
side (single-slice contract from DESIGN-CHAT-SUBSCRIPTION.md §(b)). When a
second turn starts, the server emits a fresh 2-component list and prior
turns vanish.

### New shape

`turn_event` receives the full thread history (stored messages, in insertion
order) plus the **current in-flight turn's user text and streaming agent
text** plus any beats for the current turn. Ordering is fixed and documented;
the React side continues to `setChatMessages(payload.data)` verbatim.

```rust
fn turn_event(
    history: &[StoredMessage],   // all prior messages for this thread (ASC by created_at)
    current_user_text: &str,     // the user text of the turn now streaming (NOT yet persisted as part of history for the current turn)
    current_agent_text: &str,    // accumulated delta so far this turn
    streaming: bool,             // false only on MessageStop
    ts: u64,                     // start-of-turn epoch ms
    beats: &[Component],         // current turn's tool-call beats
) -> Envelope {
    let mut components: Vec<Component> = Vec::with_capacity(history.len() + 2 + beats.len());
    for m in history {
        components.push(Component::AgentTurn {
            role: m.role.clone(),              // "user" | "assistant"
            content: m.content.clone(),
            streaming: false,                  // history is always terminal
            timestamp: Some(m.created_at as u64),
        });
    }
    components.push(user_turn_component(current_user_text, ts));
    components.push(agent_turn_component(current_agent_text, streaming, ts));
    components.extend_from_slice(beats);
    data_updated_event(components)
}
```

### Ordering guarantees (documented contract the React side depends on)

1. **Chronological history prefix.** Index `0..history.len()` is `history`
   in ASC `created_at` order, exactly as returned by
   `store.thread_messages(&thread.id)`. Consumers may rely on this to
   render turn bubbles without re-sorting.
2. **Current user turn** at `history.len()`.
3. **Current agent turn** at `history.len() + 1`. The `streaming` flag is
   `true` on every `TextDelta` / `ToolUse` emission and `false` exactly
   once (on `MessageStop`).
4. **Beats** at `history.len() + 2 ..`, in `beats` vec order (which is
   chronological first-seen, with in-place replacement preserving position
   via `push_or_replace_beat` — unchanged).
5. **Exactly one streaming AgentTurn** per event. The UI's stream-close
   logic (`App.tsx:534-548`) iterates `prev.length - 1` assuming the last
   agent turn is the live one; the new layout keeps that invariant because
   beats live *after* the agent turn.

**Non-goal:** we do not move beats to sit under the agent turn they
originated from. That's a renderer concern (DESIGN-CHAT-SUBSCRIPTION §
`Component::ToolCallBeat` rendering); the stream carries all beats for the
*current* turn only, and history is beat-less (beats are not persisted).

### `stream_ask` wiring

```rust
pub async fn stream_ask(&self, params: ActionParams, event_tx: UnboundedSender<Envelope>) {
    // … existing arg parsing & thread resolution …

    // Persist user message. thread_messages(thread.id) will now include it.
    self.store.append_message(&thread.id, "user", &text)?;

    // Load history ONCE before the stream loop. It includes the just-appended
    // user message, so we slice off the last entry to avoid double-rendering it
    // as both "history[-1]" and "current_user_turn".
    let full = self.store.thread_messages(&thread.id)?;
    let history: Vec<StoredMessage> = if full.last().is_some_and(|m| m.role == "user" && m.content == text) {
        full[..full.len() - 1].to_vec()
    } else {
        full.clone()
    };

    // … build request, call client.stream() …

    while let Some(ev) = turn_stream.next().await {
        match ev {
            Ok(StreamEvent::TextDelta { text: delta, .. }) => {
                accumulated.push_str(&delta);
                let _ = self.events.send(turn_event(&history, &text, &accumulated, true, ts, &beats));
            }
            Ok(StreamEvent::ToolUse { .. }) => { /* same, pass &history */ }
            Ok(StreamEvent::MessageStop { .. }) => {
                let _ = self.events.send(turn_event(&history, &text, &accumulated, false, ts, &beats));
                break;
            }
            // …
        }
    }

    if !accumulated.is_empty() {
        self.store.append_message(&thread.id, "assistant", &accumulated)?;
    }
}
```

The history vec is computed once per `stream_ask` call. That is acceptable —
beats and the current turn change every delta, but `history` is frozen for
the duration of this turn. For a very long thread this is an O(N) copy per
delta via `extend`-within-`turn_event`; if that shows up in profiles we move
to `Arc<Vec<StoredMessage>>` and have `turn_event` build an `Arc`-aware
component list. **Out of scope for MVP** — measure first.

### Test updates (`plugins/chat/src/plugin.rs` test module)

Two existing tests exercise `turn_event` output shape. Both must be updated:

- The `data_updated_event` roundtrip test (≈ line 1184) — expand to seed
  a 2-message history (user + assistant), call the new `turn_event`, and
  assert component count `= 2 + 2 + 0 = 4` and role ordering
  `["user", "assistant", "user", "assistant"]`.
- The tool-call beat end-to-end test (≈ line 833) — seed history of
  `["user: prev"]`, assert the emitted components start with that prior
  turn before the running-beat check.

Add one new test: **empty-history turn** — history=[], streaming delta
produces `[user_turn, agent_turn]` matching today's shape. Confirms we
haven't regressed the first-turn case.

---

## (c) Chat-event listener — `thread_id` guard via ref + stable deps

### Current code (`App.tsx:511-562`)

The listener effect has `[]` deps (comment acknowledges the stale-closure
risk with `eslint-disable`). It updates `activeThreadId` from the payload
but never *guards* against a late event from a prior thread overwriting
`chatMessages`. The only ref-based guard today is the threads-list refresh
(`threadsRef.current`), which is a different concern.

### Failure mode to close

1. User is on thread A, dispatches `/ask hello`. `stream_ask` starts.
2. User hits ⌘T, selects thread B. `loadThreadMessages('B')` sets
   `chatMessages=[]`, `activeThreadId='B'`, issues `chat_subscribe({threadId:'B'})`
   which atomically replaces the Rust-side subscription.
3. A delta from thread A, in flight before the swap finished on the Rust
   side, reaches the webview. The listener's `payload.thread_id === 'A'`,
   but it still runs `setChatMessages(payload.data)` — thread B's UI is
   stomped with thread A content.

### Fix: `subscribedThreadRef`

Introduce a new ref, updated synchronously anywhere `activeThreadId` is
written, that mirrors the *intent* of the most recent subscription call:

```ts
const subscribedThreadRef = useRef<string | null>(null)

// Single helper, always used — never `setActiveThreadId` directly.
const setActiveThread = useCallback((id: string | null) => {
  subscribedThreadRef.current = id
  setActiveThreadId(id)
}, [])
```

All of these call sites change to `setActiveThread(...)`:

- `App.tsx:461` (`loadThreadMessages`)
- `App.tsx:488` (`handleNewThread`)
- `App.tsx:518` (listener: `if (payload.thread_id) setActiveThread(...)`)

Listener body:

```ts
win.listen<{ thread_id: string | null; event_type: string; data: unknown }>(
  'dust://chat-event',
  ({ payload }) => {
    // Guard: drop payloads whose thread_id doesn't match the currently
    // subscribed thread. A null incoming thread_id is accepted only when
    // we are also in the null-thread state (e.g. new_thread before the
    // first data_updated pins the id).
    const subscribed = subscribedThreadRef.current
    if (payload.thread_id !== null && subscribed !== null
        && payload.thread_id !== subscribed) {
      console.debug('[chat-event] dropped stale payload', {
        payload_thread: payload.thread_id, subscribed,
      })
      return
    }

    // First delta carrying a thread_id pins the thread in the null-state.
    if (payload.thread_id && subscribed === null) {
      setActiveThread(payload.thread_id)
    }

    // … existing data_updated / error handling …
  },
)
```

### Why a ref, not `activeThreadId` in deps

Putting `activeThreadId` in deps would re-attach the Tauri `listen` on every
thread switch — which is the exact race `DESIGN-CHAT-SUBSCRIPTION.md`
prevents by keeping the listen handle stable (§Key invariants #1). The ref
gives us the current-thread read from inside the listener without coupling
subscription lifecycle to renders. Deps stay `[]`; the `eslint-disable`
comment stays.

### Interaction with `fetchThreads` 500ms refresh gate

The current effect also gates a `list_threads` refresh on an unknown
`thread_id` via `threadsRef`. Order of operations inside the listener:

1. Run stale-thread guard (above). If dropped, `return` — the 500ms refresh
   does NOT fire for stale events.
2. If accepted, run the `threadsRef.current.some(...)` check and schedule
   the trailing-edge refresh as today.

This ordering matters: a stale event from thread A would otherwise trigger
a `list_threads` refresh on behalf of thread B's UI, which is merely
wasteful but confusing in logs.

---

## (d) Failure-surfacing strategy for `fetchThreads` / `loadThreadMessages`

### Channel choice: reuse `slashError`

`slashError` is already the dust-shell's in-band error surface — a
terracotta (`#DA7757`) single-line banner rendered at
`App.tsx:768-780`, between the search bar and the body. It dismisses on
the next keystroke (`App.tsx:762`). We **reuse** it for chat-surface
errors instead of introducing a second banner slot.

**Rename** the state variable from `slashError` to `chatError` OR keep the
name for diff-minimality — `implement-frontend` chooses one. Copy spec
below is independent of the variable name.

Rationale vs alternatives:

| Candidate                              | Impl effort | Visibility | Disrupts chat? |
|----------------------------------------|-------------|------------|----------------|
| **Reuse `slashError` banner** *(chosen)* | trivial     | high       | no             |
| Inline placeholder inside ThreadRail    | low         | medium     | no             |
| Toast (new component)                    | medium      | high       | no             |
| Console + no UI                          | zero        | none       | no             |

Toasts are premature — dust has no toast primitive. ThreadRail inline copy
only reaches users who already opened the rail, missing the case where
`fetchThreads` fails silently on first ⌘T and the user closes the rail
thinking there are no threads.

### Failure → copy matrix

| Site                        | Error class                                   | Banner copy                                                    |
|-----------------------------|-----------------------------------------------|----------------------------------------------------------------|
| `fetchThreads`              | IPC error (plugin offline, socket gone)        | `Couldn't load threads: {truncatedMsg}`                         |
| `fetchThreads`              | OK but `data` shape unexpected                 | `Couldn't load threads: malformed response`                     |
| `loadThreadMessages` sub    | `chat_subscribe` rejects                        | `Couldn't open thread: subscription failed ({truncatedMsg})`    |
| `loadThreadMessages` msgs   | `dispatch_action` throws                        | `Couldn't open thread: {truncatedMsg}`                          |
| `loadThreadMessages` msgs   | OK but result not a `StoredMessage[]`           | `Couldn't open thread: malformed response`                      |
| `handleNewThread`           | subscribe or dispatch throws                    | `Couldn't start a new thread: {truncatedMsg}`                   |
| `dispatchSlash` `/ask`      | (already handled, keep existing copy)           | `Failed to dispatch /ask: {msg}`                                |

`truncatedMsg` = first 140 chars of the error's `.message` (or `String(err)`).
Error is always the *root* error, never a wrapped JSON blob.

**ThreadRail "Loading…"** must have a terminal state:

- After a successful `fetchThreads` returning `[]` ⇒ copy becomes
  `No threads yet — press ⌘N to start one.` (not "Loading…").
- After a failed `fetchThreads` ⇒ copy becomes `Couldn't load threads.`
  (shorter than the banner — banner carries the detail).

Implementation:

```ts
type ThreadsStatus = 'idle' | 'loading' | 'ready' | 'error'
const [threadsStatus, setThreadsStatus] = useState<ThreadsStatus>('idle')

const fetchThreadsSafe = useCallback(async () => {
  setThreadsStatus('loading')
  try {
    const list = await fetchThreads()
    applyThreads(list)
    setThreadsStatus('ready')
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err)
    setSlashError(`Couldn't load threads: ${msg.slice(0, 140)}`)
    setThreadsStatus('error')
  }
}, [fetchThreads, applyThreads])
```

All call-sites (`App.tsx:525`, `App.tsx:584`) switch to `fetchThreadsSafe`.
The rail renders copy by `threadsStatus`:

```tsx
{threads.length === 0 ? (
  <p className="px-2.5 py-3 text-[10px]" style={{ color: 'var(--text-secondary)' }}>
    {threadsStatus === 'loading' ? 'Loading…'
      : threadsStatus === 'error' ? "Couldn't load threads."
      : threadsStatus === 'ready' ? 'No threads yet — press ⌘N to start one.'
      : 'Loading…'}
  </p>
) : (…)}
```

---

## (e) `ChatPane` placeholder — two-state model

### Today

```tsx
{messages.length === 0 ? (
  <p>Waiting for response…</p>
) : (<ComponentRenderer …/>)}
```

Wrong because `messages.length === 0` is true in two distinct cases:

1. **No dispatch yet** — user just summoned ChatPane (sticky mode with a
   cleared thread, or first-type before Enter). No request is pending.
   Copy should invite input.
2. **In-flight** — user pressed Enter; `chat_subscribe` + `dispatch_action`
   have been called but the first `data_updated` hasn't arrived. Copy
   should reassure.

### State model

Introduce one new state slice on `App`:

```ts
type DispatchStatus = 'idle' | 'in_flight'
const [dispatchStatus, setDispatchStatus] = useState<DispatchStatus>('idle')
```

Transitions:

| Event                                                  | New value   |
|--------------------------------------------------------|-------------|
| `handleEnter` commits a chat dispatch (slash or sticky) | `in_flight` |
| First `data_updated` event received                    | `idle`      |
| `error` event received                                 | `idle`      |
| Thread switch via `loadThreadMessages`                 | `idle`      |
| `handleNewThread`                                      | `idle`      |
| Dispatch IPC rejects synchronously                     | `idle` (error already banner'd) |

Pass `dispatchStatus` to `ChatPane`:

```tsx
<ChatPane messages={chatMessages} dispatchStatus={dispatchStatus} />
```

Placeholder rendering:

```tsx
function ChatPane({ messages, dispatchStatus }: Props) {
  // …
  <div className="flex-1 overflow-y-auto p-4">
    {messages.length === 0 ? (
      <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
        {dispatchStatus === 'in_flight'
          ? 'Waiting for response…'
          : 'Type a message and press Enter.'}
      </p>
    ) : (
      <ComponentRenderer components={messages} onAction={async () => {}} />
    )}
    <div ref={bottomRef} />
  </div>
}
```

### Why not infer from `chatMessages` alone

`messages.length === 0` is insufficient for the reason above. An alternative
candidate — infer "in_flight" as `chatMessages[-1]?.streaming === true`
(existing streaming marker) — works *during* a stream but fails for the
window between Enter and the first delta, which is the exact copy problem
we're fixing. Rejected.

### Why not a third "error" state

An "error" state was considered and rejected. Errors are already surfaced
via the terracotta text component appended by the listener
(`App.tsx:534-548`). Adding an error placeholder would compete with that
rendering. The two-state model stays lean.

---

## (f) Auto-submit investigation plan

### Suspected symptom

While typing `/ask Xyz`, the chat dispatch fires *before* Enter is pressed —
either consistently (bug) or occasionally (race). The user reports seeing
Claude start responding to a partial prompt.

### Hypothesis surface (rank-ordered by prior probability)

| # | Hypothesis                                                                                     | Prior | How to distinguish                                         |
|---|------------------------------------------------------------------------------------------------|-------|------------------------------------------------------------|
| 1 | **Enter double-fire** — a key listener attached twice dispatches twice; the second one sees the query already cleared and re-reads stale state. | med   | Count `enter-pressed` debug lines per keystroke             |
| 2 | **Safari `<input type="search">` Enter quirk** — on macOS WebView (WKWebView underpins Tauri), `type="search"` inputs fire a native `search` event on Enter in addition to `keydown`. | med   | Check for a `search` event on the input                     |
| 3 | **IME composition commit** — during non-ASCII input, `compositionend` can trigger `keydown` with `key='Enter'` (for confirming the candidate list) that is *not* intended as submit. | low–med | `isComposing`/`compositionstart`/`compositionend` trace      |
| 4 | **Blur double-fire** — onBlur handler somewhere dispatches an action when focus leaves input while focused right before Enter. | low   | Log `blur` events with cause                                |
| 5 | **React strict-mode double-invoke** — effects run twice in dev. If `handleEnter` reads from state-in-closure and we fired it via an effect, the double would hit. (Unlikely — it's a callback, not an effect.) | low   | Check prod build behaviour                                   |
| 6 | **Autocomplete Enter select** — browser autocompletes a history suggestion; selecting the suggestion fires Enter-like keydown. | low   | `autoComplete="off"` already set; reconfirm no suggestion UI|
| 7 | **Perception** — user sees the sticky-mode short-circuit to ChatPane (a) and mistakes it for auto-submit. | high (but fixed by `displayResults` case table anyway) | Repro under the new (a) behaviour; if it goes away, this was it |

### Instrumentation to add

All logs go behind a `DUST_DEBUG_CHAT` URL param OR `window.__DUST_DEBUG__`
flag so we can toggle them on without a rebuild. Default off.

```ts
const DEBUG = typeof window !== 'undefined'
  && (window.location?.search?.includes('debug=chat')
      || (window as any).__DUST_DEBUG__)

// 1. Input onChange — record every keystroke with the new query value and
//    whether we're about to short-circuit search.
onChange={e => {
  const v = e.target.value
  if (DEBUG) console.debug('[dust] onChange', {
    value: v, len: v.length, t: performance.now(),
    slash: parseSlash(v)?.prefix ?? null,
  })
  setQuery(v)
  if (slashError) setSlashError(null)
}}

// 2. onKeyDown — log every key that reaches the handler, with isComposing.
const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
  if (DEBUG) console.debug('[dust] keydown', {
    key: e.key, code: e.code,
    isComposing: (e.nativeEvent as KeyboardEvent).isComposing,
    repeat: e.repeat, t: performance.now(),
  })
  // …existing body…
}, […])

// 3. Enter branch in handleEnter — tag with a per-call id.
const handleEnter = useCallback(() => {
  const callId = Math.random().toString(36).slice(2, 8)
  if (DEBUG) console.debug('[dust] handleEnter START', {
    callId, query, isChatActive, slash: parseSlash(query)?.prefix ?? null,
    t: performance.now(),
  })
  // …branch…
  if (DEBUG) console.debug('[dust] handleEnter DISPATCH', {
    callId, path: 'slash|sticky|ask|match',
  })
}, […])

// 4. Input-level native listeners for the three exotic cases.
useEffect(() => {
  if (!DEBUG) return
  const el = inputRef.current
  if (!el) return
  const onSearch = (e: Event) => console.debug('[dust] input.search', {
    value: (e.target as HTMLInputElement).value, t: performance.now(),
  })
  const onCompStart = (e: CompositionEvent) => console.debug('[dust] compositionstart', { data: e.data })
  const onCompEnd = (e: CompositionEvent) => console.debug('[dust] compositionend', { data: e.data })
  const onBlur = (e: FocusEvent) => console.debug('[dust] blur', { relatedTarget: e.relatedTarget })
  el.addEventListener('search', onSearch)
  el.addEventListener('compositionstart', onCompStart)
  el.addEventListener('compositionend', onCompEnd)
  el.addEventListener('blur', onBlur)
  return () => {
    el.removeEventListener('search', onSearch)
    el.removeEventListener('compositionstart', onCompStart)
    el.removeEventListener('compositionend', onCompEnd)
    el.removeEventListener('blur', onBlur)
  }
}, [])
```

Server-side, add one targeted `eprintln!` at the top of `stream_ask`
(already exists) **behind `CHAT_DEBUG=1`** (see §Cleanup below) so we can
confirm whether the backend saw one request or two:

```rust
if std::env::var_os("CHAT_DEBUG").is_some() {
    eprintln!("chat: stream_ask ENTER call_id={} thread_id_arg={} text={:?}",
              uuid::Uuid::new_v4(), thread_id_arg, text);
}
```

`call_id` is a fresh UUID per invocation. Pair it with the React `callId`
via timing to prove 1:1 or 1:many.

### Reproduction steps

Run the dust shell with `DUST_DEBUG_CHAT=1` and `CHAT_DEBUG=1` exported:

```
CHAT_DEBUG=1 cargo run -p dust-tauri --release
# in webview devtools:
window.__DUST_DEBUG__ = true
```

**Scenario A — baseline typing:**
1. Summon window (⌥Space).
2. Type `/ask one` keystroke at a time.
3. Do NOT press Enter.
4. Verify: **zero** `[dust] handleEnter DISPATCH` lines, **zero**
   `chat: stream_ask ENTER` lines.
5. Press Enter once.
6. Verify: **one** `handleEnter DISPATCH`, **one** `stream_ask ENTER`.

**Scenario B — rapid Enter:**
1. Type `/ask hello`.
2. Press Enter, hold for 300ms (allowing OS key-repeat).
3. Verify: `e.repeat === true` on subsequent keydowns; handler should
   still only dispatch once (since query clears after first dispatch).

**Scenario C — IME:**
1. Switch to a CJK IME (Pinyin).
2. Type romaji that would trigger a candidate window; commit with Enter.
3. Verify: `isComposing === true` on the first Enter (IME commit);
   handler MUST ignore it (add `if ((e.nativeEvent as KeyboardEvent).isComposing) return`
   at the top of the keydown handler — DECISION-safe guard).
4. Press Enter again (no composition active).
5. Verify: dispatch fires exactly once.

**Scenario D — Safari `type="search"`:**
1. Type `hi` and press Enter.
2. Verify: `[dust] input.search` appears exactly once; this does NOT
   trigger a second dispatch because it doesn't go through our keydown path.
   If it does, switch the input to `type="text"` (no dust-visible
   behaviour changes — the magnifier icon is a separate SVG).

### Known false-positive causes to rule out

- **Double-fire on blur.** Not applicable — no blur handler dispatches an
  action in `App.tsx`; `onFocusChanged` only hides the window. But confirm
  by scenario-D logs.
- **IME composition.** The `isComposing` guard above is the accepted
  mitigation; it's a one-line defensive change regardless of whether the
  auto-submit is reproducible.
- **Safari `type="search"` Enter.** The `<input>` has `type="search"`
  (`App.tsx:887`). Safari/WebKit fires a `search` event when Enter is
  pressed on an empty value or after a debounce. It does not fire a
  synthetic `keydown`, so our React handler won't double-run — but the
  native event may clear the input if some other code listens. Confirm
  none does.
- **Perception.** If scenarios A–D all show 1:1 dispatches and the user
  still reports the symptom, it's the `/ask tra` flicker (ChatPane
  unmounting) being mistaken for auto-submit. Fixed by §(a).

### Fix decision tree

After running scenarios:

- **All 1:1** ⇒ auto-submit is not-reproducible. Mark in review doc,
  keep the `isComposing` guard as a defence-in-depth, remove the rest
  of the instrumentation.
- **Scenario C double** ⇒ IME is the culprit; `isComposing` guard fixes it.
- **Scenario A/B double** ⇒ multiple dispatch paths; hunt with `callId`
  correlation — the debug output will name the path that double-fired.
- **`[dust] input.search` precedes `handleEnter`** ⇒ the `search` event
  is indirectly triggering our handler (unlikely given the code path).
  Mitigation: `type="text"`.

The instrumentation and the `isComposing` guard ship as part of the fix
commit; debug code is gated by `window.__DUST_DEBUG__`, the `isComposing`
guard is always-on.

### DECISION: `isComposing` guard is unconditional

```ts
const handleKeyDown = (e: React.KeyboardEvent) => {
  if ((e.nativeEvent as KeyboardEvent).isComposing || e.keyCode === 229) return
  // …existing body…
}
```

The `keyCode === 229` check handles the legacy IME Enter where
`isComposing` is false but the key repeats as "process" keycode 229. It's
the standard React-IME idiom and adds no runtime cost.

---

## Cleanup — eprintln demotion

All chat-plugin `eprintln!` lines added during debugging go behind a
`CHAT_DEBUG` env guard rather than being deleted. Reason: they're useful
for the **next** streaming bug; deleting them just means re-adding them
later. The guard is zero-cost when unset.

Target lines (from `plugins/chat/src/plugin.rs` + `server.rs`):

```
chat: dispatch_request method=…           server.rs:159
chat: is_streaming check …                server.rs:164
chat: stream_ask ENTER …                  plugin.rs:72
chat: stream_ask building request …       plugin.rs:108
chat: stream_ask calling client.stream()  plugin.rs:117
chat: client.stream() OK / FAILED         plugin.rs:120/122
chat: stream event: …                     plugin.rs:133
chat: build_request FAILED …              plugin.rs:112
```

Wrap with a once-computed guard:

```rust
static CHAT_DEBUG: once_cell::sync::Lazy<bool> =
    once_cell::sync::Lazy::new(|| std::env::var_os("CHAT_DEBUG").is_some());

macro_rules! debug_log {
    ($($arg:tt)*) => { if *CHAT_DEBUG { eprintln!($($arg)*); } }
}
```

Then `eprintln!("chat: …")` → `debug_log!("chat: …")`. **Leave in place**
the registry-side stderr → `~/.alluka/logs/plugin-{id}.log` routing — that
pathway is useful beyond this mission and is not diagnostic noise.

---

## Component map — ownership & boundaries

```
┌──────────────────────────────────────────────────────┐
│ chat plugin (Rust)                                   │
│   store.thread_messages(tid) ──┐                     │
│   stream_ask ──────────────────┼──► turn_event ──►   │
│     for ev in client.stream()  │      [history…,     │
│       push user/agent/beats    │       user,         │
│                                │       agent,        │
│                                │       beats…]       │
│                                │                     │
│                                └──► events.send(Env) │
└──────────────────────────────────────────────────────┘
                │ broadcast → Tauri forward task
                ▼
┌──────────────────────────────────────────────────────┐
│ dust React (App.tsx)                                 │
│   query ─┬─► parseSlash ──┐                          │
│          │                ├──► displayResults        │
│   results├───────────────►┤    (case table §a)       │
│   chatMsg│                │                          │
│   actTid ┴───►isChatActive┘                          │
│                                                      │
│   listen('dust://chat-event')                        │
│       ├── subscribedThreadRef guard (§c)             │
│       ├── data_updated → setChatMessages(data)       │
│       ├── error        → append terracotta text      │
│       └── (both)       → setDispatchStatus('idle')   │
│                                                      │
│   fetchThreadsSafe → setSlashError on fail (§d)      │
│   loadThreadMessagesSafe → setSlashError on fail     │
│                                                      │
│   ChatPane(messages, dispatchStatus) → §e            │
└──────────────────────────────────────────────────────┘
```

Boundary rule: React never reaches into the chat plugin's turn semantics
(history assembly is server-side). The server never assumes a particular
React render strategy (replace vs append — replace is the contract).

---

## Risks

1. **History copy cost per delta.** `turn_event` clones the full history
   vec every delta. For a 200-message thread at ~200 bytes/message that's
   ~40 KB per delta × ~50 deltas/turn = ~2 MB of transient churn per turn.
   Acceptable today (threads in practice are <30 messages). Mitigation if
   it matters later: `Arc<[StoredMessage]>` + component-by-ref builder.
2. **Sticky mode hijacks one-off search.** If the user is in a chat
   thread and types a literal plugin query (e.g., `tracker`), the case
   table routes it to chat. Accepted trade-off: `/tracker` slash is the
   explicit escape hatch, and the user can clear the thread with ⌘N
   (which sets `isChatActive=false` after the fresh thread's
   `chatMessages=[]`, `activeThreadId=null`).
3. **Stale `subscribedThreadRef`.** Forgetting to go through
   `setActiveThread` at any new call site would reintroduce the race.
   Mitigation: search/grep for `setActiveThreadId(` after the refactor
   and assert every usage is inside `setActiveThread`.
4. **IME guard blocks legitimate Enter** in rare IMEs where `isComposing`
   stays true across Enter. Mitigation: the `keyCode === 229` check is
   orthogonal and catches the older path. If a real user hits this, they
   can press Enter twice; we wait for a complaint before adding more
   guards.
5. **`CHAT_DEBUG` accidentally shipped on.** The guard is env-based — no
   accidental logs in a packaged build as long as CI doesn't export
   `CHAT_DEBUG`. Mitigation: document in `scripts/nanika-update.sh` header.
6. **`ThreadsStatus` regression for ⌘T toggle.** If ⌘T toggles visibility
   without re-fetching and the last status is `error`, the user sees
   `Couldn't load threads.` forever. Mitigation: ⌘T on an `error` state
   re-runs `fetchThreadsSafe` (add one extra branch to the existing
   conditional at `App.tsx:582-587`).

---

## Trade-offs accepted

- **Sticky mode is thread-scoped, not global.** ⌘N clears sticky; a
  non-chat plugin Enter (case 7 with results) clears it only if it
  unmounts ChatPane (i.e., `isAskClaude=false`). Cross-thread behaviour
  is intentional: running `/tracker create foo` from inside a chat
  doesn't drop the user out of the chat — they hit ⌘T or Esc to leave.
- **No persistence of `dispatchStatus` across unmount.** If ChatPane
  unmounts while in_flight (user Escapes away), the status resets to
  `idle` on remount. The stream keeps running on the Rust side; the
  first delta on re-subscribe flips the placeholder. Tiny visual
  flicker possible but not worth persistence plumbing.
- **Banner reuse over toast.** We lose independent dismissal of chat
  errors vs slash errors (keystroke clears both), but save the design
  cost of introducing a second error primitive today. Revisit if a
  mission requires simultaneous surfacing.
- **O(N) history clone in `turn_event`.** See Risk #1.

---

## Reviewer checklist

Each line is a discrete behaviour the review phase runs live and records
as pass/fail in `plugins/dust/DESIGN-REVIEW-CHAT-UX.md`.

### Slash / sticky routing (§a)

- [ ] `query = ""` with empty chatMessages and null thread ⇒ ResultsList
      shows "No capabilities found" or the populated result set; ChatPane
      not mounted.
- [ ] Type `/` alone ⇒ no crash, no flicker; the bare-`/` goes through
      the existing empty-search fallback.
- [ ] Type `/ask tra` one char at a time ⇒ ChatPane stays mounted
      throughout; ResultsList never renders.
- [ ] Type `/tracker create foo` ⇒ same — ChatPane stays mounted during
      typing (the slash still dispatches tracker on Enter).
- [ ] In an active chat thread, clear the query — ChatPane remains
      mounted (Case 2).
- [ ] Dispatch `/ask hello`, receive a response; type `follow up`
      (no slash) and Enter — dispatches as chat, prior turns stay visible.
- [ ] `search_capabilities` is NOT invoked during slash typing (confirm
      in network/IPC trace or `[bench] results-updated` console lines).

### Server turn_event (§b)

- [ ] After a `/ask` followed by `follow up`, `chatMessages` contains
      4 `agent_turn` components in role order `user|assistant|user|assistant`
      throughout the second stream (plus any beats at the tail).
- [ ] First user turn in a fresh thread emits only
      `[user, agent, beats…]` (no spurious empty-history entries).
- [ ] `streaming: true` on exactly one agent turn per delta; flips to
      `false` on the last delta of the turn (MessageStop).
- [ ] `cargo test -p chat` green, including the two updated tests and
      the new empty-history test.

### Stale-thread guard (§c)

- [ ] Toggle between two threads rapidly (⌘T + arrows) while a stream
      is in flight on the first thread; the second thread's UI never
      shows the first thread's text. (Trace via temporary
      `[chat-event] dropped stale payload` log; remove after verify.)
- [ ] No regression: single-thread streaming still updates the UI
      normally (payload with `thread_id === subscribed` is not dropped).

### Failure surfacing (§d)

- [ ] Kill the chat plugin process mid-run. Press ⌘T. Banner appears
      within ~1s reading `Couldn't load threads: …`. ThreadRail
      copy reads `Couldn't load threads.`
- [ ] Restart chat. ⌘T again ⇒ threads reappear; banner clears on next
      keystroke.
- [ ] Click a thread while chat is down ⇒ banner reads
      `Couldn't open thread: …`; `activeThreadId` reverts to the prior
      value (or stays null if first click).

### Placeholder two-state (§e)

- [ ] Summon window on an empty chat state ⇒ ChatPane copy reads
      `Type a message and press Enter.`
- [ ] Press Enter on a non-empty query ⇒ copy flips to
      `Waiting for response…` until the first delta.
- [ ] First delta arrives ⇒ copy is replaced by rendered components.
- [ ] ⌘N on a streaming thread ⇒ copy returns to
      `Type a message and press Enter.` (dispatchStatus back to idle).

### Auto-submit investigation (§f)

- [ ] With `DUST_DEBUG_CHAT=1 CHAT_DEBUG=1`, Scenario A logs zero
      dispatches while typing `/ask one` and exactly one on Enter.
- [ ] Scenario B (held Enter) dispatches exactly once.
- [ ] Scenario C (IME) — dispatch not fired on the IME commit Enter.
- [ ] Scenario D (Safari) — `input.search` log does not correlate
      with a second `handleEnter` call.
- [ ] `isComposing`/`keyCode === 229` guard is present at the top of
      `handleKeyDown` regardless of whether auto-submit reproduced.

### Cleanup

- [ ] Default run (`CHAT_DEBUG` unset, no `__DUST_DEBUG__`) ⇒ no
      `chat: …` eprintln lines in stderr during a full turn.
- [ ] `CHAT_DEBUG=1` run ⇒ lines present.
- [ ] Bundle size delta ≤ +3 KB gzipped vs 153.64 KB baseline
      (`npm run build` output).
- [ ] `cargo test -p chat -p dust-core -p dust-tauri` green.
- [ ] `cd plugins/dust && npm run build && npm test` green.
- [ ] `tracker update TRK-584 --status done` recorded in review.

---

<!-- scratch -->
Downstream phases should note:

1. `turn_event` signature change is load-bearing — implement-backend must
   update both call-sites (streaming loop + test fixtures). The history
   dedup step (strip the just-appended user message if it matches) is a
   deliberate choice — without it the React side sees the current user
   turn twice.
2. Frontend must introduce `setActiveThread` helper and route ALL
   `activeThreadId` writes through it — otherwise the stale-thread guard
   silently fails.
3. `dispatchStatus` state lives on `App`, not inside `ChatPane` — a
   `ChatPane`-local state would reset on unmount/remount (e.g., during
   `isAskClaude` flicker) and break the "Waiting for response…" copy.
4. The `isComposing` + `keyCode === 229` guard is defensive regardless of
   whether auto-submit reproduces. Ship it.
5. Banner reuses `slashError` for diff-minimality; implement-frontend may
   rename to `chatError` — review notes either choice, just be consistent.
6. `CHAT_DEBUG` env guard: the suggested `once_cell::Lazy` pattern keeps
   the env lookup off the hot path. If `once_cell` is not already a dep
   of the chat plugin (check `plugins/chat/Cargo.toml`), an
   `AtomicBool` init-on-first-read is equally fine.
<!-- /scratch -->

LEARNING: two-state placeholder + sticky predicate is a small state-model
addition that closes three user-visible defects (placeholder copy, slash
flicker, follow-up prefix) with one shared predicate (`isChatActive`).

PATTERN: "ref-mirror of the most-recent write" is the right guard for
subscription handlers whose effect must keep stable deps — applies
here (`subscribedThreadRef`) and generalises to any single-slot
subscription.

DECISION: banner reuse over new toast primitive — saves design cost today;
revisit only when two independent error streams need simultaneous surfacing.

DECISION: `isComposing` guard shipped unconditionally even if auto-submit
proves not-reproducible — cost-free defence against CJK users.
