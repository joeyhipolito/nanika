---
produced_by: architect
phase: phase-1
workspace: 20260420-93bb0317
created_at: "2026-04-20T09:10:00Z"
confidence: high
depends_on: []
token_estimate: 3600
---

# DESIGN-CHAT-SUBSCRIPTION

Live streaming of chat-plugin events from the Tauri host into the dust React
shell. Scope: the minimum wiring needed so that when the user presses ↵ on an
**Ask Claude** result, subsequent `data_updated` deltas from the chat plugin
flow into a dedicated UI slice and render as they arrive — with a clean
re-subscribe path when the thread changes.

## Context

The dust shell already has `dispatch_action` to *fire* a chat request (see
`plugins/dust/src/App.tsx:162`), and `dust-registry` already exposes
`Registry::connect_and_subscribe` (see `plugins/dust/dust-registry/src/lib.rs:770`)
that yields a replay snapshot plus a `tokio::sync::broadcast::Receiver<EventEnvelope>`
for live events. What is missing is:

1. A Tauri command surface that holds the subscription state on the Rust side
   (neither the `State<AppState>` struct nor the invoke handler registration
   in `plugins/dust/src-tauri/src/lib.rs:55` wire up any subscription lifecycle).
2. A well-defined frontend event payload — the ring carries `EventEnvelope`
   which is wider than what the UI needs (`type`, `sequence`, `id`, `ts`, `data`).
3. A deterministic React subscription lifecycle so re-subscribing on a new
   `thread_id` does not drop mid-stream deltas already in flight on the old
   thread.
4. A state slice next to the existing `detail: Component[]` that renders chat
   deltas **without fighting the detail swap** performed by Enter+`render_ui`.

## Decision

Introduce **one Tauri-managed chat subscription** per webview, keyed by the
(optional) `thread_id`, with a single long-lived Tauri event channel name
(`dust://chat-event`). The React side owns one `listen()` handle at module
scope for the lifetime of the component tree; `chat_subscribe` on the Rust
side atomically replaces the broadcast task backing that channel.

### Why not per-subscription event names
Rejected: using `dust://chat-event/<thread_id>` per thread and one
`listen()` per call. It forces the React hook to re-attach on every thread
change, which is where mid-stream deltas get dropped (an event published on
the old name after `unlisten` but before the new `listen` is live is lost —
Tauri events have no replay).

### Why not expose the registry broadcast directly via a stream command
Rejected: Tauri commands can return a `tauri::ipc::Channel<T>` that models
a stream, but (1) it ties the subscription to the lifetime of a single
`invoke` promise which complicates cancel/replace semantics from the React
side, and (2) it bypasses the `AppState`-managed cancellation we need for
`chat_unsubscribe` to be idempotent. We stay on the plain `app.emit` + global
event channel pattern already used for `file_changed`
(`plugins/dust/src-tauri/src/lib.rs:221`) and `dust://hide-request`
(`plugins/dust/src-tauri/src/lib.rs:411`).

### Why a single slice, not thread-keyed state
Rejected: `chatMessages: Record<ThreadId, Component[]>`. dust only shows one
active thread at a time; re-subscribe is the thread switch. A keyed map forces
eviction logic the UI never reads. If multi-thread history lands later, the
slice becomes the "current thread" view and the archive moves to the chat
plugin itself — a problem for a later design doc.

## Candidates Considered

| Candidate                      | Impl effort | Op complexity | Drops mid-stream? | Fits existing code |
|-------------------------------|-------------|---------------|--------------------|--------------------|
| **A. Global event + managed state** *(chosen)* | low | low | no | yes — mirrors `file_changed` |
| B. Per-thread event names      | low         | low           | yes (unlisten race) | partial           |
| C. `ipc::Channel<T>` stream    | medium      | medium        | no                 | requires refactor  |
| D. SSE over localhost HTTP     | high        | high          | no                 | no — new transport |

Scoring axes chosen because the failure mode that matters most is "deltas get
lost during a re-subscribe" — everything else is fungible.

## Component Map

```
┌───────────────────────────────┐
│  chat plugin (subprocess)     │
└──────────────┬────────────────┘
               │ event envelopes (data_updated, error, …)
               ▼
┌───────────────────────────────┐
│  Registry (dust-registry)     │  broadcast::Sender<EventEnvelope>
│  connect_and_subscribe        │
└──────────────┬────────────────┘
               │ broadcast::Receiver<EventEnvelope>
               ▼
┌───────────────────────────────┐
│  ChatSubscription (Rust)      │  owned by AppState
│  - thread_id: Option<String>  │
│  - handle: JoinHandle<()>     │  (forward loop: recv → app.emit)
│  - subscription_id: String    │  (from registry)
└──────────────┬────────────────┘
               │ app.emit("dust://chat-event", ChatEventPayload)
               ▼
┌───────────────────────────────┐
│  App.tsx                      │
│  - chatMessages: Component[]  │  (new slice)
│  - detail: Component[] | null │  (existing, untouched)
└───────────────────────────────┘
```

Boundary rule: only `ChatSubscription` ever interacts with the registry's
broadcast; React only sees the flattened `ChatEventPayload`.

## Interfaces

### (a) Tauri Command Signatures

```rust
/// Start (or replace) a live subscription to the `chat` plugin's events.
///
/// `thread_id = None` subscribes to the current "no thread yet" channel —
/// the first `data_updated` event carrying a `thread_id` pins the thread
/// for subsequent re-subscribes. Calling `chat_subscribe` when a
/// subscription is already active atomically cancels the previous forward
/// loop before installing the new one (no gap in which events can be
/// missed because the new receiver is created *before* the old JoinHandle
/// is dropped).
#[tauri::command]
async fn chat_subscribe(
    thread_id: Option<String>,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String>;

/// Tear down the current chat subscription, if any. Idempotent — no error
/// when nothing is active. MUST be called from the React unmount path;
/// not wired to window-hide (hide keeps the subscription alive so deltas
/// arriving while the window is hidden are replayed on re-show via the
/// registry's event ring).
#[tauri::command]
async fn chat_unsubscribe(
    state: State<'_, AppState>,
) -> Result<(), String>;
```

Managed state additions in `AppState`:

```rust
pub struct ChatSubscription {
    pub thread_id: Option<String>,
    pub subscription_id: String,
    pub conn_id: dust_registry::ConnectionId,
    pub forward_task: tokio::task::JoinHandle<()>,
}

pub struct AppState {
    pub registry: Arc<Registry>,
    pub chat_sub: tokio::sync::Mutex<Option<ChatSubscription>>,
}
```

Replacement protocol inside `chat_subscribe`:

1. `let guard = state.chat_sub.lock().await;` — serialises overlapping calls.
2. `let handle = registry.connect_and_subscribe("chat", 0).await?` — new live
   receiver exists *before* old task is cancelled.
3. Spawn the new forward task reading from `handle.live_rx`, filtering by
   `thread_id` if `Some`, and calling `app.emit("dust://chat-event", …)`.
4. Swap: `let old = guard.replace(new_sub);`
5. `drop(guard); if let Some(o) = old { o.forward_task.abort(); let _ =
   registry.unsubscribe_plugin_events(…).await; }` — cancel the old task
   **after** releasing the lock, so an in-flight emit from the old task
   that already won the broadcast race still completes. No delta lost.

### (b) Frontend Event Payload

Single event channel: `dust://chat-event`. Payload type:

```ts
// src/types.ts addition
export type ChatEventType = 'data_updated' | 'error'

export type ChatEventPayload = {
  thread_id: string | null
  event_type: ChatEventType
  data: unknown          // shape depends on event_type; see below
}
```

Rust-side projection from `EventEnvelope`:

```rust
#[derive(Serialize)]
pub struct ChatEventPayload {
    pub thread_id: Option<String>,
    pub event_type: String,  // "data_updated" | "error"
    pub data: serde_json::Value,
}
```

Projection rules (Rust → wire):

- `thread_id` is extracted from `envelope.data["thread_id"]` (string) and
  falls back to the subscription's pinned `thread_id`, then `None`.
- Only `EventType::DataUpdated` and `EventType::Error` are forwarded. All
  other envelope types (`progress`, `log`, `status_changed`, `ready`, …)
  are dropped at the forward task — the UI does not render them and
  surfacing them as `ChatEventPayload` with a stringly-typed variant
  would push the discriminator check into every React consumer.
- `data` is the envelope's `data` field verbatim. The shape contract lives
  in the chat plugin's protocol, not in dust — dust treats it as opaque
  until it reaches `<ComponentRenderer>`.

#### Example: `data_updated`

The chat plugin emits rendered `Component[]` deltas in `data.components`:

```json
{
  "thread_id": "thr_4b1f0ae2c9d84621",
  "event_type": "data_updated",
  "data": {
    "delta_kind": "append",
    "components": [
      {
        "type": "markdown",
        "content": "Here's the breakdown of the subscription lifecycle…"
      }
    ]
  }
}
```

`delta_kind` is one of `"replace" | "append"`:

- `replace` — chatMessages is set to `data.components` (first delta after
  `chat_subscribe`, or a tool-use re-render).
- `append` — `data.components` is concatenated onto the existing slice.

#### Example: `error`

```json
{
  "thread_id": "thr_4b1f0ae2c9d84621",
  "event_type": "error",
  "data": {
    "code": -33000,
    "message": "chat plugin: upstream model timeout",
    "recoverable": true
  }
}
```

`recoverable = true` means the React layer SHOULD keep the existing
`chatMessages` and render an inline banner; `false` means clear the slice
and show a terminal error. The field is informational — the chat plugin
owns the recoverability taxonomy; dust renders what it's told.

### (c) React Subscription Lifecycle

Scope: a new hook `useChatSubscription(threadId: string)` called once from
`App`. The hook owns exactly one `listen()` handle and one Rust-side
subscription at any time.

```
App mount
  ↓
useEffect #1 (no deps)  ── listen("dust://chat-event", handler) ──▶ unlistenRef
  ↓
useEffect #2 ([threadId]) ── invoke("chat_subscribe", { thread_id })
  ↓
(while mounted)           ── handler runs on every event; dispatches to setChatMessages
  ↓
threadId changes           ── effect cleanup runs: NO unlisten (handle is stable)
                              NO invoke("chat_unsubscribe")
                          ── new effect body runs: invoke("chat_subscribe", { thread_id: NEW })
                              which atomically replaces on the Rust side
  ↓
App unmount               ── effect #2 cleanup: invoke("chat_unsubscribe")
                          ── effect #1 cleanup: unlistenRef.current?.()
```

Key invariants:

1. **The `listen()` handle is attached exactly once** (mount-time effect with
   `[]` deps). It survives every thread change. This is what prevents
   mid-stream delta loss during re-subscribe — events arriving between
   `chat_subscribe(old)` return and `chat_subscribe(new)` completion still
   land in the same handler.
2. **`chat_unsubscribe` is only called on unmount**, never on thread change.
   Thread changes call `chat_subscribe` again with the new `thread_id`; the
   Rust side handles the atomic replace (see Interfaces (a) replacement
   protocol). If we called `chat_unsubscribe` first, there would be a
   window where the broadcast receiver is dropped — the race the design
   exists to prevent.
3. **Handler filtering happens in React**, not only in Rust. The Rust side
   already filters by the subscription's pinned `thread_id`, but the
   React handler additionally drops payloads whose `thread_id` does not
   match the current `threadId` state. This handles the interleaving case
   where the Rust subscription has been replaced to thread B, but an
   emit for thread A (already in-flight at the time of swap) still reaches
   the webview. We prefer to drop it visibly in the React boundary than
   paper over it in Rust.

Pseudocode:

```ts
function useChatSubscription(threadId: string) {
  const [chatMessages, setChatMessages] = useState<Component[]>([])
  const unlistenRef = useRef<UnlistenFn | null>(null)
  const threadIdRef = useRef(threadId)
  threadIdRef.current = threadId

  // Effect #1 — listen attached once for the component's lifetime.
  useEffect(() => {
    let cancelled = false
    listen<ChatEventPayload>('dust://chat-event', ({ payload }) => {
      if (payload.thread_id && payload.thread_id !== threadIdRef.current) return
      if (payload.event_type === 'data_updated') {
        const { delta_kind, components } = payload.data as {
          delta_kind: 'replace' | 'append'
          components: Component[]
        }
        setChatMessages(prev =>
          delta_kind === 'replace' ? components : [...prev, ...components],
        )
      } else if (payload.event_type === 'error') {
        // render error banner — details omitted here; see state shape §(d)
      }
    }).then(unlisten => {
      if (cancelled) unlisten()
      else unlistenRef.current = unlisten
    })
    return () => {
      cancelled = true
      unlistenRef.current?.()
      unlistenRef.current = null
      void invoke('chat_unsubscribe')
    }
  }, [])

  // Effect #2 — atomic subscribe/replace on thread change.
  useEffect(() => {
    void invoke('chat_subscribe', { threadId: threadId || null })
    // no cleanup — Rust side handles replace; unmount handled by effect #1
  }, [threadId])

  return chatMessages
}
```

### (d) App.tsx State Shape Addition

Current state (relevant portion, `plugins/dust/src/App.tsx:105`):

```ts
const [detail, setDetail] = useState<Component[] | null>(null)
const [currentThreadId, _setCurrentThreadId] = useState('')
```

Additions:

```ts
const chatMessages = useChatSubscription(currentThreadId)
const [chatError, setChatError] = useState<ChatErrorInfo | null>(null)
```

**Reconciliation with `detail`**: these two slices are rendered by different
UI paths and never compete:

| Slice          | Source                                    | Consumer in DetailPane                 |
|----------------|-------------------------------------------|----------------------------------------|
| `detail`       | `invoke('render_ui')` on Enter            | Existing `<ComponentRenderer>` block   |
| `chatMessages` | `listen('dust://chat-event')` deltas      | **New** chat stream block, rendered when `pluginInfo?.manifest.id === 'chat'` |

Rendering rule (inside `DetailPane`, replacing the current `{detail && …}`
branch):

```tsx
const isChat = pluginInfo?.manifest.id === 'chat'
const components = isChat ? chatMessages : detail

{components && components.length > 0 ? (
  <ComponentRenderer components={components} onAction={onAction} onOpenFile={onOpenFile} />
) : (
  <p>…empty-state copy…</p>
)}
```

This preserves the existing non-chat code path byte-for-byte: `detail` still
drives `<ComponentRenderer>` for every other plugin. For the chat plugin,
the one-shot `render_ui` result (if any) is intentionally ignored — the
chat plugin emits its entire UI as streamed `data_updated` deltas.

**State-transition rules for `chatMessages`**:

| Transition              | Trigger                                        | Effect                              |
|-------------------------|------------------------------------------------|-------------------------------------|
| new thread starts       | `currentThreadId` changes                      | `chat_subscribe` replaces Rust sub; first `data_updated` with `delta_kind:"replace"` resets slice |
| streaming delta arrives | `data_updated` with `delta_kind:"append"`      | append to slice                     |
| error arrives           | `event_type:"error"`                           | set `chatError`; slice kept iff `recoverable` |
| user presses Esc        | existing `detail !== null` branch              | slice **not** cleared — chat window stays so the user can re-open it; only `detail` and `pluginInfo` clear (matches current Esc semantics) |
| unmount                 | `chat_unsubscribe`                             | slice discarded with the component  |

**Escape-key reconciliation** is the only subtle point. Today, Esc on a
loaded detail sets `detail = null` and `pluginInfo = null`
(`plugins/dust/src/App.tsx:325`). With the new rendering rule, `pluginInfo`
going null means `isChat` flips to false, so `ComponentRenderer` falls
through to `detail` (which is also null) and shows the empty-state copy —
**even though `chatMessages` still holds content**. That is intentional:
Esc is the user asking to hide the payload; the subscription stays alive
so re-entering the same thread with ↵ re-renders instantly from the slice.

## Risks

1. **`AppHandle` emit while webview hidden.** Tauri's `app.emit` delivers
   to all webviews including hidden ones — no risk here. Confirmed by the
   `file_changed` emit at `plugins/dust/src-tauri/src/lib.rs:221` which
   fires whether or not the window is visible.
2. **Broadcast channel lag.** `tokio::sync::broadcast` drops the oldest
   message for slow receivers. The forward task is a tight `recv → emit`
   loop with no awaits except the emit itself (synchronous in practice).
   If a lag error does occur, we log and continue — the registry's event
   ring still holds the missed events, but dust will not replay them on
   its own. Mitigation: downstream phase can add a `since_sequence` cursor
   pinned in `ChatSubscription` for recovery; out of scope for MVP.
3. **Initial subscribe races mount.** Effect #1 (`listen`) resolves
   asynchronously; if `chat_subscribe` emits its first event before
   `listen` is attached, the event is lost. The chat plugin MUST wait
   for a `chat_subscribe` request before emitting its first delta (which
   is already the envelope protocol: plugins emit in response to host
   actions, not before).
4. **Thread-id pin drift.** The first `data_updated` carrying a
   `thread_id` is expected to pin the thread — but the subscription's
   Rust-side `thread_id` field stays whatever was passed to
   `chat_subscribe`. If the plugin reassigns thread IDs mid-flight, dust
   filters incorrectly. Mitigation: have `dispatch_action` for the chat
   plugin return the thread id synchronously so the React shell can call
   `chat_subscribe` with a known thread id. Already feasible — the
   existing ask-claude dispatch returns a `Promise` that can be awaited
   before setting `currentThreadId`.
5. **`chat` plugin absent from registry.** `connect_and_subscribe("chat", 0)`
   errors with `RegistryError::NotFound`. `chat_subscribe` must surface
   this as `Err(String)` and the React layer must tolerate it silently —
   users on a vanilla dust install without the chat plugin should still
   be able to use the shell. Guarded by `if (pluginInfo?.manifest.id ===
   'chat')` at the call site.

## Trade-offs Accepted

- **Chat-specific commands, not generic `plugin_subscribe`.** We pick chat
  names (`chat_subscribe`, `dust://chat-event`) rather than generic
  `plugin_subscribe(plugin_id, …)` because it keeps the MVP surface
  minimal and avoids premature abstraction for a second streaming plugin
  that does not yet exist. Generalising is a rename + one extra param
  when the second plugin lands — cheap to defer.
- **No persistence across webview restart.** Closing the window and
  reopening it starts from an empty `chatMessages`. The registry's event
  ring still has the history (512 KiB / 1000 events, see
  `plugins/dust/dust-core/src/events.rs:22`), but the React shell does not
  pass `since_sequence > 0` on reconnect. Accepted because dust is a
  summoned-pane UI where users expect fresh state on re-summon.
- **Filter-in-React for residual races.** Cleaner to enforce the thread
  filter solely in Rust, but defence-in-depth here costs two lines and
  survives future refactors on either side.
- **Single event channel, single subscription.** No fan-out to multiple
  simultaneous chat views. Acceptable: the shell has one DetailPane.

<!-- scratch -->
Implementer notes (phase-2):

1. `AppState` mutation — add `chat_sub: tokio::sync::Mutex<Option<ChatSubscription>>`.
   Register the two commands in `invoke_handler![…]` at
   `plugins/dust/src-tauri/src/lib.rs:432`. Use `tokio::sync::Mutex`, not
   `std::sync::Mutex`, because the critical section awaits the registry.

2. Registry subscribe-unsubscribe pair: use `connect_and_subscribe` not the
   two-step `open_subscriber_connection` + `subscribe_plugin_events` — the
   combined path does rollback on failure. See
   `plugins/dust/dust-registry/src/lib.rs:770`.

3. Forward task shape (sketch):
   ```rust
   let app2 = app.clone();
   let thread_filter = thread_id.clone();
   let mut rx = handle.live_rx;
   let task = tokio::spawn(async move {
     loop {
       match rx.recv().await {
         Ok(evt) => {
           let et = match evt.event_type {
             EventType::DataUpdated => "data_updated",
             EventType::Error => "error",
             _ => continue,
           };
           let tid = evt.data.get("thread_id").and_then(|v| v.as_str()).map(String::from)
                       .or_else(|| thread_filter.clone());
           if let (Some(f), Some(t)) = (&thread_filter, &tid) { if f != t { continue; } }
           let _ = app2.emit("dust://chat-event", ChatEventPayload {
             thread_id: tid, event_type: et.into(), data: evt.data,
           });
         }
         Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
         Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
       }
     }
   });
   ```

4. GOTCHA: tests that mock the chat plugin must emit at least one
   `ready` + one `data_updated` to exercise the filter branch. Replay ring
   tests in `plugins/dust/dust-conformance/tests/replay.rs` already cover
   the registry boundary — do not duplicate those.

5. DECISION marker: should `chat_unsubscribe` flush pending deltas before
   aborting the forward task, or is abort-fire-and-forget fine? Current
   design says fire-and-forget (unmount is terminal). Flag for review if
   the chat plugin adds a graceful close handshake.

6. PATTERN: mirror the `file_changed` emit pattern
   (`plugins/dust/src-tauri/src/lib.rs:221`) for string-typed event names
   so the frontend type surface stays grep-friendly.
<!-- /scratch -->

LEARNING: Tauri event channels have no replay — the single-listen + atomic
Rust-side replace pattern is the only way to guarantee zero-loss across
subscription changes.

PATTERN: Two-slice state (`detail` vs `chatMessages`) dispatched by plugin
id at render time scales to additional streaming plugins without touching
the existing non-streaming code paths.

GOTCHA: `tokio::sync::broadcast::Receiver` drops events for lagged consumers.
The forward task must handle `RecvError::Lagged` explicitly — silently
continuing is acceptable for MVP only because the registry ring is the
source of truth.

DECISION: `chat_unsubscribe` is called on React unmount only, never on
thread change. Thread change relies on Rust-side atomic replace. Rationale:
eliminates the unlisten-vs-subscribe race window that is the default
failure mode of streaming UI code.
