// Mock diff fixture — drives 3.9-review-gate and 3.9b-diff-viewer.
// ≥3 files, ≥11 hunks total; hunk-0-2 has forcedFailOnApply for `r` retry demo.

export type HunkLineType = 'add' | 'rem' | 'ctx'

export interface HunkLine {
  type:    HunkLineType
  content: string
}

export interface Hunk {
  id:                 string
  header:             string
  lines:              HunkLine[]
  forcedFailOnApply?: boolean
}

export interface ChangedFile {
  path:      string
  additions: number
  deletions: number
  why:       string
  hunks:     Hunk[]
}

export const CHANGED_FILES: ChangedFile[] = [
  {
    path:      'src/components/PaletteShell.tsx',
    additions: 38,
    deletions: 12,
    why:       'Added warmBleed prop and WarmBleedLine decorator for mission-state visual alerts',
    hunks: [
      {
        id:     'hunk-0-0',
        header: '@@ -82,8 +82,12 @@ interface PaletteShellProps {',
        lines: [
          { type: 'ctx', content: '  railItems?:    RailItem[]' },
          { type: 'ctx', content: '  mainContent?:  ReactNode' },
          { type: 'rem', content: '  hints?:        HintEntry[]' },
          { type: 'add', content: '  runLog?:       RunLogRow[]' },
          { type: 'add', content: '  warmBleed?:    boolean' },
          { type: 'add', content: '  hints?:        HintEntry[]' },
          { type: 'ctx', content: '  onQueryChange?:  (v: string) => void' },
          { type: 'ctx', content: '  onResultSelect?: (id: string) => void' },
        ],
      },
      {
        id:     'hunk-0-1',
        header: '@@ -198,6 +202,14 @@ function WarmBleedLine() {',
        lines: [
          { type: 'ctx', content: 'function WarmBleedLine() {' },
          { type: 'rem', content: '  return null' },
          { type: 'add', content: '  return (' },
          { type: 'add', content: '    <div style={{' },
          { type: 'add', content: "      position: 'absolute', top: 0, left: 0, right: 0," },
          { type: 'add', content: '      height: 2,' },
          { type: 'add', content: "      background: 'linear-gradient(90deg,var(--accent),var(--accent-2))'," },
          { type: 'add', content: '    }} />' },
          { type: 'add', content: '  )' },
          { type: 'ctx', content: '}' },
        ],
      },
      {
        id:                'hunk-0-2',
        header:            '@@ -214,7 +226,9 @@ export function PaletteShell({',
        forcedFailOnApply: true,
        lines: [
          { type: 'ctx', content: 'export function PaletteShell({' },
          { type: 'ctx', content: '  scale, pillMode, recording, convCount,' },
          { type: 'ctx', content: '  sections, detailContent, breadcrumb, railItems, mainContent,' },
          { type: 'rem', content: '  hints, onQueryChange, onResultSelect, onSubmit, onEsc,' },
          { type: 'add', content: '  runLog, hints, warmBleed,' },
          { type: 'add', content: '  onQueryChange, onResultSelect, onSubmit, onEsc,' },
          { type: 'ctx', content: '}: PaletteShellProps) {' },
        ],
      },
    ],
  },
  {
    path:      'src/mocks/events.ts',
    additions: 22,
    deletions: 6,
    why:       'Exported MissionSummary interface and added diffs.ts re-export helpers',
    hunks: [
      {
        id:     'hunk-1-0',
        header: '@@ -1,5 +1,8 @@ // Mock mission event stream',
        lines: [
          { type: 'ctx', content: '// Mock mission event stream' },
          { type: 'ctx', content: '' },
          { type: 'add', content: "export { CHANGED_FILES } from './diffs'" },
          { type: 'add', content: "export type { ChangedFile, Hunk, HunkLine } from './diffs'" },
          { type: 'ctx', content: 'export interface MissionPhase {' },
          { type: 'ctx', content: '  persona:    string' },
          { type: 'ctx', content: '  phase:      string' },
        ],
      },
      {
        id:     'hunk-1-1',
        header: '@@ -64,6 +67,18 @@ // ─── Multi-mission directory data',
        lines: [
          { type: 'ctx', content: '// ─── Multi-mission directory data (3.13) ─' },
          { type: 'ctx', content: '' },
          { type: 'add', content: 'export interface MissionSummary {' },
          { type: 'add', content: '  id:           string' },
          { type: 'add', content: '  phase:        string' },
          { type: 'add', content: '  changedFiles: number' },
          { type: 'add', content: '  additions:    number' },
          { type: 'add', content: '  deletions:    number' },
          { type: 'add', content: '}' },
          { type: 'ctx', content: '' },
          { type: 'ctx', content: 'export interface ActiveMission {' },
          { type: 'ctx', content: '  id: string' },
        ],
      },
      {
        id:     'hunk-1-2',
        header: '@@ -88,6 +103,9 @@ export const ACTIVE_MISSIONS',
        lines: [
          { type: 'ctx', content: 'export const ACTIVE_MISSIONS: ActiveMission[] = [' },
          { type: 'rem', content: "  { id: 'whim-palette',   phase: 'implement-canvas', startedMsAgo: 134_000, workers: 3 }," },
          { type: 'add', content: "  { id: 'whim-palette',   phase: 'review',           startedMsAgo: 134_000, workers: 3 }," },
          { type: 'rem', content: "  { id: 'tracker-parity', phase: 'staff-review',     startedMsAgo:  47_000, workers: 2 }," },
          { type: 'add', content: "  { id: 'tracker-parity', phase: 'staff-review',     startedMsAgo:  47_000, workers: 2 }," },
          { type: 'add', content: "  { id: 'nen-scheduler',  phase: 'plan',             startedMsAgo:   8_000, workers: 1 }," },
          { type: 'ctx', content: ']' },
        ],
      },
    ],
  },
  {
    path:      'src/state/DiffStore.ts',
    additions: 74,
    deletions: 0,
    why:       'New file: hunk-FSM (pending→accepted/rejected→applied/failed) and cursor state for diff-viewer',
    hunks: [
      {
        id:     'hunk-2-0',
        header: '@@ -0,0 +1,24 @@',
        lines: [
          { type: 'add', content: "export type HunkStatus =" },
          { type: 'add', content: "  | 'pending'" },
          { type: 'add', content: "  | 'accepted'" },
          { type: 'add', content: "  | 'rejected'" },
          { type: 'add', content: "  | 'applied'" },
          { type: 'add', content: "  | 'failed'" },
          { type: 'add', content: '' },
          { type: 'add', content: 'export interface Cursor { fileIndex: number; hunkIndex: number }' },
          { type: 'add', content: '' },
          { type: 'add', content: 'export interface DiffStore {' },
          { type: 'add', content: '  statuses:    Record<string, HunkStatus>' },
          { type: 'add', content: '  cursor:      Cursor' },
          { type: 'add', content: '  undoHunkId:  string | null' },
          { type: 'add', content: '  commitToast: { applied: number; failed: number } | null' },
          { type: 'add', content: '}' },
        ],
      },
      {
        id:     'hunk-2-1',
        header: '@@ -0,0 +26,28 @@',
        lines: [
          { type: 'add', content: 'export function accept(store: DiffStore, id: string): DiffStore {' },
          { type: 'add', content: "  if (store.statuses[id] !== 'pending') return store" },
          { type: 'add', content: "  return { ...store, statuses: { ...store.statuses, [id]: 'accepted' } }" },
          { type: 'add', content: '}' },
          { type: 'add', content: '' },
          { type: 'add', content: 'export function reject(store: DiffStore, id: string): DiffStore {' },
          { type: 'add', content: "  if (store.statuses[id] !== 'pending') return store" },
          { type: 'add', content: "  return { ...store, statuses: { ...store.statuses, [id]: 'rejected' } }" },
          { type: 'add', content: '}' },
          { type: 'add', content: '' },
          { type: 'add', content: 'export function retry(store: DiffStore, id: string): DiffStore {' },
          { type: 'add', content: "  if (store.statuses[id] !== 'failed') return store" },
          { type: 'add', content: "  return { ...store, statuses: { ...store.statuses, [id]: 'pending' } }" },
          { type: 'add', content: '}' },
        ],
      },
      {
        id:     'hunk-2-2',
        header: '@@ -0,0 +56,20 @@',
        lines: [
          { type: 'add', content: 'export function commit(' },
          { type: 'add', content: '  store:       DiffStore,' },
          { type: 'add', content: '  forcedFails: Set<string>,' },
          { type: 'add', content: '): DiffStore {' },
          { type: 'add', content: '  let applied = 0, failed = 0' },
          { type: 'add', content: '  const next = { ...store.statuses }' },
          { type: 'add', content: "  for (const [id, st] of Object.entries(next)) {" },
          { type: 'add', content: "    if (st !== 'accepted') continue" },
          { type: 'add', content: "    if (forcedFails.has(id)) { next[id] = 'failed'; failed++ }" },
          { type: 'add', content: "    else { next[id] = 'applied'; applied++ }" },
          { type: 'add', content: '  }' },
          { type: 'add', content: '  return { ...store, statuses: next,' },
          { type: 'add', content: '    commitToast: { applied, failed },' },
          { type: 'add', content: '  }' },
          { type: 'add', content: '}' },
        ],
      },
    ],
  },
  {
    path:      'src/scenarios/Scenes.tsx',
    additions: 198,
    deletions: 4,
    why:       'Implemented 3.9-review-gate (Changed-Files card + summary table) and 3.9b-diff-viewer (HERO surface)',
    hunks: [
      {
        id:     'hunk-3-0',
        header: '@@ -1,6 +1,9 @@ import { useState',
        lines: [
          { type: 'ctx', content: "import { useState, useEffect, useRef } from 'react'" },
          { type: 'rem', content: "import { MOCK_SECTIONS, MOCK_TRANSCRIPT_LINES, MOCK_TRANSCRIPT_COMMITTED } from '../mocks/index'" },
          { type: 'add', content: 'import {' },
          { type: 'add', content: '  MOCK_SECTIONS, MOCK_TRANSCRIPT_LINES, MOCK_TRANSCRIPT_COMMITTED,' },
          { type: 'add', content: "  CHANGED_FILES," },
          { type: 'add', content: "} from '../mocks/index'" },
          { type: 'ctx', content: 'import {' },
          { type: 'ctx', content: "  MISSION_PHASES, MISSION_TOTAL_MS, buildRunLog, formatElapsed," },
        ],
      },
      {
        id:     'hunk-3-1',
        header: '@@ -1032,4 +1036,202 @@ export function MultiMissionScene()',
        lines: [
          { type: 'ctx', content: '  )' },
          { type: 'ctx', content: '}' },
          { type: 'ctx', content: '' },
          { type: 'add', content: '// ─── 3.9 · Review gate ─────────────────────────' },
          { type: 'add', content: "export function ReviewGateScene() { /* ... */ }" },
          { type: 'add', content: '' },
          { type: 'add', content: '// ─── 3.9b · Diff viewer (HERO) ─────────────────' },
          { type: 'add', content: "export function DiffViewerScene() { /* ... */ }" },
        ],
      },
    ],
  },
]

export const DIFF_TOTAL_ADDITIONS = CHANGED_FILES.reduce((s, f) => s + f.additions, 0)
export const DIFF_TOTAL_DELETIONS = CHANGED_FILES.reduce((s, f) => s + f.deletions, 0)
