# DESIGN — Tracker TUI in Dust Dashboard

Scope: enrich the tracker plugin's `render_issues` output, promote the Ctrl+K
palette from a boolean overlay to a first-class state machine, keep an
`events.subscribe` connection open for the lifetime of `UILoaded`, and split
the detail pane so a focused row can show its full context without losing the
issue list.

The dashboard today carries one `AppState` enum with five variants and a
`bool action_palette_open`; the palette is modal but has no sub-states, the
issues view is a single `Component::Table`, and nothing subscribes to plugin
events. This doc names the new variants, registry helpers, and components to
close those gaps.

---

## A. Enriched `render_issues` component tree

### Returned component order

`render_issues` (tracker-side, `plugins/tracker/src/dust_serve.rs`) will
return a five-element `Vec<Component>` in the following fixed order. The
dashboard renders them top-to-bottom via `component_renderer::render`, which
already splits the detail-pane area with `Layout::vertical` and per-component
height estimates.

| # | Component | Role |
|---|-----------|------|
| 1 | `Component::KeyValue { pairs }` | Summary row: Total · Open · In-Progress · Blocked |
| 2 | `Component::Divider` | Separator between summary and table |
| 3 | `Component::Table { columns, rows }` | Enriched issue table (see mapping below) |
| 4 | `Component::Divider` | Separator between table and badge legend |
| 5 | `Component::List { items, title: Some("Legend") }` | Priority / status colour key |

The detail-pane row-selection model (§D) operates on component #3 only;
components #1, #2, #4, #5 are decorative and non-selectable.

### Field → component mapping (component #3, the Table)

`Issue` fields come from `plugins/tracker/src/models.rs`. The enriched Table
widens to six columns:

| Column | Width | Source field | Rendered by |
|--------|-------|--------------|-------------|
| `#` | 6 | `seq_id` → `TRK-{n}` (fallback: first 8 chars of `id`) | plain cell |
| `St` | 3 | `status` → glyph + colour (`○` open, `◐` in-progress, `✓` done, `✗` cancelled) | host-side span colouring driven by `Component::Badge`-style ANSI in the cell string |
| `Pri` | 3 | `priority` → colour-coded letter (`P0`=red, `P1`=orange, `P2`=yellow, `P3`=dim) | cell text; colour applied during `render_component` path after decomposition (see below) |
| `Title` | Fill | `title` | plain cell, truncated at column width |
| `Assn` | 8 | `assignee` or `"-"` | dim cell when `None` |
| `Upd` | 10 | `updated_at` → relative (`2h`, `3d`) | dim cell |

The `labels`, `description`, `parent_id`, and `created_at` fields are not
column values — they surface only in the detail split (§D). `parent_id`
influences row presentation: child issues get a `  ↳ ` prefix injected into
the `Title` cell by `render_issues` before serialisation, so the dashboard
needs no tree awareness.

### Summary KeyValue (component #1)

`KVPair` pairs, in order:

| Label | Value | `value_color` |
|-------|-------|---------------|
| `Total` | count of all non-cancelled issues | `None` |
| `Ready` | `commands::ready(&conn).len()` | green |
| `In Progress` | count with `status == "in-progress"` | orange |
| `Blocked` | count with `status == "blocked"` | red |

### Legend List (component #5)

Six `ListItem`s — four priorities and two statuses that use non-obvious
glyphs. Items are `disabled: true` so the host renders them dimmed; each
item's `icon` field carries the coloured glyph (host maps through
`map_text_style`).

---

## B. Ctrl+K palette state machine

The palette is promoted from `bool action_palette_open` to a proper slice of
`AppState`. Three new variants replace the boolean and add an
arg-collection sub-state.

### New `AppState` variants (complete list)

```rust
pub enum AppState {
    // Existing
    Idle,
    Searching,
    SelectedCapability,
    UILoaded,
    ActionDispatched,
    // NEW
    IssueFocused,          // a row in render_issues Table is active (§D)
    PaletteOpen,           // Ctrl+K overlay showing action list
    PaletteCollectingArg,  // an action is chosen; collecting positional args
    PaletteConfirm,        // all args gathered; waiting on Enter to dispatch
}
```

`action_palette_open: bool` is removed. The `App` struct gains:

```rust
pub palette: Option<PaletteCtx>,

pub struct PaletteCtx {
    pub op_id: String,                          // e.g. "create", "update"
    pub schema: Vec<ArgSpec>,                   // ordered positional args from manifest
    pub collected: HashMap<String, Value>,      // completed args
    pub current: Option<ArgSpec>,               // arg being typed (None in PaletteOpen)
    pub buffer: String,                         // in-progress text for `current`
    pub cursor: usize,                          // action-list cursor (PaletteOpen only)
}

pub struct ArgSpec {
    pub name: String,        // "title", "status", …
    pub prompt: String,      // shown in the palette status line
    pub required: bool,
    pub kind: ArgKind,       // Text | Enum(Vec<String>) | IssueId
}
```

`schema` is provided by a new manifest field (`Capability::Command` gains an
optional `actions: Vec<ActionDef>`); if the plugin does not advertise any
actions, the palette lists the raw capability enum (current behaviour) and
`PaletteCollectingArg` is unreachable.

### Transition diagram

```
                 Ctrl+K                    Enter (on action)
UILoaded ────────────────▶ PaletteOpen ────────────────────▶ PaletteCollectingArg
    ▲        Esc / Ctrl+K       │                                 │  │
    │◀──────────────────────────┘                                 │  │ Tab / Enter
    │                                                             │  │ (per arg)
    │                                                             ▼  │
    │                                                    PaletteCollectingArg
    │                                                       (next arg)
    │                                                             │
    │                                                             │ all args done
    │                                                             ▼
    │                                                       PaletteConfirm
    │                    Enter (dispatch)                         │
    │◀──────── ActionDispatched ◀─────────────────────────────────┘
    │                                    Esc at any palette state
    │◀────────────────────────────────────────────────────────────┘
    │         (IssueFocused if there was a selected row, else UILoaded)
```

Esc inside any palette state unwinds to the state the palette was opened
from (`UILoaded` or `IssueFocused`). Ctrl+K from within any palette state is
equivalent to Esc (toggle-to-close).

### Keybindings (per state)

| State | Key | Effect |
|-------|-----|--------|
| `PaletteOpen` | `↑` / `↓` | move `palette.cursor` over actions |
| `PaletteOpen` | `Enter` | load `schema` for action at cursor; if empty → `PaletteConfirm`; else → `PaletteCollectingArg` with `current = schema[0]` |
| `PaletteOpen` | `Esc` / `Ctrl+K` | drop `palette`, return to prior state |
| `PaletteCollectingArg` | `char` | append to `palette.buffer` |
| `PaletteCollectingArg` | `Backspace` | pop from buffer |
| `PaletteCollectingArg` | `Tab` / `Enter` | commit buffer into `collected[current.name]`; advance to next `ArgSpec` or `PaletteConfirm` |
| `PaletteCollectingArg` | `Shift+Tab` | step back to previous arg (restore buffer from `collected`) |
| `PaletteCollectingArg` | `↑` / `↓` | when `kind == Enum`, cycle preset value into buffer |
| `PaletteCollectingArg` | `Esc` | drop `palette`, return to prior state |
| `PaletteConfirm` | `Enter` | call `Registry::dispatch_action_structured` → `ActionDispatched` |
| `PaletteConfirm` | `Shift+Tab` | return to `PaletteCollectingArg` on last arg |
| `PaletteConfirm` | `Esc` | drop `palette`, return to prior state |

---

## C. `events.subscribe` connection lifecycle

Today the dashboard polls plugins for updates. The subscriber protocol
(`Registry::open_subscriber_connection` → `subscribe_plugin_events`) is
already implemented but unused. This design wires it to the `UILoaded`
lifecycle so the detail pane refreshes from live events instead of manual
re-render.

### New `App` fields

```rust
pub event_stream: Option<EventStreamHandle>,

pub struct EventStreamHandle {
    pub plugin_id: String,
    pub conn_id: ConnectionId,
    pub subscription_id: String,
    pub last_sequence: u64,
    pub rx: tokio::sync::broadcast::Receiver<EventEnvelope>,
    pub task: tokio::task::JoinHandle<()>,     // forwards rx → UI channel
}
```

### New registry helpers

The raw four-call dance (open → subscribe → recv loop → unsubscribe →
close) is wrapped by two new `impl Registry` helpers so `App` does not
manage `ConnectionId`s itself:

```rust
impl Registry {
    /// Open a connection, subscribe from `since_sequence`, return a handle
    /// bundling the receiver and metadata. Composes
    /// `open_subscriber_connection` + `subscribe_plugin_events`.
    pub async fn connect_and_subscribe(
        &self,
        plugin_id: &str,
        since_sequence: u64,
    ) -> Result<SubscriptionHandle, RegistryError>;

    /// Unsubscribe then close — idempotent, swallows NotFound so that
    /// lifecycle teardown is safe during plugin crash.
    pub async fn disconnect_subscriber(
        &self,
        plugin_id: &str,
        conn_id: ConnectionId,
        subscription_id: &str,
    ) -> Result<(), RegistryError>;
}

pub struct SubscriptionHandle {
    pub conn_id: ConnectionId,
    pub subscription_id: String,
    pub replay: Vec<EventEnvelope>,             // events with seq >= since
    pub live: tokio::sync::broadcast::Receiver<EventEnvelope>,
    pub oldest_available: u64,
}
```

And an App-side convenience:

```rust
impl App {
    /// Open a stream for the currently selected plugin, driving replayed
    /// envelopes into `self.components` and caching `last_sequence`.
    pub async fn open_event_stream(&mut self);

    /// Close the active stream if any. Safe to call from any state.
    pub async fn close_event_stream(&mut self);

    /// Consume a single envelope — maps `DataUpdated` to a component
    /// refresh by re-invoking `registry.render_ui`.
    pub async fn on_event(&mut self, env: EventEnvelope);
}
```

### Lifecycle rules

| Transition | Action |
|------------|--------|
| `* → UILoaded` (entering) | `App::open_event_stream()` with `since_sequence = 0` on first entry; `last_sequence` thereafter on re-entry without full teardown |
| `UILoaded / IssueFocused / Palette* / ActionDispatched → any non-`UILoaded`-family state | `App::close_event_stream()` before the state swap commits |
| `should_quit = true` | `App::close_event_stream()` before the main loop exits |
| `rx.recv()` → `RecvError::Lagged(n)` | log, bump `last_sequence` by the lag count (best-effort), keep handle |
| `rx.recv()` → `RecvError::Closed` or `ipc_call` error | drop handle; schedule reconnect with `tokio::time::sleep(500ms * 2^attempt)` capped at 8s; on reconnect use cached `last_sequence`; on `RegistryError::PluginError { code: -33007, .. }` (replay_gap) reset to `since_sequence = 0` and re-render from scratch |
| plugin removed via `sync()` | stream is dropped passively when `rx` closes; `status_msg` set to `"plugin removed"` and state snaps to `Idle` |

The palette-family states (`PaletteOpen`, `PaletteCollectingArg`,
`PaletteConfirm`) and `IssueFocused` count as inside the `UILoaded` family
— the stream stays open across them. The stream closes only when the user
truly leaves the plugin (Esc back to `SelectedCapability`), dispatches an
action (`ActionDispatched` → we close because the action may mutate state
and we want a clean resubscribe on return), or quits.

---

## D. Detail-pane layout split and row-selection model

### New detail-pane layout (active only in `UILoaded` / `IssueFocused`)

Today `render_detail` gives the full pane to `component_renderer::render`.
When the plugin is tracker (detected by `plugin_id == "tracker"` or by
`Capability::Command { prefix: "tracker" }`), the pane splits horizontally:

```
┌─ Detail ─────────────────────────────────────────────────────┐
│ Summary KV · Divider · Table · Divider · Legend              │  ← left: Fill(3)
│                                                              │
│                                                              │
├──────────────────────────────────────────────────────────────┤
│ Issue: TRK-442                                   [P1] [open] │  ← right: Fill(2)
│ Title: Memory V2 foundation                                  │     shown only in
│ Assignee: alpha                                              │     IssueFocused;
│ Parent: TRK-440                                              │     empty hint in
│ Labels: memory, V2                                           │     UILoaded
│ ─────                                                        │
│ Description (markdown)                                       │
└──────────────────────────────────────────────────────────────┘
```

The split is vertical (two columns) when `inner.width >= 100`, horizontal
(two rows) otherwise, via `Layout::horizontal` / `Layout::vertical` chosen
at `render_detail` entry. The left side calls the existing
`component_renderer::render`; the right side is a new
`render_issue_detail(frame, right_area, app)` function local to `ui.rs`.

### Row-selection model

The dashboard does not know that component #3 is the issues table — to the
renderer, any `Component::Table` is anonymous. Two mechanisms bridge this:

1. **Selectable component annotation.** `Component` does not change; instead
   the app maintains a sibling slice `components_selectable:
   Vec<bool>` populated by `App::open_event_stream` → `render_ui` based on
   component index. The tracker convention: component #3 is selectable;
   everyone else is false. Implemented as a helper
   `App::detect_selectable_table(&self.components) -> Option<usize>` that
   returns the index of the first `Component::Table` — tracker is the only
   consumer for now; generalising requires a protocol addition and is
   deferred (DECISION: defer multi-selectable to v2).

2. **Selected row cursor.** `App` gains:
   ```rust
   pub selected_row: Option<usize>,   // index into components[table_idx].rows
   pub table_idx: Option<usize>,      // cached from detect_selectable_table
   ```
   Both reset to `None` on `render_ui` success. First `↓` or `k/j` in
   `UILoaded` sets `selected_row = Some(0)` and transitions to
   `IssueFocused`. `Esc` in `IssueFocused` clears `selected_row` and
   returns to `UILoaded`.

### Keybindings (selection)

| State | Key | Effect |
|-------|-----|--------|
| `UILoaded` | `↓` / `j` | enter `IssueFocused`, `selected_row = 0` |
| `IssueFocused` | `↑` / `k` | `selected_row -= 1` (saturating; 0 ⇒ back to `UILoaded`) |
| `IssueFocused` | `↓` / `j` | `selected_row += 1` (clamped to `rows.len() - 1`) |
| `IssueFocused` | `Enter` | open palette scoped to the row's issue — `item_id` pre-filled on `PaletteCtx` |
| `IssueFocused` | `Esc` | clear selection, back to `UILoaded` (and close palette if open) |

### Detail-pane content source

`render_issue_detail` reads the selected row from
`app.components[table_idx].rows[selected_row]`, parses the `TRK-{n}` cell
back to a seq_id, and dispatches `tracker` action `get` (new op_id, not in
scope of this doc but tracked as a follow-up) to fetch the full `Issue`
JSON. Until `get` ships, the detail pane shows the row cells as a
`KeyValue` block — no extra IPC needed.

---

## Summary of new names

### New `AppState` variants

- `IssueFocused`
- `PaletteOpen`
- `PaletteCollectingArg`
- `PaletteConfirm`

### New `App` fields

- `palette: Option<PaletteCtx>` (replaces `action_palette_open: bool`)
- `event_stream: Option<EventStreamHandle>`
- `selected_row: Option<usize>`
- `table_idx: Option<usize>`

### New registry helpers

- `Registry::connect_and_subscribe(&self, plugin_id: &str, since_sequence: u64) -> Result<SubscriptionHandle, RegistryError>`
- `Registry::disconnect_subscriber(&self, plugin_id: &str, conn_id: ConnectionId, subscription_id: &str) -> Result<(), RegistryError>`
- `Registry::dispatch_action_structured(&self, plugin_id: &str, op_id: &str, item_id: Option<&str>, args: HashMap<String, Value>) -> Result<ActionResult, RegistryError>` *(thin wrapper around existing `dispatch_action` that builds `ActionParams` from the palette context)*

### New App helpers

- `App::open_event_stream(&mut self)`
- `App::close_event_stream(&mut self)`
- `App::on_event(&mut self, env: EventEnvelope)`
- `App::detect_selectable_table(components: &[Component]) -> Option<usize>`

### New components emitted by `render_issues`

1. `Component::KeyValue { pairs: [Total, Ready, In Progress, Blocked] }`
2. `Component::Divider`
3. `Component::Table { columns: [#, St, Pri, Title, Assn, Upd], rows }` *(widened from the current 4-column table)*
4. `Component::Divider`
5. `Component::List { items: <legend>, title: Some("Legend") }`

### New supporting types

- `PaletteCtx`
- `ArgSpec { name, prompt, required, kind: ArgKind }`
- `ArgKind { Text, Enum(Vec<String>), IssueId }`
- `SubscriptionHandle { conn_id, subscription_id, replay, live, oldest_available }`
- `EventStreamHandle { plugin_id, conn_id, subscription_id, last_sequence, rx, task }`

---

## Risks and trade-offs accepted

- **Tracker-specific branching in `ui.rs`** — the split-pane layout checks
  plugin identity. This violates the renderer's component-agnostic model
  but avoids a protocol change to mark "selectable" tables. Accepted as a
  scoped hack until a second plugin wants the same treatment.
- **`Capability::Command` schema extension** — adding `actions:
  Vec<ActionDef>` to the manifest breaks protocol back-compat for
  subscribers that reject unknown fields. Mitigated by `serde(default)` on
  the new field; existing plugins continue working.
- **Reconnect on `replay_gap`** — resetting to `since_sequence = 0` re-runs
  the full render path. For tracker this is cheap (one DB read); plugins
  with heavier `render` should implement cheaper `get` endpoints before
  adopting the same pattern.
- **Palette blocks all other input** — during `PaletteCollectingArg`,
  Ctrl+C is the only escape hatch if a plugin never returns. Mitigated by
  a visible timer and forced cancel at 5s (deferred).

## Open questions (DECISIONs flagged)

- DECISION: `Capability::Command.actions` schema — use a new
  `ActionDef { op_id, display_name, args: Vec<ArgSpec> }` struct or
  piggyback on JSON Schema? Recommend: the purpose-built struct;
  JSON Schema brings dependencies we do not need.
- DECISION: event-stream close on `ActionDispatched` — do we close or hold
  open? Recommend: close, to force a clean resubscribe after mutation.
  Open to reversal once action latency is measured.
- DECISION: multi-plugin selectable-table support — defer to v2; revisit
  once the scheduler plugin's "jobs" view lands.

<!-- scratch -->
Key decisions for the implementer to honour:

1. Do NOT remove `action_palette_open: bool` in a compat-preserving way —
   replace it outright with `palette: Option<PaletteCtx>`. The old boolean
   has no external callers.
2. The event-stream handle owns a JoinHandle; close_event_stream must
   `handle.task.abort()` before dropping to avoid a leak.
3. `render_issue_detail` should fall back to row-cell KeyValue when the
   tracker `get` op is not yet implemented — do not block on it.
4. The `detect_selectable_table` heuristic — "first Table is selectable" —
   is intentionally tracker-specific; document it loudly so the next
   plugin author does not get surprised.
5. Reconnect backoff: cap at 8s, max 5 attempts, then surface `status_msg`
   and fall back to polling mode (re-render every 30s). Do not busy-loop.
<!-- /scratch -->
