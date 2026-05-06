import type { ResultSection, RunLogRow, RailItem } from '../components/PaletteShell'

export const MOCK_SECTIONS: ResultSection[] = [
  {
    label: 'Actions',
    items: [
      { id: 'run-mission',   name: 'Run mission',         meta: '⌘↵', selected: true },
      { id: 'tracker-ready', name: 'Tracker ready items' },
      { id: 'new-thread',    name: 'New chat thread',     meta: '⌘N' },
    ],
  },
  {
    label: 'Recent Threads',
    items: [
      { id: 'thread-whim', name: 'Whim Desktop · phase-3' },
      { id: 'thread-trk',  name: 'TRK-558 code parity' },
    ],
  },
  {
    label: 'Projects',
    items: [
      { id: 'proj-nanika', name: 'nanika', meta: '3 active' },
    ],
  },
]

export const MOCK_RUN_LOG: RunLogRow[] = [
  { id: 'r1', done: true,  persona: 'architect',          phase: 'read-ux-decisions',       duration: '12s' },
  { id: 'r2', done: true,  persona: 'architect',          phase: 'scaffold',                duration: '8s' },
  { id: 'r3', done: false, live: true, persona: 'senior-frontend', phase: 'implement-palette-shell', duration: '2m 14s' },
  { id: 'r4', done: false, persona: 'staff-code-reviewer', phase: 'review' },
]

export const MOCK_RAIL: RailItem[] = [
  { id: 'whim-palette', label: 'whim-palette', active: true, meta: '2m', working: true },
  { id: 'trk-558',      label: 'TRK-558 parity' },
  { id: 'linkedin',     label: 'linkedin-post' },
]

export const MOCK_TRANSCRIPT_LINES: string[] = [
  'run the whim palette mission',
  'check tracker for ready items',
  'what is the status of TRK 558',
  'open a new thread about voice overlay',
]

export const MOCK_TRANSCRIPT_COMMITTED = 'run the whim palette mission'
