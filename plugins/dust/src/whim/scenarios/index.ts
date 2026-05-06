import type { ShellScale, PillMode } from '../components/PaletteShell'

export interface Scenario {
  id: string
  label: string
  description: string
  scale: ShellScale
  pillMode?: PillMode
}

export const SCENARIOS: Scenario[] = [
  { id: 'pill-idle',   label: 'Pill — Idle',     description: 'Edge-docked resting state',    scale: 'pill',    pillMode: 'idle' },
  { id: 'pill-hover',  label: 'Pill — Hover',    description: 'Input row revealed on hover',  scale: 'pill',    pillMode: 'hover' },
  { id: 'pill-type',   label: 'Pill — Type',     description: 'Search / compose active',      scale: 'pill',    pillMode: 'type' },
  { id: 'pill-voice',  label: 'Pill — Voice',    description: '5-bar waveform, listening',    scale: 'pill',    pillMode: 'voice' },
  { id: 'palette',     label: 'Palette',          description: 'L1 — centered results list',   scale: 'palette' },
  { id: 'detail',      label: 'Detail',           description: 'L2 — split with detail pane', scale: 'detail' },
  { id: 'canvas',      label: 'Mission Canvas',   description: 'L3 — full windowed surface',  scale: 'canvas' },

  // Slice 2 additions — append after slice-1, do not reorder above
  { id: 'projects-tree',       label: 'Projects Tree',         description: 'T3-style 280 px sidebar with ⌘K search and j/k/o/↵ keyboard nav', scale: 'canvas' },
  { id: 'left-rail',           label: 'Left Rail',             description: 'Claude-Code-style rail: 3 mode tabs + Pinned + Recents + Routines',  scale: 'canvas' },
  { id: 'files-panel',         label: 'Files Panel',           description: 'Right-rail files panel with filter and ?-prefix content-search mode', scale: 'canvas' },
  { id: 'turn-diff-inspector', label: 'Turn Diff Inspector',   description: 'Right-rail per-turn diff with turn chips and hunk lines',           scale: 'canvas' },
  { id: 'file-viewer',         label: 'File Viewer',           description: 'Read-only file viewer with `/` search and n/N match nav',           scale: 'canvas' },
  { id: 'terminal-drawer',     label: 'Terminal Drawer',       description: 'Bottom-dock terminal with live prompt, ⌘J toggle, timestamp',       scale: 'canvas' },
  { id: 'top-action-bar',      label: 'Top Action Bar',        description: 'Breadcrumb chips + Add action / Open ▾ / Commit & push ▾',         scale: 'canvas' },
  { id: 'composer-chips',      label: 'Composer Chips',        description: 'Five-chip strip: model · reasoning · mode · permissions · tokens', scale: 'canvas' },
  { id: 'composer-footer',     label: 'Composer Footer',       description: 'Bypass permissions · attach · mic · pinned right model chip',      scale: 'canvas' },
  { id: 'document-mode',       label: 'Document Mode',         description: 'Assistant document turn (transparent bg) + table + code block',    scale: 'canvas' },
  { id: 'tool-beats',          label: 'Tool Beats',            description: 'Single-line collapsibles: Ran (red) · Recalled (accent) · Read',   scale: 'canvas' },
  { id: 'commit-summary',      label: 'Commit Summary',        description: 'Branch swap chips + +N −N pill + Create PR ▾ button',              scale: 'canvas' },
  { id: 'composite-canvas',              label: 'Composite Canvas',          description: 'HERO — full 1440×900 canvas, ⌘J terminal + ⌘B right rail',        scale: 'canvas' },

  // Slice 3 additions — canvas-state variants
  { id: 'composite-canvas-all-open',    label: 'Canvas — All Open',         description: 'Three columns (ProjectsTree + main + RightRail) + TerminalDrawer open', scale: 'canvas' },
  { id: 'composite-canvas-leftrail',    label: 'Canvas — Left Rail',        description: 'Left-rail variant (240 px LeftRail) instead of ProjectsTree',           scale: 'canvas' },
  { id: 'composite-canvas-empty',       label: 'Canvas — Empty',            description: 'Empty conversation state — clean slate, no turns',                       scale: 'canvas' },
  { id: 'composite-canvas-streaming',   label: 'Canvas — Streaming',        description: 'Token-drip streaming turn, working-for counter, red stop button',        scale: 'canvas' },
  { id: 'composite-canvas-mission',          label: 'Canvas — Mission Running',    description: '3-phase mission run log with ○/✓ glyphs and live-counting active phase', scale: 'canvas' },

  // Slice 4 additions — canvas-state variants F–K
  { id: 'composite-canvas-review-gate',      label: 'Canvas — Review Gate',        description: 'Changed Files card + View diff button navigates to diff viewer', scale: 'canvas' },
  { id: 'composite-canvas-error',            label: 'Canvas — Error',              description: 'Red border composer, error label, Failed ToolBeat with Alert icon', scale: 'canvas' },
  { id: 'composite-canvas-voice',            label: 'Canvas — Voice',              description: '9-bin VoiceOverlay in composer + transcript preview; Esc → idle', scale: 'canvas' },
  { id: 'composite-canvas-files-and-viewer', label: 'Canvas — Files + Viewer',     description: 'FilesPanel open in right rail + FileViewer above composer; state lift on select', scale: 'canvas' },
  { id: 'composite-canvas-plugin-inline',    label: 'Canvas — Plugin Inline',      description: 'tracker issue card injected inline after turn 2 (§3.10 embed contract)', scale: 'canvas' },
  { id: 'composite-canvas-notifications',    label: 'Canvas — Notifications',      description: 'All three tiers simultaneously: ambient HUD + banner + footer badges', scale: 'canvas' },

  // Slice 4 additions — missing surfaces L–N + L0→L3 transition demo O
  { id: 'action-palette',      label: 'Action Palette',      description: 'Centered floating popover · item title + ≥3 verb actions with keycaps · j/k/Enter/Esc', scale: 'palette' },
  { id: 'fan-view',            label: 'Fan View',            description: 'Radial thread switcher · 60° arc · 80 px radius · 36 px chips · j/k scroll · Esc dismiss', scale: 'palette' },
  { id: 'pill-drag-snap',      label: 'Pill Drag Snap',      description: 'Horizontal pill drag · snap-line guides at 40% and 60% · Esc mid-drag cancels',         scale: 'pill' },
  { id: 'transition-l0-to-l3', label: 'Transition L0 → L3',  description: 'Single PaletteShell substrate morphing through L0 → L1 → L2 → L3 · ▶ Next / space steps · ≤ 220 ms · Esc returns to L0', scale: 'pill' },

  // M5 additions — live diff surfaces
  { id: 'live-turn-diff-inspector', label: 'Live Turn Diff Inspector', description: 'Live TurnDiffInspector wired to Tauri diff commands — file chips, hunk lines, accept/reject', scale: 'canvas' },
]
