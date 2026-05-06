---
produced_by: architect
phase: phase-1
workspace: 20260421-cd4b862e
created_at: "2026-04-21T00:00:00Z"
confidence: high
depends_on: []
token_estimate: 6200
---

# DESIGN-FILEREF — `Component::FileRef` wire format, hover+expand renderers, and editor hand-off

## Context

Agent-driven plugins (chat, tracker, scout) routinely reference a file-and-line
pair: "see `src/App.tsx:506`", "edit `dust-core/src/lib.rs:190`". Today the
TypeScript side already carries a `FileRefComponent` (`plugins/dust/src/types.ts:83`)
and a renderer (`plugins/dust/src/ComponentRenderer.tsx:337` — `RenderFileRef`)
that emits a chip plus a `⌘⇧E` handler wired to `App.tsx:506` (`handleOpenFile`
→ `QuickEditor`). But:

1. The Rust `Component` enum in `dust-core/src/lib.rs:190` has **no** `FileRef`
   variant — plugins cannot actually emit the component without a wire-level
   addition. The TypeScript type is ahead of the Rust type.
2. The chip carries no preview affordance. An agent reference ("this is at
   `lib.rs:190`") forces the user to open the file in QuickEditor just to see
   context. The standard IDE affordance is a **hover popover** showing a
   tight slice, with an **expand** that grows the slice inline without leaving
   the pane.
3. The ratatui (`dust-dashboard`) renderer does not handle FileRef at all.
4. There is no way to open the file in the user's actual editor of choice —
   `⌘⇧E` only opens the in-app `QuickEditor`. A Tauri command that shells out
   to `$EDITOR` is missing, including the cursor-position flag translation.

This design specifies: (a) the exact `Component::FileRef` wire schema with
defaults for `line`/`basename`; (b) the React layout — chip, hover popover,
expand preview — and the primitive choice for the popover; (c) the ratatui
layout — chip + ±10-line expanded block; (d) three new Tauri commands
(`stat_file`, `preview_file_slice`, `open_in_editor`) with HOME-scoped
validation; (e) editor resolution + per-editor cursor flags; (f) bundle-size
budget + syntax-highlighter choice; (g) the test matrix that proves it works.

## Candidates Considered

| # | Approach | Pros | Cons |
|---|----------|------|------|
| 1 | **`Component::FileRef` with host-side preview commands** (CHOSEN) | Schema matches existing TS type (zero TS churn); host controls hover/expand UX uniformly across plugins; preview uses existing `validate_path` HOME gate; `open_in_editor` reuses the same gate. | Two new Tauri commands; syntax highlighting added for previews. |
| 2 | `Component::Markdown` with a conventional `[basename:line](path)` link | Zero protocol change. | No structured `line`, no hover preview (markdown anchors don't carry slice data), `⌘⇧E` hook point is lost. Defeats the point of having a FileRef variant. |
| 3 | Embed the slice in the wire payload (`{ preview: "..." }`) | No host-side read; preview is free. | Plugin now has to read, slice, and re-emit on every render — stale if the file changes; triples payload size on re-renders; still needs HOME gating for `open_in_editor`; burns plugin sandbox quota. |
| 4 | Preview via a plugin action round-trip (`dispatch_action("file_ref.preview")`) | Uniform with CodeDiff accept protocol. | Adds a sockets round-trip on every hover (≥2ms); plugins have no business knowing the host's viewport or slice radius. Host-side read is the right seam. |

**Selected: #1.** Host-side preview keeps plugins stateless and keeps the wire
schema tiny (three fields). The host already enforces a HOME sandbox for the
QuickEditor `read_file` command — extend the same helper, don't invent a new
policy. Slice-in-payload (#3) is rejected because it couples render freshness
to emit cadence, and #4 pushes viewport concerns into plugins.

## Decision

1. Add `Component::FileRef { path, basename, line }` to `dust_core::Component`
   with serde defaults matching the existing TS type.
2. React: chip ↔ hover popover (raw Tailwind `<div>`, **not** `KeyValue`) ↔
   expand preview rendered by a headless CodeMirror 6 state.
3. Ratatui: inline chip rendered on its own line; `Enter` toggles an expanded
   ±10-line block rendered with ratatui `Paragraph`; no syntax highlight
   (terminals already color text by ANSI, and `ratatui-syntect` is too heavy
   for a TUI that only needs dim/normal tones).
4. Three new Tauri commands — `stat_file`, `preview_file_slice`,
   `open_in_editor` — all routed through the existing `validate_path` HOME
   gate in `plugins/dust/src-tauri/src/lib.rs:304`.
5. Editor resolution: `$EDITOR` → `code` → `vi`, with a small translation
   table that maps the resolved binary name to its cursor-position flag.
6. Bundle budget: **≤2 KB gzipped net delta** across everything. Preview
   highlighting reuses the CodeMirror 6 packages already shipped for the
   QuickEditor (zero incremental framework cost); the popover is plain
   Tailwind (zero incremental dep cost).

---

## (a) Wire Schema — `Component::FileRef`

Type discriminant: `"type": "file_ref"` (snake_case, matches the existing
`#[serde(tag = "type", rename_all = "snake_case")]` attribute on
`dust_core::Component`).

### Fields

| Field | JSON type | Rust type | Presence | Default | Notes |
|-------|-----------|-----------|----------|---------|-------|
| `type` | string literal `"file_ref"` | enum tag | required | — | Emitted by serde from the variant name. |
| `path` | string | `String` | required | — | Absolute path. MUST be inside `$HOME` — validated at **preview time** by the host, not at render time. A path outside HOME renders the chip in a "blocked" tone and disables preview/open (see §c.3). |
| `basename` | string \| null | `Option<String>` | optional | `None` → derived from `path` via `PathBuf::file_name().to_string_lossy()`. On the TS side the derivation is `path.split('/').pop() ?? path`. | Display name shown in the chip. Plugins SHOULD emit it; when absent the host derives it so the TS type's `basename` stays required post-derivation. |
| `line` | integer ≥ 1 \| null | `Option<u32>` | optional | `None` → chip renders without the `:line` suffix and preview renders lines `1..=21` (first 21 lines, centered by the top). | 1-based line number. `0` is a protocol violation; the host treats it as `None`. Line numbers past EOF render the last `21` lines and flag `truncated: true` (see §d.2). |

### Exact Rust variant

```rust
// dust-core/src/lib.rs — in `pub enum Component`
/// A pointer to a file (and optional line) that the user can hover to
/// preview or ⌘⇧E to open in QuickEditor / their external editor.
///
/// The host validates `path` against `$HOME` at preview and open time; the
/// emitting plugin does not need to pre-validate. Agents MUST emit absolute
/// paths — relative paths are rejected by `validate_path`.
FileRef {
    /// Absolute path to the target file. MUST be inside `$HOME`.
    path: String,
    /// Display name for the chip. When `None`, the host derives it from
    /// `path`. Omitted from wire when `None` (`skip_serializing_if`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    basename: Option<String>,
    /// 1-based cursor line. Omitted from wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    line: Option<u32>,
},
```

> **GOTCHA**: the existing TS type declares `basename: string` as required.
> Keep that stance post-derivation: `ComponentRenderer.tsx` normalises the
> incoming payload via `basename ?? path.split('/').pop() ?? path` before
> handing it to `RenderFileRef`. The wire schema is lenient; the React prop
> shape is strict.

### Example JSON

Minimal (line-less, basename derived):

```json
{ "type": "file_ref", "path": "/Users/joey/nanika/README.md" }
```

Full (canonical agent output):

```json
{
  "type": "file_ref",
  "path": "/Users/joey/nanika/plugins/dust/dust-core/src/lib.rs",
  "basename": "lib.rs",
  "line": 190
}
```

### Builder

```rust
impl Component {
    pub fn file_ref(path: impl Into<String>) -> Self {
        Self::FileRef { path: path.into(), basename: None, line: None }
    }
    pub fn file_ref_at(path: impl Into<String>, line: u32) -> Self {
        Self::FileRef { path: path.into(), basename: None, line: Some(line) }
    }
}
```

### Serde invariants

- `basename: None` → wire omits the field (existing TS reader treats it as
  `undefined` and falls back to host derivation).
- `line: None` → wire omits the field; TS reader renders the chip without the
  `:<line>` suffix.
- `line: 0` on the wire → decoded as `Some(0)`, then **normalised to `None`**
  inside the React adapter. Round-trip tests in §g cover this.

---

## (b) React Render Layout

Implemented in `plugins/dust/src/ComponentRenderer.tsx`, exported as the same
`RenderFileRef` already present at line 337. The existing renderer becomes
the **chip-only** base; hover + expand states are wrapped around it.

### b.1 State machine

```
              mouseEnter(>350ms)     click / Enter (focus)
    ╭── chip ─────────────────────► popover ──────────────────► expanded
    │                                  │                           │
    │           mouseLeave(>120ms)     │      click outside /      │
    ╰◄─────────────────────────────────╯      Esc                  │
                                         ╰◄───────────────────────╯
```

- **chip** — baseline; matches today's rendering (rounded pill, file icon,
  `basename:line`).
- **popover** — hover-only; a 360 × ~180 px floating panel showing a
  10-line slice centered on `line` (5 above, 5 below) with syntax highlighting
  and a dimmed gutter.
- **expanded** — click-through from popover OR keyboard `Enter` when the chip
  is focused. Renders inline in-flow (pushes siblings down) with the same
  ±10-line slice but in a 760 × ~320 px container; includes an "Open in
  editor" button (calls `open_in_editor`) and a "⌘⇧E to open inline"
  hint.

`⌘⇧E` (`(metaKey|ctrlKey) && shiftKey && key==='E'`) is handled on the
**chip** and on the **expanded** container — both dispatch the existing
`onOpenFile(path, basename, line)` handler that opens `QuickEditor`.

### b.2 Primitive choice — popover

**Choice: raw Tailwind `<div>`, not `KeyValue`.**

`KeyValue` (`Component::KeyValue`) is a **protocol-level component**: it is
something a plugin emits on the wire for the host to render as label/value
pairs. It is not a host UI primitive — composing it inside another renderer
would mean synthesising `KVPair` structs from `path`/`line`/`lang`/`mtime`
just to feed them to the list renderer, and then styling it to not look like
a KeyValue (no label-column ceremony, no forced row breaks, dimmed value).
That is fighting the primitive.

A raw `<div>` with Tailwind classes (we already ship Tailwind) is:
- ~40 LOC of markup,
- positioned with `position: absolute` + `bottom: calc(100% + 6px)` + flex
  centering (no Floating UI dep),
- transparent to keyboard focus (the chip stays the focus target; the popover
  is `aria-describedby` linked),
- dismissable via `mouseleave` on the chip's hover root AND via `Esc` when
  expanded has focus.

A header row inside the popover **does** carry label/value metadata (path,
mtime, size) and could be tempted to reach for `KeyValue` — resist. The
popover header is three spans in a flex row with `text-[11px] text-neutral-400`.
Keep `KeyValue` reserved for plugin-emitted content.

### b.3 Chip layout

```
 ┌──────────────────────────────────┐
 │ 📄 basename.ext :NNN             │   (existing — no structural change)
 └──────────────────────────────────┘
```

- Hover: 1px border brightens from `--pill-border` to `--text-secondary`.
- Focus visible: 2px ring in `--intaglio-terracotta`.
- `title` attribute removed (replaced by the popover; leaving it in causes a
  native tooltip to race the custom one on macOS).

### b.4 Popover layout

```
 ╔════════════════════════════════════════════════════════════╗
 ║ lib.rs · 7.4 KB · modified 3m ago                         ║   ← header (11px, neutral)
 ╠════════════════════════════════════════════════════════════╣
 ║ 185 │ // ── Component ─────────────────                     ║
 ║ 186 │                                                        ║
 ║ 187 │ /// UI components that a plugin can render …          ║
 ║ 188 │ #[derive(Debug, Clone, PartialEq, Serialize, …)]      ║
 ║ 189 │ #[serde(tag = "type", rename_all = "snake_case")]     ║
 ║ 190►│ pub enum Component {                          ◄       ║   ← highlighted target line
 ║ 191 │     Text {                                            ║
 ║ 192 │         content: String,                              ║
 ║ 193 │         #[serde(default, skip_serializing_if = …)]   ║
 ║ 194 │         style: TextStyle,                             ║
 ║ 195 │     },                                                ║
 ╠════════════════════════════════════════════════════════════╣
 ║ ⌘⇧E QuickEdit   ·   ⌥↵ Open in editor   ·   ↵ Expand      ║   ← footer
 ╚════════════════════════════════════════════════════════════╝
```

- **Header**: `basename · <size bytes formatted>` · `modified <relative time>`
  — populated by `stat_file` (§d.1).
- **Body**: ±5 lines around `line` (11 rows total), rendered by
  `<PreviewSlice>` (see §f) with a dimmed gutter column and the target line
  highlighted (`background: var(--slice-hl-bg)`).
- **Footer**: keybind hints in `text-[10px] text-neutral-500`.

### b.5 Expanded preview layout

Same content as the popover but:
- Inline in-flow (not absolute), occupies the full width of the detail pane
  minus 16 px padding.
- Shows ±10 lines (21 rows total; matches ratatui — see §c).
- Gutter widens to accommodate 4-digit line numbers up to 9999.
- Adds an "Open in external editor" `<button>` on the right of the footer
  that calls `invoke('open_in_editor', { path, line })` and closes the
  popover on success.

### b.6 State & hooks

```ts
type FileRefState = 'chip' | 'hover' | 'expanded'
type SliceData = { startLine: number; lines: string[]; truncated: boolean }
type StatData  = { size: number; mtimeMs: number; lines: number }

function RenderFileRef(props: {
  path: string
  basename?: string
  line?: number
  onOpenFile?: OpenFileHandler
}) {
  const effectiveBasename = props.basename ?? props.path.split('/').pop() ?? props.path
  const [state, setState] = useState<FileRefState>('chip')
  const [slice, setSlice] = useState<SliceData | null>(null)
  const [stat,  setStat]  = useState<StatData  | null>(null)
  const [error, setError] = useState<string | null>(null)
  // ...hover timers (350 ms in, 120 ms out), keybinds, focus trap on expanded
}
```

- Slice + stat are fetched **lazily** on the first hover and memoised in the
  component instance for the lifetime of the mount. `stat_file` and
  `preview_file_slice` are issued in **parallel** via `Promise.all`.
- If `stat_file` returns `path outside HOME`, the chip renders in blocked
  tone (crossed-out icon + `text-neutral-500`) and the popover is suppressed
  entirely. `onOpenFile` remains disabled.
- A 2-second inactivity timer on `expanded` state does **not** auto-collapse;
  collapse is user-driven (Esc, click outside, re-click chip).

---

## (c) Ratatui Render Layout

Implemented as a new match arm in `dust-dashboard/src/component_renderer.rs`,
next to the existing `Component::CodeDiff` arm at line 388.

### c.1 Chip (collapsed)

One line, rendered with three ratatui `Span`s:

```
  📄 lib.rs:190
```

- `📄 ` — file glyph span, `Style::default().dim()`.
- `basename` — span with `Style::default().fg(intaglio_terracotta())` if the
  chip is focus-selected, else default fg.
- `:<line>` — span with `Style::default().dim()`; omitted when `line` is
  `None`.

The component occupies exactly **1 row** when collapsed — `measure_height()`
returns `1`.

### c.2 Expanded (±10-line block)

Triggered by `Enter` when the FileRef is the focused component. Appearance:

```
  📄 lib.rs:190                                                         ▲
  ┌───────────────────────────────────────────────────────────────────┐
  │ 180 │ pub struct KVPair { …                                        │
  │ 181 │     pub label: String,                                       │
  │ 182 │     pub value: String,                                       │
  │ 183 │ }                                                            │
  │ 184 │                                                              │
  │ 185 │ // ── Component ─────────────────                            │
  │ 186 │                                                              │
  │ 187 │ /// UI components that a plugin can render …                │
  │ 188 │ #[derive(Debug, Clone, PartialEq, Serialize, …)]            │
  │ 189 │ #[serde(tag = "type", rename_all = "snake_case")]           │
  │►190 │ pub enum Component {                                         │
  │ 191 │     Text {                                                  │
  │ 192 │         content: String,                                    │
  │ 193 │         #[serde(default, skip_serializing_if = …)]         │
  │ 194 │         style: TextStyle,                                   │
  │ 195 │     },                                                      │
  │ 196 │     List {                                                  │
  │ 197 │         items: Vec<ListItem>,                               │
  │ 198 │         …                                                    │
  │ 199 │     },                                                      │
  │ 200 │     …                                                        │
  └───────────────────────────────────────────────────────────────────┘
  ⏎ Collapse   ·   e Open in editor
```

- Slice radius: **±10** lines → up to 21 rows of content + 1 header + 1 footer
  = **23 rows** when `line = Some(k)` with `k ≥ 11`. When `line ≤ 10` (near
  BOF) the top gutter is clipped; same near EOF. `measure_height()` returns
  `1` when collapsed and `slice_rows + 2` when expanded.
- Slice data is fetched via the **existing plugin registry read path** — no
  new envelope. The dashboard calls a host-side helper
  `dust_host::read_slice(path, around, 10)` that wraps the same `validate_path`
  + `fs::read_to_string` logic used by `read_file`; this helper lives in
  `dust-dashboard/src/fileref.rs` (new, ~60 LOC).
- Gutter number style: `Style::default().dim()`. Target-line marker: a `►`
  glyph in the gutter column + `Style::default().bg(hl_bg())` on the content
  span. Line numbers right-aligned, width = digits of `start_line + 20`.
- No syntax highlighting. (Reject: `syntect` + `ratatui-syntect` add ~3 MB
  to the TUI binary for a ±10-line preview; terminal readers already parse
  code fine at monochrome.)

### c.3 Blocked (path outside HOME)

When the host helper returns `Err("path outside allowed root (HOME)")`:

- Chip renders with `🚫` glyph in place of `📄`, `basename` fg = `dim()`,
  `:<line>` suffix suppressed.
- `Enter` on the chip is a no-op (expansion disabled).
- Focus tooltip (bottom status bar) shows `sandbox: outside $HOME`.

### c.4 Keybindings (dashboard)

| Key | Action | Scope |
|-----|--------|-------|
| `Enter` | Toggle collapsed ↔ expanded | FileRef focused |
| `e` | Shell out via `open_in_editor` (spawned detached from TUI) | FileRef focused |
| `Esc` | Collapse if expanded | FileRef expanded |

The `e` binding does **not** suspend the TUI (editor is spawned detached and
the dashboard keeps running). Rationale: the TUI is a read surface; writes
go through the editor, not through ratatui.

---

## (d) Tauri Commands

All three new commands are `#[tauri::command]`s in
`plugins/dust/src-tauri/src/lib.rs` and registered in the
`tauri::generate_handler![...]` array at line 567. Every command routes its
`path` argument through the existing `validate_path` helper
(`plugins/dust/src-tauri/src/lib.rs:304`) — **no new sandbox policy**.

### d.1 `stat_file`

```rust
#[derive(Debug, Serialize)]
pub struct FileStat {
    pub size: u64,       // bytes
    pub mtime_ms: u64,   // unix milliseconds
    pub lines: u32,      // total line count (cap at u32::MAX)
}

#[tauri::command]
async fn stat_file(path: String) -> Result<FileStat, String>
```

**TypeScript call site:**

```ts
const stat = await invoke<{ size: number; mtime_ms: number; lines: number }>(
  'stat_file', { path }
)
```

**Behaviour:**
1. `let safe = validate_path(&path)?;` — rejects non-HOME paths with
   `"path outside allowed root (HOME): <canonical>"`.
2. `let md = std::fs::metadata(&safe).map_err(|e| e.to_string())?;`
3. `lines`: stream-read, count `\n` (not `read_to_string` — bounds memory at
   8 KB buffer for files up to the `FILE_REF_MAX_BYTES` cap of 4 MiB).
4. Files larger than 4 MiB return `"file too large: <N> bytes (max 4 MiB)"`
   without reading.
5. `mtime_ms`: `md.modified()?.duration_since(UNIX_EPOCH)?.as_millis()` with a
   `u64` saturating cast.

### d.2 `preview_file_slice`

```rust
#[derive(Debug, Deserialize)]
pub struct PreviewArgs {
    pub path: String,
    /// 1-based center line. Clamped to [1, total_lines] by the host.
    pub around_line: u32,
    /// Symmetric radius. MUST be in [0, 50]; higher values are clamped.
    pub radius: u32,
}

#[derive(Debug, Serialize)]
pub struct FileSlice {
    pub start_line: u32,     // 1-based; may be < around_line - radius near BOF
    pub lines: Vec<String>,  // len ≤ 2*radius + 1
    pub truncated: bool,     // true when around_line clamped or EOF hit
    pub total_lines: u32,    // for popover's "X of Y" readouts
}

#[tauri::command]
async fn preview_file_slice(args: PreviewArgs) -> Result<FileSlice, String>
```

**Behaviour:**
1. `validate_path(&args.path)?` — same HOME gate.
2. Read the file once with `std::io::BufReader::new(File::open(safe)?)` and
   stream line-by-line; skip lines outside `[start, end]` without allocating.
3. `start = max(1, around_line.saturating_sub(radius))`,
   `end = min(total_lines, around_line + radius)`. `around_line == 0` is
   treated as `1` (React sends `0` when the wire `line` is missing).
4. If `around_line > total_lines`, set `truncated = true`, clamp to
   `total_lines`, and return the last `2*radius+1` lines.
5. If the file has no `\n`, `lines = [content]`, `total_lines = 1`.
6. Binary detection: if any chunk of the first 4 KiB contains a null byte,
   return `"binary file: preview unavailable"` (the React popover renders
   this message in place of the slice).

### d.3 `open_in_editor`

```rust
#[derive(Debug, Deserialize)]
pub struct OpenArgs {
    pub path: String,
    pub line: Option<u32>,
}

#[tauri::command]
async fn open_in_editor(args: OpenArgs) -> Result<(), String>
```

**Behaviour:**
1. `validate_path(&args.path)?`.
2. Resolve the editor command via §e below.
3. Translate the cursor position using the per-editor flag table in §e.
4. Spawn the process **detached** with
   `std::process::Command::new(bin).args(argv).spawn()?;` — do not hold the
   handle (no zombie reaping concern; the OS reparents to `launchd`/`init`).
5. Return `Ok(())` as soon as `spawn()` succeeds; never wait for exit.
6. On failure (binary not found, spawn EACCES, etc.), return a
   `"cannot open editor '<bin>': <err>"` string so the React layer can toast.

**HOME-scope rationale:** even though `open_in_editor` only *names* the path
(it does not read it), we still refuse paths outside HOME to keep the sandbox
boundary consistent with read/write. An agent that needed to hand the user a
path outside HOME would be laundering it through `open_in_editor` as a side
channel; we don't want that.

---

## (e) Editor Resolution + Cursor Flags

### e.1 Resolution order

1. `$EDITOR` environment variable, if set **and** the first token resolves to
   an executable on `$PATH`. Supports spaces (`EDITOR="code -w"`): parsed
   with a minimal POSIX-shell tokeniser that respects single/double quotes
   but rejects `;`, `|`, `&`, `` ` ``, `$` — anything beyond a command +
   args is refused with `"editor contains unsafe shell chars"`. (We don't
   run a shell, so metachars are meaningless but reject-on-sight is the
   safer default.)
2. `code` — if `which code` succeeds.
3. `vi` — fallback. `vi` is POSIX-mandated; if it is not on `$PATH` we
   return `"no editor available (tried $EDITOR, code, vi)"`.

Resolution is computed once per `open_in_editor` call — do not cache across
calls, since `$EDITOR` can change between invocations (users running
`EDITOR=subl dust` mid-session is a real flow).

### e.2 Per-editor cursor-position flag

The resolved binary's basename (final path component) is matched against
this table. Unknown binaries get **no cursor flag** — the file opens at the
last cursor the editor remembers. We never guess.

| Editor basename | Cursor flag translation | Notes |
|-----------------|-------------------------|-------|
| `code`, `cursor`, `code-insiders`, `windsurf` | `code -g <path>:<line>` | VS Code family; `-g` = goto. |
| `vi`, `vim`, `nvim`, `mvim` | `vim +<line> <path>` | `+<line>` is POSIX. |
| `nano` | `nano +<line> <path>` | |
| `emacs`, `emacsclient` | `emacs +<line> <path>` | `+<line>:<col>` is also accepted but we omit col. |
| `hx` (Helix) | `hx <path>:<line>` | Same form as `code` but no `-g`. |
| `subl`, `sublime_text` | `subl <path>:<line>` | |
| `micro` | `micro +<line> <path>` | |
| `zed` | `zed <path>:<line>` | |
| `idea`, `webstorm`, `pycharm`, `rustrover` | `<bin> --line <line> <path>` | JetBrains family. |
| anything else | `<bin> <path>` (no line flag) | Safe fallback; opens at last cursor. |

When `line` is `None`, all variants drop the cursor flag entirely:

| Editor | No-line argv |
|--------|--------------|
| `code` family | `code <path>` |
| `vi` family | `vim <path>` |
| everything else | `<bin> <path>` |

### e.3 Split-token parsing for `$EDITOR`

```rust
fn parse_editor(raw: &str) -> Result<Vec<String>, String> {
    // Reject unsafe chars up front.
    if raw.chars().any(|c| matches!(c, ';' | '|' | '&' | '`' | '$' | '\n')) {
        return Err("editor contains unsafe shell chars".into());
    }
    // Split on unquoted whitespace; honour ' and " for paths with spaces.
    shlex::split(raw).ok_or_else(|| "editor parse failed".to_string())
}
```

Using `shlex` (adds ~6 KB to the tauri binary — one-time cost, not part of
the React bundle budget in §f).

### e.4 Compose final argv

```rust
fn compose_argv(bin: &str, path: &Path, line: Option<u32>, leading_args: &[String])
    -> Vec<String>
{
    let basename = Path::new(bin).file_name().unwrap_or_default()
        .to_string_lossy().to_lowercase();
    let path_s = path.to_string_lossy().to_string();
    let mut argv = leading_args.to_vec();
    match (line, basename.as_str()) {
        (Some(l), "code" | "cursor" | "code-insiders" | "windsurf") => {
            argv.push("-g".into());
            argv.push(format!("{path_s}:{l}"));
        }
        (Some(l), "vi" | "vim" | "nvim" | "mvim" | "nano" | "emacs"
                | "emacsclient" | "micro") => {
            argv.push(format!("+{l}"));
            argv.push(path_s);
        }
        (Some(l), "hx" | "subl" | "sublime_text" | "zed") => {
            argv.push(format!("{path_s}:{l}"));
        }
        (Some(l), "idea" | "webstorm" | "pycharm" | "rustrover") => {
            argv.push("--line".into());
            argv.push(l.to_string());
            argv.push(path_s);
        }
        _ => argv.push(path_s),
    }
    argv
}
```

Unit tests cover each row of the table (§g).

---

## (f) Bundle-Size Budget + Syntax Highlighter

### f.1 Budget

| Surface | Gzipped delta | Status |
|---------|---------------|--------|
| `RenderFileRef` (chip + popover + expanded container) | ≤ 1.4 KB | New host code only; uses existing Tailwind classes. |
| `<PreviewSlice>` (syntax highlighter, see below) | ≤ 0 KB incremental | Reuses CodeMirror 6 packages already bundled for QuickEditor. |
| Per-language CodeMirror lang pack (e.g. `lang-rust`) | 0 KB incremental *per language* | Already code-split and lazy-loaded for QuickEditor (`plugins/dust/src/QuickEditor.tsx:15–65`). Reuse the same dynamic imports. |
| `types.ts` schema change (`basename?`) | ≈ 0 KB | Type-only. |
| **Net frontend delta** | **≤ 2 KB gzipped** | Hard budget. CI enforces (§g). |

The Rust side grows by ~200 LOC for the three commands + resolver (+shlex
~6 KB uncompressed in the tauri binary); not part of the frontend budget.

### f.2 Syntax highlighter — decision

**Choice: headless CodeMirror 6 + existing `@codemirror/lang-*` packages,
rendered as static HTML via `highlightTree`.**

Candidates evaluated:

| # | Option | Bundle delta | Trade-off |
|---|--------|--------------|-----------|
| 1 | **Headless CodeMirror (`@lezer/highlight` `highlightTree` over an already-loaded lang-pack)** (CHOSEN) | **0 KB incremental** | Same parser used by QuickEditor; no new dep. Static render — no DOM editor instance, no diff/state overhead. |
| 2 | Shiki (`shiki` + themes) | +180 KB gzipped | Best quality highlighting, but blows the budget 90×. |
| 3 | Prism (`prismjs` + needed langs) | +30 KB gzipped | 15× the budget; doubles the grammar inventory we already ship via CodeMirror. |
| 4 | Highlight.js (`highlight.js/lib/core` + langs) | +25 KB gzipped | Same problem as Prism. |
| 5 | No highlighting, plain `<pre>` | 0 KB | Legible but visually muddy next to the themed QuickEditor; rejected for UX consistency, not bundle cost. |

Headless CodeMirror works like this:

```ts
import { EditorState } from '@codemirror/state'
import { highlightTree, classHighlighter } from '@lezer/highlight'

// resolveLanguage() is the same helper already exported from QuickEditor.tsx.
async function renderSlice(text: string, ext: string) {
  const lang = await resolveLanguage(ext)
  if (!lang) return escapeHtml(text)            // fallback: plain <pre>
  const tree = lang.language.parser.parse(text)
  let out = ''
  let cursor = 0
  highlightTree(tree, classHighlighter, (from, to, cls) => {
    if (from > cursor) out += escapeHtml(text.slice(cursor, from))
    out += `<span class="${cls}">${escapeHtml(text.slice(from, to))}</span>`
    cursor = to
  })
  if (cursor < text.length) out += escapeHtml(text.slice(cursor))
  return out
}
```

`classHighlighter` emits class names like `tok-keyword`, `tok-string`,
`tok-comment`. A 70-line CSS block in `globals.css` maps them to the
intaglio palette (`--intaglio-terracotta` for keywords, `--text-secondary`
for strings, `--color-muted` for comments). No JS theme engine needed.

> **DECISION:** refactor `resolveLanguage` out of `QuickEditor.tsx` into a
> shared `src/languagePacks.ts` so both the editor and the preview slice
> use the same dynamic import table. This is a **blocking** prereq — the
> preview must not fork the language list.

### f.3 CI enforcement

Add a `size-limit` entry to `plugins/dust/package.json`:

```json
"size-limit": [
  { "path": "dist/assets/*.js", "limit": "220 KB" }
]
```

Current baseline (pre-FileRef) measured on the current `main` at HEAD:
measure once when this design lands, set `limit` = baseline + 2 KB. PR
fails if the delta exceeds the budget.

---

## (g) Test Matrix

### g.1 React component tests (Vitest + Testing Library)

Added to `plugins/dust/src/ComponentRenderer.test.tsx` following the existing
structure (`FileRef` suite, mirroring the `CodeDiff` and `ToolCallBeat`
suites already present).

| # | Test | Assertion |
|---|------|-----------|
| 1 | renders chip with basename + line | `getByText('lib.rs')`, `getByText(':190')` |
| 2 | derives basename when `basename` is absent from wire | `getByText('lib.rs')` (derived from path) |
| 3 | omits `:line` suffix when `line` is undefined | no `:` in the chip |
| 4 | treats `line: 0` as absent | no `:0` in the chip |
| 5 | `⌘⇧E` on focused chip calls `onOpenFile(path, basename, line)` | mocked handler called once with exact args |
| 6 | hover (`fireEvent.mouseEnter` + advance timers 350 ms) opens popover | `findByRole('dialog')` resolves |
| 7 | hover issues `stat_file` + `preview_file_slice` in parallel | mocked `invoke` called 2× with correct args; timestamps within 1 ms |
| 8 | `stat_file` rejection with "outside HOME" renders blocked chip | chip has `data-blocked="true"`; popover does not open on subsequent hover |
| 9 | click on popover transitions to expanded | `findByText('Open in external editor')` |
| 10 | `Esc` on expanded returns focus to chip | `document.activeElement === chip` |
| 11 | "Open in external editor" button invokes `open_in_editor` with `{ path, line }` | mocked `invoke` called with correct payload |
| 12 | slice fetch error renders inline error row (`binary file`, `too large`) | error text visible; no console noise |
| 13 | language pack resolution does not fork `QuickEditor.tsx` | import from `./languagePacks` asserted via module spy |
| 14 | no `title=` attribute set (prevents native tooltip race) | `container.querySelector('[title]')` returns `null` |
| 15 | hover leaves within 120 ms before open cancels fetch | `invoke` never called |

### g.2 Rust renderer tests (`dust-dashboard`)

Added to `dust-dashboard/src/component_renderer.rs` test module; use a
`TestBackend` from ratatui at 80×40 and assert on the rendered buffer via
`buffer.assert_eq` or by stringifying rows.

| # | Test | Assertion |
|---|------|-----------|
| 1 | `measure_height(FileRef)` returns 1 when collapsed | `assert_eq!(height, 1)` |
| 2 | `measure_height(FileRef)` returns 23 when expanded with `line=Some(50)` | `assert_eq!(height, 23)` |
| 3 | collapsed chip renders `📄 lib.rs:190` | cell 0,0..14 matches `"📄 lib.rs:190 "` |
| 4 | collapsed chip omits `:line` when `line` is `None` | no `:` cell present |
| 5 | expanded renders target-line marker `►` in gutter | cell at target row gutter col is `'►'` |
| 6 | expanded clips near BOF (`line=Some(3)`) — shows rows 1..=13 | first body row prefix is `"  1"` |
| 7 | expanded clips near EOF — `truncated: true` sets end at EOF | last body row equals last file line |
| 8 | blocked path renders `🚫` glyph + dim basename | cell 0,0 is `'🚫'`, basename span is dim |
| 9 | `Enter` on focused FileRef toggles state | snapshot before/after `Enter` differ |
| 10 | `e` key on focused FileRef spawns `open_in_editor` (mock host) | mock spawn called with correct argv |

### g.3 Rust Tauri command tests (`src-tauri`)

Integration tests under `plugins/dust/src-tauri/tests/fileref.rs` (new file),
running against a fixture dir inside a temp HOME (`tempfile::TempDir` +
`std::env::set_var("HOME", tmp.path())`).

| # | Test | Assertion |
|---|------|-----------|
| 1 | `stat_file` on file inside HOME | returns size, mtime, lines matching fs truth |
| 2 | `stat_file` on file outside HOME | `Err` contains `"outside allowed root"` |
| 3 | `stat_file` on 5 MiB file | `Err` contains `"too large"` |
| 4 | `preview_file_slice` radius=5, around=10 | returns lines 5..=15, `truncated=false` |
| 5 | `preview_file_slice` around=1 radius=10 | `start_line=1`, 11 lines, `truncated=false` |
| 6 | `preview_file_slice` around past EOF | `truncated=true`, last `2*radius+1` lines |
| 7 | `preview_file_slice` on binary fixture | `Err` contains `"binary file"` |
| 8 | `preview_file_slice` radius=100 | radius clamped to 50 |
| 9 | `open_in_editor` with `$EDITOR=code` | mocked `Command::spawn` receives argv `["code", "-g", "<path>:5"]` |
| 10 | `open_in_editor` with `$EDITOR=vim` line=`Some(7)` | argv `["vim", "+7", "<path>"]` |
| 11 | `open_in_editor` with `$EDITOR="code -w"` line=None | argv `["code", "-w", "<path>"]` |
| 12 | `open_in_editor` with `$EDITOR="code; rm -rf /"` | returns `"unsafe shell chars"` |
| 13 | `open_in_editor` with `$EDITOR` unset, `code` on PATH | argv starts with `"code"` |
| 14 | `open_in_editor` with nothing on PATH | `Err` contains `"no editor available"` |
| 15 | `open_in_editor` path outside HOME | `Err` contains `"outside allowed root"` |
| 16 | JetBrains `$EDITOR=idea` line=`Some(42)` | argv `["idea", "--line", "42", "<path>"]` |
| 17 | unknown editor `$EDITOR=myedit` line=`Some(42)` | argv `["myedit", "<path>"]` — no line flag |

### g.4 End-to-end smoke (Playwright or Tauri `cargo tauri test`)

Single happy-path scenario driven against a built tauri binary with a
fixture plugin that emits a FileRef in its render. Lives under
`plugins/dust/e2e/fileref.spec.ts`.

| Step | Action | Gate |
|------|--------|------|
| 1 | Launch dust with `DUST_FIXTURE_PLUGIN=fileref-fixture` | window visible, capability list populated |
| 2 | Select the fixture's capability; detail pane renders an agent turn containing `Component::FileRef` | chip visible with basename+line |
| 3 | Hover the chip for 400 ms | popover visible; header shows file size & mtime |
| 4 | Click popover | expanded preview in-flow; footer shows "Open in external editor" |
| 5 | Press `⌘⇧E` | `QuickEditor` mounts and the content matches the fixture file |
| 6 | Dismiss QuickEditor (`Esc`); click "Open in external editor" with `EDITOR=$(which echo)` | `open_in_editor` invoked; mocked editor receives argv (assert via log file the fixture writes) |

**Success criterion for the end-to-end:** steps 1→5 complete in under 1500 ms
on a cold window (matches the latency bucket in `scripts/shell-bench.sh`
that §c references). CI regression threshold: 2000 ms.

---

## Interfaces Summary

```rust
// dust-core
pub enum Component {
    /* … */
    FileRef {
        path: String,
        basename: Option<String>,
        line: Option<u32>,
    },
}

// src-tauri
#[tauri::command] async fn stat_file(path: String) -> Result<FileStat, String>;
#[tauri::command] async fn preview_file_slice(args: PreviewArgs) -> Result<FileSlice, String>;
#[tauri::command] async fn open_in_editor(args: OpenArgs) -> Result<(), String>;
```

```ts
// src/types.ts (update)
export type FileRefComponent = {
  type: 'file_ref'
  path: string
  basename?: string            // was required; now optional to match wire
  line?: number
}

// src/tauri.ts (new helper)
export const statFile      = (path: string) => invoke<FileStat>('stat_file', { path })
export const previewSlice  = (a: PreviewArgs) => invoke<FileSlice>('preview_file_slice', { args: a })
export const openInEditor  = (path: string, line?: number) =>
  invoke<void>('open_in_editor', { args: { path, line } })
```

## Risks

1. **Parser-fork drift** — If `resolveLanguage` in `QuickEditor.tsx` and the
   preview slice renderer diverge, a file will highlight differently in the
   popover vs the editor. Mitigation: extract to `src/languagePacks.ts` (§f)
   and gate on unit test g.1-13. Acceptance risk: **low** once extracted.
2. **Popover flicker on fast mouse transit** — 350 ms open / 120 ms close
   hysteresis is tuned for trackpad; tablet taps may feel laggy. Mitigation:
   treat `pointerType === 'touch'` as immediate-open, skip popover and go
   straight to expanded. Acceptance risk: **medium**, deferred follow-up.
3. **`$EDITOR` shell metachars** — Users with `EDITOR="code -w"` are common;
   users with `EDITOR="sh -c 'code %'"` exist. We refuse the latter (`;|&$`)
   — some users will complain. Mitigation: the error is explicit, and the
   docs tell them to set `EDITOR=code -w` directly. Acceptance risk: **low**.
4. **Path canonicalisation outside HOME via symlink** — `validate_path`
   canonicalises before comparing against HOME; a symlink in HOME pointing
   outside HOME is treated as outside (correct). But a symlink **outside**
   HOME pointing inside HOME is allowed if the plugin emits the pre-canonical
   path. Mitigation: existing helper already canonicalises first — no change.
5. **Slice cost on large files** — Files up to 4 MiB are allowed; reading
   them line-by-line to find line N is O(file). For a 4 MiB source file
   (~100 k lines) at 50 hovers/s this is dramatic. Mitigation: the 4 MiB cap
   bounds the worst case at ~20 ms on the tested hardware; hover debounce
   (350 ms) absorbs the rest. Budget is acceptable; not worth adding an
   index. Acceptance risk: **low**.
6. **Cross-platform editor resolution** — `which` on Windows needs `.exe`
   fallback. Mitigation: `which` crate handles this; dust is macOS-first
   for now. Acceptance risk: **low**.

## Trade-offs Accepted

- **Host-side preview reads** (not plugin-side) — simpler protocol, but the
  dust host now touches the filesystem directly. Already true for
  `read_file`/`write_file`; not a new boundary.
- **No col in cursor translation** — some editors accept `path:line:col`
  but the FileRef schema carries no column. Adding `col?: u32` to the
  schema is a future lane; today, `line` alone is enough.
- **Headless CodeMirror over Shiki** — lower highlight fidelity (no
  semantic tokens, no inline doc-comment rendering) in exchange for zero
  bundle growth. Acceptable at the preview scale (±10 lines); QuickEditor
  continues to use the full CodeMirror instance.
- **One detail-pane FileRef expanded at a time is NOT enforced** — React
  allows two independent FileRef components to both be expanded. This is
  fine for the default window (520 px tall) only when components scroll;
  the expanded container adds a `max-h-[320px]` + `overflow-auto` to keep
  layout stable. Accepted: layout policy lives in CSS, not component state.
- **Ratatui chip uses no syntax highlight** — documented above; saves
  binary size and matches TUI aesthetic.

<!-- scratch -->
Key decisions for the implementer:
1. Add `Component::FileRef { path, basename: Option<String>, line: Option<u32> }` to dust-core/src/lib.rs near the existing CodeDiff variant (line 259). Match arms to add in dust-dashboard/src/component_renderer.rs at lines 388 (render) and 798 (measure_height).
2. types.ts: change `basename: string` → `basename?: string` (breaking TS consumers will be flagged at compile; only ComponentRenderer.tsx consumes it).
3. Extract `resolveLanguage` from QuickEditor.tsx:15-65 into `src/languagePacks.ts` BEFORE adding the preview — both QuickEditor and <PreviewSlice> must import from the same module.
4. Hover popover MUST be a raw div, NOT Component::KeyValue (KeyValue is a wire-level primitive, not a host UI primitive). The popover header is three flex spans, not a KeyValue list.
5. `validate_path` already exists at src-tauri/src/lib.rs:304 — reuse, do not duplicate. Three commands all flow through it.
6. Editor resolution: `$EDITOR` tokeniser uses `shlex`; reject `;|&$\`\n` even though we don't pass to a shell (defence in depth).
7. Bundle budget hard cap is 2 KB gzipped — CI must enforce via size-limit; baseline before FileRef lands, then set limit to baseline + 2 KB.
8. Test matrix totals 15 React + 10 Rust renderer + 17 Tauri command + 6 e2e steps = 48 assertions covering every branch called out in the ADR.
<!-- /scratch -->

DECISION: `basename` becomes `Option<String>` on the wire; React normalises
(`basename ?? path.split('/').pop() ?? path`) before rendering — existing TS
`FileRefComponent` type changes from required to optional. Documented in §a.

DECISION: hover popover uses a raw Tailwind `<div>`, not `Component::KeyValue`.
`KeyValue` is a wire-level primitive emitted by plugins, not a host UI
primitive; using it inside a host renderer conflates two layers. Documented
in §b.2.

DECISION: Syntax highlighter is headless CodeMirror 6 via `highlightTree`,
**reusing** the `@codemirror/lang-*` packages already bundled for
QuickEditor. Extract `resolveLanguage` into `src/languagePacks.ts` first.
Budget: 0 KB incremental. Documented in §f.2.

DECISION: Editor resolution order is `$EDITOR` → `code` → `vi`; unknown
editors get the bare path (no cursor flag). Defence-in-depth rejects
`;|&$\`\n` in `$EDITOR` even though we never shell out. Documented in §e.

DECISION: Ratatui does **not** syntax-highlight the expanded slice.
`syntect`/`ratatui-syntect` cost (~3 MB binary) is rejected for a ±10-line
preview. Documented in §c.2.

LEARNING: The existing `types.ts:FileRefComponent` is ahead of the Rust
`Component` enum (TS has the type; Rust has no variant). Plugins cannot
emit FileRef today — this design closes the gap by adding the Rust variant.

PATTERN: Three new Tauri commands all reuse the existing `validate_path`
HOME gate (`src-tauri/src/lib.rs:304`). Do NOT fork the sandbox policy; this
is the established pattern shared with `read_file`/`write_file`.

GOTCHA: The current `RenderFileRef` at `src/ComponentRenderer.tsx:337` sets
a native `title=` attribute that will race the custom popover on macOS
(both trigger on hover). The new implementation MUST drop the `title`
attribute — replaced by the popover header + `aria-describedby`.
