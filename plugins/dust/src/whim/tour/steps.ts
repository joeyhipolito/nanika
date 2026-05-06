import type { TourStep } from './types'

// ─── Owned vs demonstrated key helpers ───────────────────────────────────────

const own = (key: string, label: string) => ({ key, label, owned: true  })
const demo = (key: string, label: string) => ({ key, label, owned: false })

// ─── 26-entry catalog ─────────────────────────────────────────────────────────

const TOUR_STEPS: TourStep[] = [
  // ── Chapter 1 — At rest (4 steps) ──────────────────────────────────────────

  {
    id:              'c1-opener',
    chapter:         'at-rest',
    title:           'Whim is a HUD over real work',
    body:            'Whim lives at the edge of your screen as an 820-px capsule. It never owns the foreground; it ambient-docks until summoned. The tour walks the same substrate at four scales — pill → palette → palette-detail → canvas — so what you see in 30 seconds is the same component morphing, not four separate windows.',
    keys:            [own('→', 'next')],
    scene:           { kind: 'pill', state: { dock: 'left', mode: 'idle' } },
    spotlightTarget: "[data-tour-anchor='shell-frame']",
    durationMs:      5500,
  },
  {
    id:              'c1-idle-pill',
    chapter:         'at-rest',
    title:           'Resting pill, edge-docked',
    body:            'The pill at L0 is a frameless 28-px-tall capsule on #0C0D10, no labels, no chrome beyond a 0.5-px rim. It does not occlude active windows. Look at how little it asks of you — Whim\'s discoverability comes from a single global hotkey, not from chrome.',
    keys:            [own('→', 'next')],
    scene:           { kind: 'pill', state: { mode: 'idle', dock: 'left' } },
    spotlightTarget: "[data-tour-anchor='pill-capsule']",
    durationMs:      5000,
  },
  {
    id:              'c1-drag-snap',
    chapter:         'at-rest',
    title:           'Drag-snap to 40 % / 60 % anchor lines',
    body:            'Grab the pill and drag horizontally — past 5 px the drag latches, two faint anchor lines appear at 40 % and 60 % of viewport width, and the pill snaps to whichever line is closest at release. Esc mid-drag snaps the pill back to where it started without firing onDock.',
    keys:            [own('→', 'next')],
    scene:           { kind: 'pill', state: { mode: 'idle', dock: 'right', dragOverlay: 'demo' } },
    spotlightTarget: "[data-tour-anchor='pill-drag-overlay']",
    durationMs:      6000,
  },
  {
    id:              'c1-hover-preview',
    chapter:         'at-rest',
    title:           'Hover reveals input, mic, conv badge',
    body:            'Hovering the pill reveals a read-only compact input row, a mic glyph, and a small conversation-count badge — none of them committed yet to type or voice. The badge fans up to 7 recent conversations on a 60° arc when clicked. Hover is a low-commitment preview; the substrate hasn\'t switched modes.',
    keys:            [own('→', 'next'), own('←', 'back')],
    scene:           { kind: 'pill', state: { mode: 'hover', dock: 'left' } },
    spotlightTarget: "[data-tour-anchor='pill-hover-row']",
    durationMs:      5500,
  },

  // ── Chapter 2 — Summon (3 steps) ───────────────────────────────────────────

  {
    id:              'c2-summon',
    chapter:         'summon',
    title:           '⌥Space summons the palette in place',
    body:            'The pill expands to a 820 × 520 palette in place — the same component, scale prop flipped from pill to palette. There is no pop-in window; the substrate\'s own slot mounts run the transition. Width is preserved across the morph so the eye lands on the same focus column.',
    keys:            [own('→', 'next')],
    scene:           { kind: 'palette', state: { query: '', cursor: 0 } },
    spotlightTarget: "[data-tour-anchor='palette-input']",
    durationMs:      5000,
  },
  {
    id:              'c2-type-results',
    chapter:         'summon',
    title:           'Grouped results: Actions · Recent · Projects · Tracker · Plugins',
    body:            'Typing surfaces grouped results — Actions first, then Recent threads, then Projects, then Tracker issues, then Plugin verbs. The leading icon and placeholder communicate mode (search / slash / @ / chat) without coloured pills. ↑↓ move; ↵ selects.',
    keys:            [demo('↑', 'up'), demo('↓', 'down'), demo('↵', 'select')],
    scene:           { kind: 'palette', state: { query: 'open', cursor: 1 } },
    spotlightTarget: "[data-tour-anchor='palette-results']",
    durationMs:      5500,
  },
  {
    id:              'c2-detail-pane',
    chapter:         'summon',
    title:           'Detail pane (palette-detail, L2)',
    body:            '↵ on a result expands the palette into a results-list + detail-pane split. The substrate is still one component — the right column is a Component[] slice rendered by the plugin. Esc steps back to results, never two rungs at once.',
    keys:            [demo('↵', 'expand'), own('Esc', 'back')],
    scene:           { kind: 'palette-detail', state: { query: 'open', cursor: 2 } },
    spotlightTarget: "[data-tour-anchor='palette-detail-pane']",
    durationMs:      5500,
  },

  // ── Chapter 3 — Compose & voice (5 steps) ──────────────────────────────────

  {
    id:              'c3-voice-listening',
    chapter:         'compose-voice',
    title:           'Pill morphs into a 5-bar waveform',
    body:            'Holding V outside an input — or ⌥Space globally — flips the pill\'s body into a 5-bar live waveform with the rolling transcript below. Same width, same dock, same substrate; only the body slot changes. Release commits; Esc discards.',
    keys:            [demo('V', 'hold to record'), own('Esc', 'discard')],
    scene:           { kind: 'pill', state: { mode: 'voice', dock: 'left' } },
    spotlightTarget: "[data-tour-anchor='pill-voice-waveform']",
    durationMs:      5500,
  },
  {
    id:              'c3-voice-overlay',
    chapter:         'compose-voice',
    title:           '9-bin VoiceOverlay inside the type-mode composer',
    body:            'Inside the palette\'s composer, holding V for ≥ 200 ms enters recording as a sub-state of type. A denser 9-bin overlay paints over the input row; STT partial transcript renders below. Any mode change cancels the recording, the same way Esc unwinds an L4 popover.',
    keys:            [demo('V', 'hold to record')],
    scene:           { kind: 'palette', state: { query: '', composerState: 'recording-vhold' } },
    spotlightTarget: "[data-tour-anchor='composer-voice-overlay']",
    durationMs:      5500,
  },
  {
    id:              'c3-transcript-preview',
    chapter:         'compose-voice',
    title:           'Transcript pasted into composer, never auto-submitted',
    body:            'On V-key-up, the transcript is pasted into the composer with caret at the end and the mode flips back to type. The user always edits before sending — Whim refuses to run an agent against a one-shot dictation.',
    keys:            [demo('↵', 'submit'), own('Esc', 'cancel')],
    scene:           { kind: 'palette', state: { query: 'open the diff viewer for the last commit', composerState: 'drafting' } },
    spotlightTarget: "[data-tour-anchor='composer-input']",
    durationMs:      5000,
  },
  {
    id:              'c3-composer-drafting',
    chapter:         'compose-voice',
    title:           'Composer chips: model, reasoning, mode, permissions, tokens',
    body:            'The composer carries five chips inline below the input — model (Opus 4.7 1M), reasoning (Extra high), mode (code), permissions (bypass), token estimate (12.4k). The chips are status, not nav; clicking opens a popover, never a new window.',
    keys:            [demo('Tab', 'cycle chips')],
    scene:           { kind: 'canvas', state: { composerState: 'drafting', conversationFixture: 'long-scroll' } },
    spotlightTarget: "[data-tour-anchor='composer-chips']",
    durationMs:      5500,
  },
  {
    id:              'c3-keymap-recap-1',
    chapter:         'compose-voice',
    title:           'Recognition over recall — keycaps live beside actions',
    body:            'Every on-screen action ties to an inline keycap with a tonal variant — green for accept, red for reject, amber accent for primary, neutral for navigation. There is no abstract shortcut palette; every shortcut is shown next to the action that consumes it.',
    keys:            [demo('?', 'toggle keymap')],
    scene:           { kind: 'canvas', state: { composerState: 'drafting' } },
    spotlightTarget: "[data-tour-anchor='composer-keycaps']",
    durationMs:      5000,
  },

  // ── Chapter 4 — Mission canvas (6 steps) ───────────────────────────────────

  {
    id:              'c4-mission-spawn',
    chapter:         'canvas',
    title:           'Optimistic submit, no creating-workspace dialog',
    body:            'On ↵ the user\'s turn echoes optimistically into a ChatRail, the workspace path is allocated, and the substrate flips to canvas (L3). A red stop button appears next to a Working for 10s dashed rule — perceived latency is owned, not delegated to a spinner.',
    keys:            [demo('↵', 'submit')],
    scene:           { kind: 'canvas', state: { conversationFixture: 'short', missionRun: null } },
    spotlightTarget: "[data-tour-anchor='canvas-chat-rail']",
    durationMs:      5500,
  },
  {
    id:              'c4-run-log',
    chapter:         'canvas',
    title:           'Stacked phase log, one row per phase',
    body:            'The mission canvas\'s run log shows one line per phase — not per tool call — transitioning ○ → ✓ with a live duration counter. T3\'s per-turn duration banner and Raycast\'s stacked tool log compress into a single grammar; raw bash chatter stays hidden.',
    keys:            [own('j', 'next phase'), own('k', 'prev phase')],
    scene:           { kind: 'canvas', state: { conversationFixture: 'short' } },
    spotlightTarget: "[data-tour-anchor='runlog-phase-active']",
    durationMs:      6000,
  },
  {
    id:              'c4-review-gate',
    chapter:         'canvas',
    title:           'Changed-files card + per-file plain-English summary',
    body:            'When a mission completes with pending diffs, the canvas surfaces a Changed-Files card (CHANGED FILES (N) • +X / -Y) followed by a per-file one-sentence summary table. The card is a first-class block, not a footer banner. Pressing ⌘D (or clicking View diff) enters the diff viewer.',
    keys:            [demo('⌘D', 'view diff')],
    scene:           { kind: 'canvas', state: { rightRailOpen: true, rightRailMode: 'turn-diff' } },
    spotlightTarget: "[data-tour-anchor='changed-files-card']",
    durationMs:      6000,
  },
  {
    id:              'c4-diff-viewer',
    chapter:         'canvas',
    title:           'Hero — 11-key modal hunk grammar',
    body:            'The diff viewer is Whim\'s hero surface. y accepts a hunk, n rejects, j/k move between hunks, a accepts all in the file, Y accepts the file, ⇧A accepts everything, ⌘[ / ⌘] move between files, c commits, ⌘Z undoes, r re-reads on a failed apply. Modal — these keys are inert outside the viewer.',
    keys:            [demo('y', 'accept'), demo('n', 'reject'), own('j', 'next hunk'), own('k', 'prev hunk'), demo('a', 'accept file')],
    scene:           { kind: 'canvas-diff', state: { hunkCursor: 3 } },
    spotlightTarget: "[data-tour-anchor='diff-hunk-active']",
    durationMs:      7000,
  },
  {
    id:              'c4-terminal-drawer',
    chapter:         'canvas',
    title:           '⌘J opens a 240-px terminal drawer (dock-push)',
    body:            'Docks push, overlays float. ⌘J opens a terminal drawer at the bottom; the canvas reflows up by 240 px without losing transcript context. The terminal is real — blinking-cursor prompt, right-aligned timestamps, ≥ 8 lines of mock history.',
    keys:            [demo('⌘J', 'toggle terminal')],
    scene:           { kind: 'canvas', state: { terminalOpen: true } },
    spotlightTarget: "[data-tour-anchor='terminal-drawer']",
    durationMs:      5500,
  },
  {
    id:              'c4-right-rail',
    chapter:         'canvas',
    title:           '⌘B opens the right rail (files / turn-diff)',
    body:            'The right rail at 300 px hosts the FilesPanel by default and TurnDiffInspector when toggled. The rail\'s mode survives ⌘B close + reopen — closing is hide, not reset. Behaviour pinned by the slice-4 GOTCHA, now a CompositeCanvasState invariant.',
    keys:            [demo('⌘B', 'toggle rail')],
    scene:           { kind: 'canvas', state: { rightRailOpen: true, rightRailMode: 'files' } },
    spotlightTarget: "[data-tour-anchor='right-rail-header']",
    durationMs:      5500,
  },

  // ── Chapter 5 — Plugins (2 steps) ──────────────────────────────────────────

  {
    id:              'c5-plugin-inline',
    chapter:         'plugins',
    title:           'Plugins embed inline mid-conversation',
    body:            'A plugin\'s chat-block can be injected mid-stream — between a tool-beat and an assistant turn. The host owns positioning and chrome stripping; the plugin emits Component[] (Markdown + List + Divider + CodeDiff + FileRef + ToolCallBeat + AgentTurn) and nothing else. Plugins never push toasts; the host decides notification surface.',
    keys:            [own('→', 'next')],
    scene:           { kind: 'canvas', state: { conversationFixture: 'with-plugin', pluginInline: { prefix: 'tracker', afterTurnIndex: 2 } } },
    spotlightTarget: "[data-tour-anchor='plugin-inline-block']",
    durationMs:      6000,
  },
  {
    id:              'c5-plugin-shell',
    chapter:         'plugins',
    title:           'Four embed points, no fifth',
    body:            'Dust plugins render in exactly four embed points: command-palette result row, palette detail pane, sidebar widget, inline chat block. CodeDiffs, FileRefs, ToolCallBeats are components inside those four — they are not new embed points. Expanding the surface area would require a protocol version bump.',
    keys:            [own('→', 'next')],
    scene:           { kind: 'overview', state: { route: '#/s/99-plugin-shell' } },
    spotlightTarget: "[data-tour-anchor='plugin-shell-table']",
    durationMs:      6500,
  },

  // ── Chapter 6 — Ambient & error (3 steps) ──────────────────────────────────

  {
    id:              'c6-ambient-hud',
    chapter:         'ambient-error',
    title:           'Ambient HUD + footer badges + palette banner',
    body:            'Three notification tiers: ambient HUD top-right (context-window usage, working-for counter), footer badges bottom (nen-staleness, scheduler-overdue, scout alerts), and a single terracotta palette banner under the input slot for explicit errors. None of them reflow content; HUDs float, docks push.',
    keys:            [own('→', 'next')],
    scene:           { kind: 'canvas', state: { ambientHud: 'context+working' } },
    spotlightTarget: "[data-tour-anchor='ambient-hud']",
    durationMs:      5500,
  },
  {
    id:              'c6-error-retry',
    chapter:         'ambient-error',
    title:           'In-context errors — never modal',
    body:            'Errors render at the surface that produced them. Composer red border for input validation; inline red bar on the offending tool/phase block for runtime failures; diff-failed artboard in the right rail with re-read & retry r. Retry is always a single-key action with a visible keycap.',
    keys:            [demo('r', 'retry')],
    scene:           { kind: 'canvas', state: { errorMessage: { kind: 'apply-failed', body: 'Hunk 3 failed' }, rightRailOpen: true, rightRailMode: 'turn-diff' } },
    spotlightTarget: "[data-tour-anchor='error-banner']",
    durationMs:      5500,
  },
  {
    id:              'c6-notifications',
    chapter:         'ambient-error',
    title:           'Toast stack — non-blocking, capped at 3 visible',
    body:            'Top-right toast stack docks under the TopActionBar; up to 3 visible, older entries collapse into a +N more chip. Each toast carries icon + body + optional action label + auto-dismiss timer (longer for error). Plugins do not push toasts; the host produces them from ActionResult returns.',
    keys:            [own('→', 'next')],
    scene:           { kind: 'canvas', state: { notifications: [{ id: 't1', kind: 'success', body: 'Mission complete' }, { id: 't2', kind: 'info', body: 'Scout found 3 new threads' }, { id: 't3', kind: 'warn', body: 'Context at 92 %' }] } },
    spotlightTarget: "[data-tour-anchor='toast-stack']",
    durationMs:      5000,
  },

  // ── Chapter 7 — Wrap (3 steps) ─────────────────────────────────────────────

  {
    id:              'c7-multi-mission',
    chapter:         'wrap',
    title:           'Multi-mission overview lives in the palette',
    body:            'There is no separate dashboard. The palette\'s grouped results — Active · Recent · Scheduled — are the multi-mission overview. Each row carries mission id, current phase, live duration, worker count. ⌘Space over an Active row enters that mission\'s L3 canvas — root-search is the navigation primitive.',
    keys:            [demo('↑', 'up'), demo('↓', 'down'), demo('↵', 'open mission')],
    scene:           { kind: 'palette', state: { query: '', group: 'missions' } },
    spotlightTarget: "[data-tour-anchor='palette-group-active']",
    durationMs:      6000,
  },
  {
    id:              'c7-esc-ladder',
    chapter:         'wrap',
    title:           'Esc never skips a rung',
    body:            'Five rungs (L4 popover → L3 detail → L2 canvas → L1 query → L0 pill). One press unwinds exactly one rung. The Tour itself owns its own L4-equivalent rung; pressing Esc exits the tour, it does not unwind the underlying scenario.',
    keys:            [own('Esc', 'exit tour')],
    scene:           { kind: 'overview', state: { route: '#/s/esc-ladder' } },
    spotlightTarget: "[data-tour-anchor='esc-ladder-diagram']",
    durationMs:      5500,
  },
  {
    id:              'c7-keymap-recap',
    chapter:         'wrap',
    title:           'Keymap recap — the host owns every key',
    body:            'Plugins never register keybindings; the host owns all keys. The complete v1 keymap fits in three groups — global (⌥Space, ⌘K, Esc), palette/canvas (⌘N ⌘T ⌘E ⌘⇧E ↑ ↓ ↵), diff-mode (y n j k a Y ⇧A ⌘[ ⌘] c ⌘Z r). Recognition over recall, every shortcut beside its action.',
    keys:            [demo('?', 'toggle keymap')],
    scene:           { kind: 'overview', state: { route: '#/s/99-plugin-shell' } },
    spotlightTarget: "[data-tour-anchor='keymap-card']",
    durationMs:      7000,
  },
]

export default TOUR_STEPS
