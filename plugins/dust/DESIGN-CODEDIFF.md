---
produced_by: architect
phase: phase-1
workspace: 20260420-3e20fc2d
created_at: "2026-04-20T10:05:00Z"
confidence: high
depends_on: []
token_estimate: 4800
---

# DESIGN-CODEDIFF — `Component::CodeDiff` wire format, renderers, and accept protocol

## Context

Dust today emits eight component variants (`Text`, `List`, `Markdown`, `Divider`,
`Table`, `KeyValue`, `Badge`, `Progress`, `AgentTurn`). Agent-driven plugins —
notably the `chat` plugin — need to show proposed file edits inline in an
agent turn and let the user accept or reject each hunk with a single keystroke
(ratatui) or click (React). No existing component carries the structural
information a diff renderer needs (per-line kind markers, hunk headers, the
source path). A minimal free-form text blob would work for display but cannot
carry the `hunk_id` that an accept-dispatch round-trip needs.

The design below introduces a ninth variant, `Component::CodeDiff`, with a
schema rich enough to drive both the intaglio-styled React layout and a ratatui
layout, plus an action-dispatch protocol (`op_id = "code_diff.accept_hunk"`) so
either frontend can request the emitting plugin apply a single hunk.

The design covers seven surfaces called out by the task:
(a) wire schema, (b) Rust struct / serde, (c) React layout, (d) ratatui
layout, (e) accept-hunk dispatch protocol, (f) path-validation reuse,
(g) end-to-end test plan.

## Candidates Considered

| # | Approach | Pros | Cons |
|---|----------|------|------|
| 1 | **Structured `CodeDiff` variant with per-line kinds** (CHOSEN) | Single source of truth; both renderers and accept-dispatch can derive everything from one payload; hunk_id travels with the component. | New variant requires version bump coordination across host + plugin. |
| 2 | `Component::Markdown` fenced diff block | Zero protocol change; markdown already renders. | No hunk_id → accept dispatch has to re-parse text; no gutter line numbers; no per-hunk chip placement. |
| 3 | `Component::Table` with `+`/`-` prefix cells | Reuses table renderer + table selection. | Column model forces every line to share the same schema — no hunk header row without special rows; accept-dispatch needs a side-channel to know which rows are one hunk. |
| 4 | Embed diff as a foreign-key reference (`{ type: "code_diff_ref", diff_id }`) | Smaller payload on-the-wire. | Adds a fetch round-trip for every render; host now has to cache opaque plugin state. Speculative. |

**Selected: #1.** The diff render layout differs enough from tables (grouped
hunks, two-gutter numbering, per-hunk action chips) that mapping it onto tables
is a worse forcing-function than adding a variant. Option 2 is cheaper but
breaks the accept-hunk round-trip — the very thing the feature exists for.
Option 4 invents a state store we don't have.

## Decision

Add `Component::CodeDiff` to `dust_core::Component` with the exact schema
defined below. Reuse the existing `dispatch_action` path (no new envelope)
with a conventional `op_id` of `"code_diff.accept_hunk"` and `item_id` = the
per-hunk stable id. Duplicate the `validate_path` helper in the plugin that
owns the accept-hunk handler (currently chat) with a pointer-comment back to
`plugins/dust/src-tauri/src/lib.rs`; defer extracting a shared crate until a
second caller appears.

---

## (a) Wire Schema — `Component::CodeDiff`

Type discriminant: `"type": "code_diff"` (snake_case, matches the existing
`#[serde(tag = "type", rename_all = "snake_case")]` on `Component`).

### Fields

| Field | JSON type | Rust type | Presence | Notes |
|-------|-----------|-----------|----------|-------|
| `type` | string literal `"code_diff"` | enum tag | required | Emitted by serde from variant name. |
| `path` | string | `String` | required | Absolute path to the target file. MUST be inside `$HOME` — validated at accept time, not at render time. |
| `basename` | string | `String` | required | Display name shown in the file-header chip. Plugin-supplied so the host does not need to know OS path rules. |
| `language` | string \| null | `Option<String>` | optional | Hint for syntax highlighting (e.g. `"rust"`, `"tsx"`, `"markdown"`). Omitted from wire when `None` (`skip_serializing_if`). |
| `hunks` | array of `Hunk` | `Vec<Hunk>` | required, min length 1 | Rendered in order; empty array is a protocol violation and the host SHOULD render an error placeholder. |

Each `Hunk`:

| Field | JSON type | Rust type | Presence | Notes |
|-------|-----------|-----------|----------|-------|
| `id` | string | `String` | required | Stable per-(component-render) hunk id. Used as `item_id` in the accept-dispatch round-trip. Plugin chooses the format (e.g. `"h-0"`, `"chat.msg-42.h-1"`) — host treats it opaquely. |
| `old_start` | integer ≥ 0 | `u32` | required | 1-based line number in the pre-image; `0` means the file did not previously exist. |
| `old_count` | integer ≥ 0 | `u32` | required | Line count in the pre-image. `0` for a file-creation hunk. |
| `new_start` | integer ≥ 0 | `u32` | required | 1-based line number in the post-image; `0` means the file will be deleted. |
| `new_count` | integer ≥ 0 | `u32` | required | Line count in the post-image. `0` for a file-deletion hunk. |
| `header` | string \| null | `Option<String>` | optional | Free-form human label shown next to the `@@ … @@` range (e.g. `"fn render_component"`). Omitted when `None`. |
| `lines` | array of `DiffLine` | `Vec<DiffLine>` | required, min length 1 | Rendered in order. |

Each `DiffLine`:

| Field | JSON type | Rust type | Presence | Notes |
|-------|-----------|-----------|----------|-------|
| `kind` | `"context" \| "add" \| "remove"` | `DiffLineKind` enum | required | Drives gutter glyph and intaglio color token. |
| `content` | string | `String` | required | The raw line content **without** the leading `+`/`-`/` ` marker — marker is rendered from `kind`. Trailing newlines are stripped by the plugin. |

### Example JSON Payload

Single-file, two-hunk edit to a Rust source file:

```json
{
  "type": "code_diff",
  "path": "/Users/joey/nanika/plugins/dust/dust-core/src/lib.rs",
  "basename": "lib.rs",
  "language": "rust",
  "hunks": [
    {
      "id": "h-0",
      "old_start": 142,
      "old_count": 3,
      "new_start": 142,
      "new_count": 4,
      "header": "pub enum Component",
      "lines": [
        { "kind": "context", "content": "#[serde(tag = \"type\", rename_all = \"snake_case\")]" },
        { "kind": "context", "content": "pub enum Component {" },
        { "kind": "remove",  "content": "    Text {" },
        { "kind": "add",     "content": "    CodeDiff(CodeDiffBody)," },
        { "kind": "add",     "content": "    Text {" }
      ]
    },
    {
      "id": "h-1",
      "old_start": 208,
      "old_count": 0,
      "new_start": 209,
      "new_count": 2,
      "header": null,
      "lines": [
        { "kind": "context", "content": "}" },
        { "kind": "add",     "content": "" },
        { "kind": "add",     "content": "// new trailing helper" }
      ]
    }
  ]
}
```

A file-creation payload uses `old_start = 0, old_count = 0`. A file-deletion
payload uses `new_start = 0, new_count = 0`. Neither is a protocol violation.

---

## (b) `dust_core` Struct Definition

Place in `plugins/dust/dust-core/src/lib.rs`, immediately after the
`AgentTurn` variant. Reuse the existing module-level `#[serde(tag = "type",
rename_all = "snake_case")]` on `Component` — no per-variant rename override
is needed for the variant tag.

```rust
/// A proposed file edit, rendered inline so the user can accept or reject
/// each hunk with a single keystroke (ratatui) or click (React).
///
/// The emitting plugin owns the `hunks[i].id` values and the physical write
/// (via the `code_diff.accept_hunk` action — see DESIGN-CODEDIFF.md §e).
CodeDiff {
    path: String,
    basename: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    hunks: Vec<Hunk>,
},
```

Add the three support types at module scope:

```rust
/// A single `@@` block inside a [`Component::CodeDiff`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub id: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub lines: Vec<DiffLine>,
}

/// One line inside a [`Hunk`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
}

/// The three diff-line categories.
///
/// `serde(rename_all = "snake_case")` matches the host-wide convention and
/// yields on-the-wire values `"context" | "add" | "remove"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    Context,
    Add,
    Remove,
}
```

### Serde choice rationale

- **Variant uses named-struct form, not a newtype around a body struct.** Keeps the
  variant self-describing on the wire (all fields appear as siblings of `type`)
  and matches the shape of every other variant. A newtype (`CodeDiff(CodeDiffBody)`)
  would need `#[serde(flatten)]` to avoid a nested object and complicates
  custom handling later.
- **`rename_all = "snake_case"` on `DiffLineKind`** — mirrors `BadgeVariant`
  and `RestartPolicy` in the same module. No custom renames needed.
- **`skip_serializing_if = "Option::is_none"` on `language` and `header`** —
  matches the rest of the file (`List.title`, `ListItem.description`, etc.).
  Keeps defaults out of the wire format so the round-trip tests below can
  assert exact JSON bodies.
- **No `skip_serializing_if` on numeric fields.** `old_start = 0` is a
  meaningful value (file creation), not an absence, so it must serialize.

---

## (c) React Render Layout

**Files to touch:**
- `plugins/dust/src/types.ts` — add `CodeDiffComponent` to the union.
- `plugins/dust/src/ComponentRenderer.tsx` — add a `RenderCodeDiff` case.

### Intaglio styling reference points

The component inherits the app-wide tokens from `plugins/dust/src/globals.css`.
The two intaglio brand colors from the project memory and the existing
ratatui renderer are reused here — add them as tokens in `globals.css`
rather than inlining hex in the component.

Add to `:root` in `globals.css`:

```css
--intaglio-terracotta: #DA7757;   /* add-line accent, accept chip */
--intaglio-cream:      #F2EAD7;   /* file header text */
--diff-add-bg:         rgba(218, 119, 87, 0.10);
--diff-remove-bg:      rgba(229, 83, 75, 0.10);   /* builds on --color-error */
--diff-gutter-fg:      var(--text-secondary);
```

The chip placement ("accept" / "reject") reuses the existing `--pill-border`
and `--selected-bg` tokens from the same file, so the look lines up with
existing capability chips and `AgentTurn` styling.

### Per-component layout (vertical stack, top to bottom)

```
┌─ file header chip ────────────────────────────────────────────────┐
│ [icon] basename.ext                              rust · 2 hunks    │
│  └── path (truncated, text-secondary)                              │
└────────────────────────────────────────────────────────────────────┘
  (per hunk:)
┌─ hunk header bar ─────────────────────────────────────────────────┐
│ @@ -142,3 +142,4 @@  fn render_component      [Accept] [Reject]   │
└────────────────────────────────────────────────────────────────────┘
┌─ hunk body ───────────────────────────────────────────────────────┐
│ old# | new# | ± │ content                                          │
│  142 |  142 |   │ #[serde(tag = "type", rename_all = "snake_case")]│  <- context
│  143 |  143 |   │ pub enum Component {                             │  <- context
│  144 |      | - │ ····Text {                                       │  <- remove (red-ish)
│      |  144 | + │ ····CodeDiff(CodeDiffBody),                      │  <- add (terracotta)
│      |  145 | + │ ····Text {                                       │  <- add
└────────────────────────────────────────────────────────────────────┘
  (gap — 8px — between hunks)
```

### Element breakdown

- **File header chip** — a `<div>` with `flex items-center gap-2`, `px-3 py-2`,
  rounded, background `--mic-bg`, border `1px solid --pill-border`. Title line:
  basename in `font-semibold`, color `var(--intaglio-cream)`. Trailing meta
  (`rust · 2 hunks`) right-aligned, color `--text-secondary`. Full `path` on a
  second line, `text-[11px]`, truncated.
- **Hunk header bar** — `flex items-center justify-between`, `px-3 py-1`,
  font-mono `text-[11px]`, background `--bg-elevated`. Left: `@@ -old,ocnt
  +new,ncnt @@` then the `header` string (if present) in
  `--text-secondary`. Right: two chips.
  - `Accept` chip: `--intaglio-terracotta` fg, transparent bg, `1px solid
    --intaglio-terracotta`, rounded-sm, `px-2 py-0.5`, `text-[10px]
    font-semibold uppercase tracking-wider`. On hover: fill with
    `--intaglio-terracotta` at 15% alpha. Disabled once the hunk has been
    accepted; the component mutates to a `[Accepted]` badge.
  - `Reject` chip: same shape; fg `--text-secondary`. Reject is a pure
    client-side dismissal — no round-trip.
- **Hunk body** — a `<div role="table">` with CSS grid
  `grid-template-columns: 3.5rem 3.5rem 1.25rem 1fr`. Each line is four cells:
  - *old-number gutter* — right-aligned, tabular-nums, color
    `--diff-gutter-fg`. Blank for `add` kind.
  - *new-number gutter* — same, blank for `remove` kind.
  - *marker cell* — single glyph: ` ` (context), `+` (add), `−` (remove).
    Colored per kind; backgrounds extend across the *content* cell only.
  - *content cell* — raw `content`, monospace (already body default), no
    wrapping (`whitespace-pre`), `overflow-x-auto` per row. Background
    `--diff-add-bg` / `--diff-remove-bg` / transparent based on `kind`.

Line numbers are computed client-side: walk `lines` once and maintain
`(old, new)` cursors starting at `old_start` / `new_start`, incrementing as
described in the unified-diff spec (context bumps both, add bumps only new,
remove bumps only old).

### Empty / edge states

- Empty `hunks` array — render a single muted line: `"No hunks returned by
  plugin."` in `--text-secondary`, same pattern used by the `List` renderer.
- Content line with trailing whitespace — preserved verbatim. Do not trim.
- Very wide lines — per-row horizontal scroll, not component-level, so the
  gutters stay visible. The existing scrollbar tokens (`::-webkit-scrollbar`
  in `globals.css`) already cover this.

---

## (d) Ratatui Render Layout

**File to touch:** `plugins/dust/dust-dashboard/src/component_renderer.rs`.

### Per-hunk layout

```
src/lib.rs  · rust  · 2 hunks                                 <- header line (cream)
────────────────────────────────────────────────────────────
@@ -142,3 +142,4 @@  fn render_component        [a]ccept      <- hunk header line
 142  142     #[serde(tag = "type", rename_all = "snake_case")]
 143  143    pub enum Component {
 144      -    Text {
      144 +    CodeDiff(CodeDiffBody),
      145 +    Text {
────────────────────────────────────────────────────────────
@@ -208,0 +209,2 @@                              [a]ccept      <- hunk 2 header
 208  209  }
      210 +
      211 + // new trailing helper
```

### Column widths

Compute once at render time from `hunks.iter().flat_map(|h| h.lines.len()).max()`:

- `old#` — `old_start + old_count` max digit width, min 3.
- `new#` — `new_start + new_count` max digit width, min 3.
- marker — 2 cols (glyph + space).
- content — remaining width, clipped at area.

### Styling reference points (extend existing intaglio tokens)

Reuse the same RGB triples already used by `AgentTurn` in
`component_renderer.rs:275-325`:

- File header line: `Color::Rgb(0xF2, 0xEA, 0xD7)` (cream) bold for basename;
  the `path`, `language`, and `N hunks` suffix in `Color::DarkGray`.
- Hunk header line: `@@ … @@` in `Color::DarkGray`; `header` (function name) in
  default fg, bold.
- `add` line: `Color::Rgb(0xDA, 0x77, 0x57)` (terracotta) fg, default bg. No
  background fill — terminals vary too much on bg transparency to make that
  legible everywhere.
- `remove` line: `Color::Red` fg. A `-` glyph in column 3.
- `context` line: default fg, no glyph.
- Gutter numbers: `Color::DarkGray`.
- **Selected hunk** (the one the cursor is on): border the whole hunk area
  with a `Block::default().borders(Borders::LEFT).border_style(Style::default().fg(Color::Rgb(0xDA, 0x77, 0x57)))` — a 1-col terracotta left rule, no full box (keeps the layout tight). Show the hint `[a]ccept` at the right end of the header line in bold terracotta *only* when that hunk is selected.

### Selection model

Extend the existing `render_with_selection` entry point with a second
optional argument (or, simpler, add `selected_code_diff_hunk:
Option<(usize /*component idx*/, usize /*hunk idx*/)>`) so the dashboard's
keybinding layer can drive it without re-shaping the function signature for
every new component.

### Keybindings (dashboard-side; owned by the dashboard's input handler, not
`component_renderer.rs`)

| Key | Action |
|-----|--------|
| `j` / `↓` | Move highlight to the next hunk (across components). |
| `k` / `↑` | Move highlight to the previous hunk. |
| `a` | Accept the highlighted hunk — dispatch `code_diff.accept_hunk` (see §e). |
| `x` | Reject (dismiss) the highlighted hunk — no round-trip; the dashboard drops it from local render state. |
| `Enter` | Alias for `a`. |

`a` is chosen over `Enter`-only because `Enter` is already load-bearing for
`List` item activation; a letter key is unambiguous for the diff context.
Both are wired so muscle memory works.

### Height estimation update

Update `component_height` to handle `CodeDiff`:

```rust
Component::CodeDiff { hunks, .. } => {
    // 1 file header + per hunk: 1 header + N lines + 1 separator
    let mut rows: u32 = 1;
    for h in hunks {
        rows += 1 + h.lines.len() as u32 + 1;
    }
    rows.min(u16::MAX as u32) as u16
}
```

Clip at area height as the rest of the file already does.

---

## (e) Accept-Hunk Dispatch Protocol

The protocol reuses the existing `dispatch_action` Tauri command
(`plugins/dust/src-tauri/src/lib.rs:155-190`) and the
`ActionParams` envelope (`dust_core::envelope::ActionParams`, see
`plugins/dust/dust-core/src/envelope.rs:146-158`). No new envelope kind, no
new Tauri command.

### Request shape

From the frontend (React or ratatui) the dispatch is:

```rust
// Frontend builds this:
ActionParams {
    op_id:   Some("code_diff.accept_hunk".into()),
    item_id: Some(hunk.id.clone()),
    args: {
        "path":      Value::String(code_diff.path.clone()),
        "hunk_id":   Value::String(hunk.id.clone()),       // duplicate, see note
        "component_revision": Value::String(component_rev) // optional, see note
    },
}
```

From React, via the Tauri command wrapper — the existing handler lifts `id`
out of `params` into `item_id`, so the frontend just sends:

```ts
await invoke('dispatch_action', {
  pluginId,               // e.g. 'chat'
  capabilityId: '',       // unused here but required by the command signature
  actionId: 'code_diff.accept_hunk',
  params: {
    id: hunk.id,          // becomes ActionParams.item_id via the tauri handler
    path: diff.path,
    hunk_id: hunk.id,     // kept in args for plugins that ignore item_id
  },
})
```

From ratatui, the dashboard calls `Registry::dispatch_action(&plugin_id,
ap)` directly — same `ActionParams`, no JSON-translation layer.

#### Naming conventions

- **`op_id` string**: `"code_diff.accept_hunk"` — namespaced with the
  component type so future actions (`code_diff.reject_hunk` if we ever make
  reject a round-trip, `code_diff.accept_all`) do not collide with other
  plugin actions. The existing `ActionParams::op_id` docstring calls it a
  heartbeat-grouping id — the same string doubles as the "what to do"
  discriminant because the plugin-side handler receives `op_id` anyway and
  there is no separate method slot. Plugins MUST dispatch on `op_id` to
  pick the handler.
- **`item_id`**: the hunk's stable id. The src-tauri command already pulls
  `id` out of `params` into `item_id` — preserve that pattern so existing
  tracker-style dispatches keep working.
- **`args.path` / `args.hunk_id`**: carried redundantly so plugins that do
  not read `ActionParams::item_id` (e.g. a plugin that treats the component
  as the source of truth) can still correlate. Negligible wire cost; cuts a
  class of off-by-one routing bugs.

### Response envelope

The plugin responds with a standard `response` envelope carrying an
`ActionResult`:

```json
{
  "kind": "response",
  "id": "<request-correlation-id>",
  "result": {
    "success": true,
    "message": "applied hunk h-0 to /Users/joey/.../lib.rs",
    "data": {
      "path": "/Users/joey/.../lib.rs",
      "hunk_id": "h-0",
      "bytes_written": 1843
    }
  }
}
```

- `success: true` → frontend transitions the hunk's chip to `[Accepted]`,
  greys out its `Accept`/`Reject` chips, and leaves the hunk body visible.
- `success: false` → frontend shows `message` in `--color-error` / `Color::Red`
  below the hunk header and keeps the chips clickable.
- Envelope-level `error` (plugin crashed, method not found) is surfaced by
  the existing `dispatch_action` error path (`.map_err(|e| e.to_string())`
  at src-tauri/src/lib.rs:189).

### Idempotency

The plugin MUST treat repeat dispatches with the same `(path, hunk_id)` as a
no-op once the hunk is applied. The frontend already greys the chip, but a
double-dispatch can race on flaky networks or user mashing — the emitting
plugin handles this by keeping a set of applied hunk ids per component
render. First-write-wins; second dispatch returns `success: true, data:
{ "already_applied": true }`.

---

## (f) Path-Validation Reuse Strategy

### The existing function

`plugins/dust/src-tauri/src/lib.rs:304-331` defines `validate_path(raw:
&str) -> Result<PathBuf, String>`: canonicalize, canonicalize `$HOME`,
assert `starts_with(home)`. Handles the non-existent-file case by
canonicalizing the parent and rejoining the basename.

### Where it's needed for CodeDiff

Only at accept time, in the plugin process that emits the diff and owns the
write. The ratatui and React renderers never touch the filesystem for
CodeDiff; they dispatch and wait for the `ActionResult`. The src-tauri
`write_file` command is *not* the accept path — we intentionally do not
pipe CodeDiff writes through the shell's generic `write_file`, so no call
to `validate_path` lands there either.

### Decision: inline duplicate in the plugin, pointer comment, no shared crate

For now, duplicate `validate_path` into the plugin that owns the accept
handler (chat, initially) as a private helper, with this doc comment:

```rust
/// Canonicalize `raw` and reject anything that escapes `$HOME`.
///
/// Mirror of `plugins/dust/src-tauri/src/lib.rs:304` — kept in sync by
/// convention until a second plugin needs the same logic, at which point
/// both call sites move to `dust-core::path` (see DESIGN-CODEDIFF.md §f).
fn validate_path(raw: &str) -> Result<std::path::PathBuf, String> { /* … */ }
```

### Why not a shared crate yet

- **Only one caller per side today.** src-tauri has one; after this mission,
  chat has one. Two call sites across two crates is the *minimum* population
  for an abstraction — Anti-Pattern #4 in the architect brief (abstracting
  before the second use).
- **The two sites have different trust models.** src-tauri is gating
  writes from an untrusted frontend (user clicks → arbitrary path). The
  plugin-side validation guards writes from its own agent-loop state —
  still necessary, still the same algorithm, but the review posture is
  different. Sharing the code too early invites one review to rubber-stamp
  the other.
- **The function is 28 lines and has no deps.** Copy cost is low. Drift cost
  is also low as long as both sites include the pointer comment above.

### Trigger to lift into a shared crate

The moment a third caller appears (e.g., a second plugin that writes
files, or the ratatui dashboard sprouts a `write_file` command of its
own), extract into `dust_core::path::validate_inside_home` and update
all call sites in the same PR. Target location: a new `path` module in
`plugins/dust/dust-core/src/` — not a new crate; the logic has no deps
`dust-core` does not already pull in.

---

## (g) End-to-End Test Plan

### Layer 1 — `dust_core` round-trip (unit)

File: `plugins/dust/dust-core/src/lib.rs`, in the existing `#[cfg(test)] mod tests`.

Fixture (construct in Rust):

```rust
fn fixture_code_diff() -> Component {
    Component::CodeDiff {
        path: "/tmp/dust-code-diff-test/fixture.rs".into(),
        basename: "fixture.rs".into(),
        language: Some("rust".into()),
        hunks: vec![Hunk {
            id: "h-0".into(),
            old_start: 1,
            old_count: 2,
            new_start: 1,
            new_count: 3,
            header: Some("fn main".into()),
            lines: vec![
                DiffLine { kind: DiffLineKind::Context, content: "fn main() {".into() },
                DiffLine { kind: DiffLineKind::Remove,  content: "    println!(\"old\");".into() },
                DiffLine { kind: DiffLineKind::Add,     content: "    println!(\"new\");".into() },
                DiffLine { kind: DiffLineKind::Add,     content: "    println!(\"line two\");".into() },
            ],
        }],
    }
}
```

Assertions:

1. `code_diff_serde_roundtrip`: serialize → deserialize → `assert_eq!(before, after)`.
2. `code_diff_wire_tag`: the serialized JSON contains `"type":"code_diff"`.
3. `code_diff_omits_language_when_none`: re-run with `language: None`, assert
   the serialized JSON does not contain `"language"`.
4. `code_diff_omits_header_when_none`: same for `header`.
5. `code_diff_kind_wire_values`: assert the serialized JSON contains
   `"kind":"context"`, `"kind":"add"`, and `"kind":"remove"` — catches an
   accidental rename_all drift.
6. `code_diff_deserializes_from_fixture`: parse a manually-crafted JSON
   blob matching the §a example (copied verbatim) and assert every field.
7. `code_diff_rejects_unknown_kind`: `{ "kind": "sparkle", "content": "…" }`
   must fail — regression guard on the enum.

### Layer 2 — ratatui render (unit)

File: `plugins/dust/dust-dashboard/src/component_renderer.rs`, test module
at the bottom.

1. `code_diff_height_matches_rows`: call `component_height(&fixture)` and
   assert the number against the formula (1 + 1 + lines.len() + 1).
2. `code_diff_rendered_lines_contain_marker_glyphs`: render into a
   `TestBackend` of 80×20 and assert the buffer contains `"+"`, `"-"`, and
   the file basename. Ratatui's `TestBackend` is the existing pattern —
   check the crate already does this elsewhere; if not, the test adds a
   one-time dev-dep on nothing new (ratatui ships `test_backend`).
3. `code_diff_selected_hunk_shows_accept_hint`: render with
   `selected_code_diff_hunk = Some((0, 0))` and assert the rendered buffer
   contains `"[a]ccept"` on the hunk header row.

### Layer 3 — React render (vitest + RTL)

File: `plugins/dust/src/ComponentRenderer.test.tsx` (existing file).

1. `renders basename and path in the header chip`.
2. `renders + and − markers in the marker column per line kind`.
3. `renders line numbers per the unified-diff cursor algorithm` — exact
   text match on the two gutter columns for the `fixture` above.
4. `accept chip calls the action handler with the hunk id` — render with
   a spy `ActionHandler`, click the Accept chip for `h-0`, assert the
   handler was called with `("code_diff.accept_hunk", "h-0")` or whatever
   the agreed signature becomes. This is the seam the frontend engineer
   can stub while the plugin side is still being built.
5. `accepted hunk replaces chips with [Accepted] badge` — drive via a
   prop or local state hook, assert the chips are gone.

### Layer 4 — End-to-end (integration)

Scratch-file setup (bash, run by test harness):

```bash
workdir="$(mktemp -d)/dust-code-diff-e2e"
mkdir -p "$workdir"
cat >"$workdir/fixture.rs" <<'EOF'
fn main() {
    println!("old");
}
EOF
# Force HOME to the tmp parent so validate_path accepts it.
export HOME="$(dirname "$workdir")"
```

Flow:

1. Start a mock "emitting plugin" (a minimal dust-sdk process that on
   `render` returns the fixture `Component::CodeDiff` pointing at
   `$workdir/fixture.rs`, and on `dispatch_action` with
   `op_id = "code_diff.accept_hunk"` applies the hunk and returns
   `ActionResult::ok_with("applied")`).
2. Boot the dashboard with the registry pointed at the mock plugin.
3. Render the component; assert via the TestBackend buffer that both
   hunks appear.
4. Simulate `j` to select hunk 0, then `a`. Assert the dispatch was
   received by the mock plugin with `item_id = "h-0"` and
   `args["path"]` inside `$HOME`.
5. Assert the on-disk contents of `$workdir/fixture.rs` now contain the
   three expected lines.
6. Simulate `a` again on the same (now-applied) hunk. Assert the mock
   returns `success: true, data: { already_applied: true }` and the
   on-disk file is unchanged.

Tear-down: `rm -rf "$workdir"`.

This fixture shape is small enough to live in
`plugins/dust/dust-conformance/` next to the existing conformance harness
(if it exists — if not, either `dust-dashboard/tests/` or a new
`plugins/dust/tests/` dir). The senior-backend-engineer decides which —
the test structure does not depend on that placement.

---

## Component Map

```
 ┌─────────────────────┐        render/action        ┌────────────────────┐
 │ emitting plugin     │◀────────────────────────────┤ dust-registry      │
 │ (e.g. chat)         │                             │                    │
 │                     │     JSON framed msgs        │                    │
 │  - emits CodeDiff   ├────────────────────────────▶│                    │
 │  - handles op       │                             │                    │
 │    "code_diff       │                             └────────┬───────────┘
 │     .accept_hunk"   │                                      │
 │  - validate_path    │                                      ▼
 │    (inline copy)    │              ┌──────────────────────────────────────┐
 └─────────────────────┘              │ Frontend (either, not both at once): │
                                      │                                      │
                                      │  React (src/ComponentRenderer.tsx)   │
                                      │    - RenderCodeDiff                  │
                                      │    - Accept chip → invoke('dispatch_ │
                                      │      action', { actionId:            │
                                      │      'code_diff.accept_hunk', … })   │
                                      │                                      │
                                      │  Ratatui (dust-dashboard/component_  │
                                      │  renderer.rs)                        │
                                      │    - component_renderer::CodeDiff    │
                                      │      arm                             │
                                      │    - input handler: j/k/a/x          │
                                      │      → Registry::dispatch_action     │
                                      └──────────────────────────────────────┘
```

### Interfaces

1. **`dust_core::Component::CodeDiff { path, basename, language?, hunks }`** — §a / §b.
2. **`dust_core::Hunk`, `DiffLine`, `DiffLineKind`** — §b.
3. **Accept dispatch** — `ActionParams { op_id:
   Some("code_diff.accept_hunk"), item_id: Some(hunk.id), args:
   {"path", "hunk_id"} }` — §e.
4. **Accept response** — `ActionResult { success, message?, data: { path,
   hunk_id, bytes_written?, already_applied? } }` — §e.
5. **React types** — `CodeDiffComponent`, `Hunk`, `DiffLine`, `DiffLineKind`
   in `plugins/dust/src/types.ts`.
6. **React renderer** — `RenderCodeDiff({ diff, onAccept, onReject })` in
   `ComponentRenderer.tsx`, where `onAccept = (hunkId: string) => Promise<void>`.

## Risks

1. **Renderer drift between ratatui and React.** If the plugin updates the
   hunk id after a successful accept, the ratatui and React sides must
   handle it identically. Mitigation: the test fixture (§g) is shared —
   both layers run against the same JSON.
2. **Large diffs overwhelm ratatui.** A 2000-line hunk in an 80×24 terminal
   is unusable. Mitigation: out of scope for phase 1 — the renderer clips at
   area height. Future: add a `max_lines_per_hunk` param to the dispatch or
   a "collapse" key (`space`).
3. **Path validation drift.** §f's duplicate will drift if one side gains a
   symlink check the other lacks. Mitigation: the pointer comment, and a
   `grep validate_path plugins/dust -rl` smoke test in CI that asserts both
   copies are byte-identical *except* for the first doc-comment line. If
   even that smells too expensive, rely on the trigger rule to lift into a
   shared crate the moment a second plugin wants the function.
4. **`op_id` collision.** Another plugin might already use `op_id =
   "code_diff.accept_hunk"` for an unrelated action. Low likelihood given
   the namespace, but a grep across existing plugin sources as part of the
   implementation phase is cheap insurance.
5. **`already_applied` round-trip depends on plugin-side bookkeeping.** A
   plugin that restarts loses its applied-hunks set. The frontend's
   `[Accepted]` chip only persists within the render — a restart + re-render
   simply shows the hunk as un-accepted again. Users will accept it again
   and the write is idempotent on the filesystem, so this is a quality-of-life
   issue, not a correctness one. Accepted as-is.

## Trade-offs Accepted

- **Variant count grows.** Adds a ninth variant to `Component`. Every future
  component consumer (SDK examples, hypothetical third-party hosts) has to
  handle it. Worth it because mapping to tables/markdown was a worse fit.
- **Duplicated path validation.** Chose duplication-with-pointer over a
  shared crate today. Will revisit at the second-caller trigger.
- **Accept-only round-trip.** Reject is a client-side dismiss. This means
  the plugin has no way to learn a hunk was rejected, so it cannot self-
  correct. Adding a reject round-trip is cheap (`op_id =
  "code_diff.reject_hunk"`) and can ship later without a protocol change —
  we're not painting ourselves into a corner.
- **`op_id` doubles as the method discriminant.** Keeps the envelope
  untouched. The trade-off is that the existing docstring on
  `ActionParams::op_id` calls it a "progress grouping" id — we're loading
  a second meaning onto it. Acceptable because every existing consumer
  (tracker, chat) already treats `op_id` as "which action", not just
  "which progress group".

<!-- scratch -->
Implementation split for phase-2+:
- backend-phase: add the 3 types + variant to dust-core, update existing
  round-trip test pattern, wire `component_renderer.rs` arm + height, add
  the ratatui selection field. One PR.
- frontend-phase: add types.ts entries + RenderCodeDiff + the CSS tokens
  in globals.css + the RTL tests. One PR. Can land in parallel with the
  backend PR because the wire tests on the Rust side guarantee the JSON
  shape the frontend consumes.
- plugin-phase (chat or whichever emits first): implement the op-id
  handler + inline validate_path copy. Depends on the backend PR being
  merged because it imports `dust_core::Hunk` et al.

Key invariants for implementers:
- hunk.id is opaque to the host — plugins choose format, host compares bytes
- content lines have NO leading +/- marker (kind carries it)
- language is a hint; hosts MAY ignore
- ratatui and React must render the same JSON identically (modulo styling)
- the Accept chip in React and the `a` key in ratatui dispatch the SAME
  op_id string: "code_diff.accept_hunk" — hard-code it in one const per
  side, no stringly-typed drift

DECISION: defer shared-crate extraction of validate_path until a second
caller appears. Revisit at the trigger event described in §f.

DECISION: reject is client-side only for phase 1. Room to add a round-trip
later without protocol change.

Open (for the orchestrator, not blocking the implementers):
- Should `CodeDiff` ever carry a `file_mode` / `binary` flag? Not today.
  Add only when a real use case shows up.
<!-- /scratch -->
