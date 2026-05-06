// Mock per-turn diff data — drives TurnDiffInspector

export type DiffLineType = 'add' | 'rem' | 'ctx'

export interface DiffLine {
  type:    DiffLineType
  content: string
}

export interface Turn {
  id:    number
  label: string   // e.g. "Turn 1"
  time:  string   // e.g. "09:12"
}

export const MOCK_TURNS: Turn[] = [
  { id: 1, label: 'Turn 1', time: '09:12' },
  { id: 2, label: 'Turn 2', time: '09:15' },
  { id: 3, label: 'Turn 3', time: '09:20' },
  { id: 4, label: 'Turn 4', time: '09:28' },
]

export const MOCK_HUNK_LINES: DiffLine[] = [
  { type: 'ctx', content: '## journaler' },
  { type: 'ctx', content: '' },
  { type: 'rem', content: 'Route all daily notes to /daily.' },
  { type: 'add', content: 'Route all daily notes to /daily/YYYY-MM-DD.' },
  { type: 'ctx', content: '' },
  { type: 'rem', content: '### On ambiguous routing' },
  { type: 'rem', content: 'Ask the user for clarification.' },
  { type: 'add', content: '### On ambiguous routing' },
  { type: 'add', content: 'Prefer the most specific match; ask only when' },
  { type: 'add', content: 'two candidates are equally specific.' },
  { type: 'ctx', content: '' },
]

export const MOCK_COLLAPSED_CONTEXT: string[] = [
  '## Configuration',
  '',
  'vault: ~/.alluka/vault',
  'index: notes/index.md',
  'dailies: daily/',
  'templates: templates/',
  'format: obsidian',
  'timezone: America/Los_Angeles',
]
