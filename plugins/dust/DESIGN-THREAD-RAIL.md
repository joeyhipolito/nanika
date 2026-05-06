---
produced_by: architect
phase: phase-1
workspace: 20260421-70b515e6
created_at: "2026-04-21T00:00:00Z"
confidence: high
depends_on: []
token_estimate: 2600
---

# DESIGN — Tauri Shell Chat Thread Rail (TRK-569)

## 1. Context

The Tauri shell's chat entry (Ask-Claude fallback + `/ask`) has no thread browser. Users can only continue the *current* thread or start a new one via `⌘N`. Every prior conversation is effectively inaccessible from the Tauri surface.

The TUI already solves this with `Ctrl+T` (`plugins/dust/dust-dashboard/src/app.rs` — `chat_threads`, `chat_thread_cursor`, `chat_show_threads`, `load_chat_messages`). Copy the *pattern*, not the code: React + existing Tauri IPC, no new backend surface.

All three chat-plugin actions already exist (`plugins/chat/src/plugin.rs:494-531`):

- `new_thread` → `{id, title, created_at, updated_at}`
- `list_threads` → `Thread[]` (ordered `updated_at DESC`)
- `list_messages(thread_id)` → `StoredMessage[]`

## 2. Decision

Add a **collapsible 180 px left-hand thread rail** that is mutually exclusive with the existing 280 px results column, toggled by `⌘T`, visible only while the right pane is rendering `chatMessages` (`isAskClaude === true`).

### Candidates Considered

| Approach | Width | Mutex with results | Verdict |
|---|---|---|---|
| A. Rail *replaces* results column (chosen) | 180 px | Yes | Preserves 820 px total; chat pane grows from 540 → 640 px, which chat benefits from. Zero overflow risk. |
| B. Rail overlays as a floating drawer | 180 px float | No | Z-index juggling, animation cost, covers chat content. Over-engineered for solo maintainer. |
| C. Rail stacks *beside* results (three columns) | 180 + 280 + pane | No | 820 − 460 = 360 px chat pane — cramped. Violates mission constraint "rail replaces results column when chat is active, not added on top". |

**Chosen: A.** Primary reason: preserves the 820 px frame, keeps layout math trivial (two states, not three), matches the mission's explicit mutex constraint. What we give up: the user can't browse results *and* browse threads simultaneously — acceptable because threads are only meaningful in chat mode, and results are only meaningful pre-chat.

## 3. Component Map

```
App (root)
├── SearchBar                         (52 px, unchanged)
├── SlashError banner                 (optional, unchanged)
└── Body (flex row, 440 px)
    │
    │  ── when !isAskClaude (non-chat) ───────────────
    ├── ResultsList          (280 px, existing)
    └── DetailPane           (540 px, existing)
    │
    │  ── when  isAskClaude && !threadsVisible ──────
    └── ChatPane             (820 px, full-width)
    │
    │  ── when  isAskClaude &&  threadsVisible ──────
    ├── ThreadRail  ★NEW    (180 px)
    └── ChatPane             (640 px)

★ ThreadRail
├── RailHeader              ( + new-thread button, refresh indicator)
└── ThreadRow[]             (title, ago-timestamp, terracotta highlight if active)
```

Borders: 1 px `var(--border)` right-edge on ThreadRail (mirrors existing `borderRight` on ResultsList:732). No other layout borders change.

### 3.1 Rendering Guard

```tsx
const showResultsColumn = !isAskClaude
const showThreadRail    = isAskClaude && threadsVisible
// invariant: showResultsColumn && showThreadRail is ALWAYS false
```

This invariant is the review acceptance criterion for "mutually exclusive".

## 4. Interfaces

### 4.1 State Shape (additions to `App()` in `plugins/dust/src/App.tsx:107`)

```ts
type ThreadMeta = {
  id: string
  title: string
  created_at: number   // ms epoch (matches Rust Thread.updated_at i64 in ms)
  updated_at: number
}

const [threads, setThreads]             = useState<ThreadMeta[]>([])
const [threadsVisible, setThreadsVisible] = useState(false)      // hidden by default
const [activeThreadId, setActiveThreadId] = useState<string | null>(null)
const [threadCursor, setThreadCursor]   = useState(0)            // keyboard-select index
```

**Replace `currentThreadId` with `activeThreadId`.** They name the same concept; having two is a bug vector. Rename at the call sites: `:112, :191, :196, :248, :256, :415` (set to `null` in `⌘N`). The chat-event listener (`:371`) also writes this slot.

Refs needed:

```ts
const refreshTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
const threadsRef      = useRef<ThreadMeta[]>([])  // for stable closure in event handler
```

`threadsRef` mirrors `threads` so the `dust://chat-event` listener (which is mounted with `[]` deps) can read the current list without resubscribing on every thread change.

### 4.2 Data-Loading Primitives

```ts
// Populate `threads` from the chat plugin. Caller sets `activeThreadId` if desired.
async function fetchThreads(): Promise<ThreadMeta[]> {
  const res = await invoke<ThreadMeta[]>('dispatch_action', {
    pluginId: 'chat', capabilityId: 'ask',
    actionId: 'list_threads', params: {},
  })
  // NOTE: `dispatch_action` returns ActionResult; unwrap .data before use.
  // If the existing invoke<…>('dispatch_action') returns the outer envelope,
  // implement a tiny `unwrapData<T>(result): T` helper rather than duplicating casts.
  return res ?? []
}

// Load a thread's messages and swap them into `chatMessages`.
async function loadThreadMessages(threadId: string): Promise<void> {
  setChatMessages([])                               // clear first — no flash of prior
  setActiveThreadId(threadId)
  await invoke('chat_subscribe', { threadId })      // re-subscribe BEFORE dispatch
  const res = await invoke<Component[]>('dispatch_action', {
    pluginId: 'chat', capabilityId: 'ask',
    actionId: 'list_messages', params: { thread_id: threadId },
  })
  setChatMessages(res ?? [])
}
```

**Ordering is load-bearing**: (1) `setChatMessages([])` before dispatch prevents the "flash of prior thread". (2) `chat_subscribe` before `list_messages` matches the invariant already enforced in `handleEnter` at `:250` — never miss the first delta. (3) `setActiveThreadId` before the await so re-renders see the new selection immediately.

### 4.3 Action Hooks

| Trigger | Handler |
|---|---|
| Rail row click | `onClick={() => loadThreadMessages(t.id)}` |
| `↑` / `↓` inside rail | `setThreadCursor(c => clamp(c ± 1, 0, threads.length - 1))` |
| `Enter` inside rail | `loadThreadMessages(threads[threadCursor].id)` |
| `+` button | `handleNewThread()` — identical to existing `⌘N` block at `:412-427`; extract to callback so both sites share it. |
| `Esc` while rail visible | `setThreadsVisible(false)` — intercept before the existing `Esc` chain in `handleKeyDown`. |

### 4.4 `⌘T` Keymap Integration

Add to the global `document.addEventListener('keydown', onKey)` effect at `:410-451`, alongside `⌘N` and `⌘E`:

```ts
if (e.metaKey && e.key === 't') {
  e.preventDefault()
  // No-op when chat pane isn't active — flashing open an empty rail is confusing.
  if (!isAskClaudeRef.current) return
  setThreadsVisible(v => {
    const next = !v
    if (next && threadsRef.current.length === 0) fetchThreads().then(setThreads)
    return next
  })
  return
}
```

Requires a new `isAskClaudeRef` (mirrors `isAskClaude`, updated in a `useEffect([isAskClaude])`) because the keydown listener is mounted once with `[transitionTo]` deps. Do not widen the deps — that would rebind every result-selection change.

**Collision audit:**
- Existing global metas: `⌘N`, `⌘E`. `⌘T` is free.
- Local (SearchBar onKeyDown): `⌃K`, `↑`, `↓`, `Enter`, `Esc`. No collision.
- Browser defaults: `⌘T` opens a new tab in Safari/Chrome — not applicable inside a Tauri webview (no tab surface), confirmed by existing use of `⌘N` which has the same "new window" default.

## 5. Layout Math

Window is 820 × 520, transparent frameless alwaysOnTop (`App.tsx:5`).

| Vertical slice | Height |
|---|---|
| SearchBar | 52 px |
| SlashError banner | 0 or ~22 px |
| Body (flex-1) | 440 px |
| ActionBar | 28 px |

Body horizontal math (the only thing changing):

| Mode | Left column | Right column | Total |
|---|---|---|---|
| Non-chat (unchanged) | ResultsList **280 px** | DetailPane **540 px** | 820 |
| Chat, rail hidden | — | ChatPane **820 px** (full-width) | 820 |
| Chat, rail visible | ThreadRail **180 px** | ChatPane **640 px** | 820 |

Implementation: conditionally render the left column. ResultsList already uses `w-[280px] shrink-0`; ThreadRail uses `w-[180px] shrink-0`. ChatPane uses `flex-1` (existing) — it auto-fills whatever remains. No width animation; instant snap is consistent with ⌘E transitions.

**Why 180 px?** Mission says "~180 px". Rationale: enough for a 14-char truncated title + 6-char ago timestamp (`yesterday` is longest common value at 9 chars; truncate title to ~18 chars to leave a 2 px gap). 160 px is too cramped for `3 days ago`; 200 px steals too much from the chat column.

## 6. Rail Visuals (Intaglio)

### 6.1 Header row

- Height: 32 px. Background: `var(--bg-elevated)`. Border-bottom: 1 px `var(--border)`.
- Label: `THREADS` (10 px uppercase, tracking-widest, `var(--text-secondary)`) — matches the ChatPane header at `:942-949`.
- `+` button (right-aligned, 18 × 18): `color: var(--text-secondary)`; hover `var(--text-primary)`; `aria-label="New thread (⌘N)"`.

### 6.2 Thread row

- Height: 40 px, `padding: 6px 10px`, stacked title above timestamp (2 px gap).
- Title: 12 px, `var(--text-primary)`, `font-weight: 500`, single-line truncate (`overflow: hidden; text-overflow: ellipsis; white-space: nowrap`). Untitled threads (`title === "New Conversation"`) render in italic.
- Timestamp: 10 px, `var(--text-secondary)`, `font-variant-numeric: tabular-nums`.
- Hover: `background: var(--hover-bg)`.
- Keyboard-selected row (`i === threadCursor` AND `threadsVisible`): `background: var(--selected-bg)`.
- Active thread (`t.id === activeThreadId`): **left border 2 px terracotta** (`#DA7757`, already exported as `TERRACOTTA` at `:101`) — title inherits `color: var(--text-primary)` but becomes 600-weight. Survives hover/selection states; a thread can be simultaneously keyboard-selected and active (user is about to reload the thread they're already on).
- Row separator: `border-bottom: 1 px var(--border)` on all but the last.

### 6.3 Ago-timestamp format

Single pure function, pure JS, no `Intl.RelativeTimeFormat` (bundle size):

```ts
function agoLabel(updatedMs: number, now = Date.now()): string {
  const s = Math.max(0, Math.floor((now - updatedMs) / 1000))
  if (s < 60)        return 'just now'
  if (s < 3600)      return `${Math.floor(s/60)}m ago`
  if (s < 86400)     return `${Math.floor(s/3600)}h ago`
  if (s < 2 * 86400) return 'yesterday'
  if (s < 7 * 86400) return `${Math.floor(s/86400)}d ago`
  // Fallback: MMM D (e.g. "Apr 3"). Do not year-adjust — within-year is enough for UX.
  const d = new Date(updatedMs)
  return d.toLocaleDateString('en-US', { month: 'short', day: 'numeric' })
}
```

Re-render cadence: accept staleness. Only recompute on re-render (i.e. on state changes). Do not set up a `setInterval` tick — 60 s of staleness on an idle pane is not worth the wakeup cost.

## 7. Event Listener — `data_updated` Refresh

The existing listener at `:364-402` already handles `data_updated` for `chatMessages`. Extend the same handler (no new listener, no new effect) with a debounced `list_threads` refresh:

```ts
if (payload.event_type === 'data_updated') {
  setChatMessages(payload.data as Component[])

  // Detect "thread we don't know about" — triggers a rail refresh.
  const newTid = payload.thread_id
  const known = threadsRef.current.some(t => t.id === newTid)
  if (newTid && !known) {
    if (refreshTimerRef.current) clearTimeout(refreshTimerRef.current)
    refreshTimerRef.current = setTimeout(() => {
      fetchThreads().then(list => {
        setThreads(list)
        threadsRef.current = list
      })
      refreshTimerRef.current = null
    }, 500)
  }
}
```

**Why this debounce shape:** the first unknown thread_id schedules a refresh; subsequent `data_updated` events within 500 ms are *known* (the thread is already in the pending fetch) and skip. If a new *different* thread_id arrives while the timer is live, the `clearTimeout` resets the 500 ms window — a single trailing fetch covers both. This is cheaper than leading-edge debounce and matches the TUI's refresh-on-new-thread behavior.

**Cleanup**: clear `refreshTimerRef` in the effect's return (alongside `unlisten?.()` and `chat_unsubscribe`).

## 8. Keyboard Routing Table

Routing reflects focus: the SearchBar input is always focused. But `isAskClaude && threadsVisible` re-purposes the arrow keys because ResultsList isn't rendered.

| Key | `!isAskClaude` | `isAskClaude && !threadsVisible` | `isAskClaude && threadsVisible` |
|---|---|---|---|
| `↑` / `↓` | move `selectedIndex` (results) | no-op | move `threadCursor` (rail) |
| `Enter` | open result / dispatch slash / Ask Claude | same | `loadThreadMessages(threads[threadCursor].id)` |
| `Esc` | palette → detail → clear+hide (existing) | existing chain | **close rail first** → then existing chain |
| `⌘T` | no-op (see note) | show rail (fetch if empty) | hide rail |
| `⌘N` | (no-op — no chat active) | new thread | new thread + close rail? **No — keep rail open, just empty the selection; user likely still wants context.** |
| `⌘E`, `⌃K`, `⌘K` | existing | existing | existing |

Note: `⌘T` with no active chat is a no-op (not an error) because the rail has no meaning without chat. This matches the mission's "only meaningful when chat pane is active" constraint.

Implementation lives inside the existing `handleKeyDown` (SearchBar input) for arrow/Enter/Esc arbitration, and inside the global `onKey` listener for `⌘T`. Do not split into a new component's event handler — the input-focus contract (search input is always focused) means all routing must go through the input's `onKeyDown`.

## 9. Risks

1. **`dispatch_action` return shape drift.** The existing code assumes invoke returns `Component[]` directly for `render_ui` and `CapabilityMatch[]` for `search_capabilities`, but `dispatch_action` historically returns an `ActionResult` envelope. If the existing wiring auto-unwraps `.data`, great; if not, `fetchThreads` and `loadThreadMessages` will break in production but succeed in dev. **Mitigation**: implementer must log the raw result for `list_threads` on first run and add an `unwrapData` shim if needed.

2. **`activeThreadId` rename touches 6 call sites.** Forget one and thread continuity breaks for `/ask`, `⌘N`, or the event handler. **Mitigation**: do the rename as the first commit in the implementation phase; grep for `currentThreadId` after to confirm zero matches.

3. **`threadsRef` drift.** If the implementer forgets to keep `threadsRef.current = list` in sync with `setThreads(list)`, the event-handler "known thread" check will false-positive and spam `list_threads`. **Mitigation**: always update both in the same statement; or wrap in a helper `applyThreads(list)` that does both.

4. **First-fetch timing.** If a user opens Ask-Claude and immediately hits `⌘T`, the rail opens with an empty list. The fetch starts in the same tick but the UI shows "no threads" briefly. **Mitigation**: accepted — show a 9 px `var(--text-secondary)` "Loading…" label when `threads.length === 0 && threadsVisible`; swap in rows on resolution. Alternative (rejected): kick off `fetchThreads` on *every* Ask-Claude activation — wastes bandwidth for users who never open the rail.

5. **Thread title empty / duplicate.** `new_thread` defaults title to `"New Conversation"` (`plugins/chat/src/plugin.rs:499`). A fresh user will have N identical titles. Distinguishing them is the *timestamp's* job — rail is usable, just ugly. **Mitigation**: none in this mission. Title-generation-on-first-turn is out of scope (not mentioned in TRK-569).

6. **Bundle size.** No new deps, but `agoLabel` + rail JSX + new handlers add ~2 KB raw. Mission budget is +5 KB gzipped. Should fit.

## 10. Trade-offs Accepted

- **Static timestamps.** `agoLabel` doesn't tick. Users who leave the pane open for 2 h see stale labels. Acceptable: `alwaysOnTop` chat panes are not long-lived; next re-render refreshes all rows.
- **No scroll-into-view for the active thread.** If the user has 50 threads and `activeThreadId` is at position 30, opening the rail scrolls to top. ResultsList solves this via `scrollIntoView` at `:719-724` — the implementer **should** copy that pattern if trivial, but it's not strictly required. Listed as nice-to-have.
- **No multi-select / drag reorder / delete.** Pure browse + switch + create. Matches the TUI's scope exactly.
- **Rail can't be resized.** Fixed 180 px. A resize handle would be 4 hours of work and a solo-maintainer liability. Revisit when a second user asks.
- **No thread rename.** Titles are what the plugin set them to. If the user wants to rename, they'll ask for it as TRK-5xx and we'll add an inline edit on double-click.

<!-- scratch -->
Implementation priorities:

1. Rename `currentThreadId` → `activeThreadId` FIRST as an isolated commit (`App.tsx:112, 191, 196, 248, 256, 371, 415`). Grep after to confirm zero matches.

2. Add state slices: `threads, threadsVisible (false), activeThreadId (null), threadCursor (0)`. Add refs: `refreshTimerRef`, `threadsRef`, `isAskClaudeRef`.

3. Extract `handleNewThread` callback from the existing `⌘N` block — reused by rail `+` button.

4. `fetchThreads` + `loadThreadMessages` — check that `invoke('dispatch_action', …)` return shape auto-unwraps `.data`; if not, add an `unwrapData<T>(res): T` helper. Log raw result on first call.

5. Extend the existing `dust://chat-event` listener (not a new effect) with the 500 ms debounced refresh. Update both `setThreads(list)` and `threadsRef.current = list` in lockstep — wrap in `applyThreads(list)` if that's clearer.

6. `⌘T` block in the global keymap listener; needs `isAskClaudeRef` (mirror via tiny `useEffect([isAskClaude])`). Do NOT widen the listener's deps — rebinding on every result-selection kills ergonomics.

7. Arrow/Enter/Esc routing inside `handleKeyDown` (input's onKeyDown) — branch on `isAskClaude && threadsVisible`. Esc from rail closes rail *before* the existing detail-close / query-clear chain.

8. Layout: gate ResultsList behind `!isAskClaude`; render ThreadRail only when `isAskClaude && threadsVisible`. The invariant `!(showResultsColumn && showThreadRail)` is the review checkbox.

9. Build the rail as a component in `App.tsx` (don't split to a new file — under 80 lines of JSX). If it grows past 120 lines, extract to `ThreadRail.tsx` at that point.

10. Test loop: `cargo test -p chat`, `npm run build`, then live smoke — open chat, ⌘T, arrow-down, Enter, ⌘N, Esc — confirm no flash of prior messages at step 4.

GOTCHA: the `activeThreadId` rename will break `handleEnter` at `:248` if missed — that's the one that gates "continue current thread" vs "start new thread" for non-slash Ask-Claude dispatch. Verify by asking Claude twice in a row and confirming both turns land in the same thread.

DECISION: rail replaces (not overlays / not stacks beside) the results column — trade-off accepted because results and threads are both left-column, both "list of jumpable things", and never both meaningful at the same time.
<!-- /scratch -->
