// ─── Terminal mock data ───────────────────────────────────────────────────────

export interface TerminalLine {
  kind: 'cmd' | 'stdout' | 'stderr' | 'info'
  text: string
}

export interface TerminalPrompt {
  user: string
  host: string
  path: string
  branch: string
  gitStatus: string
  timestamp: string
}

export const MOCK_TERMINAL_LINES: TerminalLine[] = [
  { kind: 'cmd',    text: 'orchestrator run ~/.alluka/missions/nanika-t3-parity.md --dry-run' },
  { kind: 'info',   text: '▸ Dry-run mode — no changes will be committed' },
  { kind: 'stdout', text: '✓ phase architect        DEPENDS []' },
  { kind: 'stdout', text: '✓ phase scenario-tour    DEPENDS [architect]' },
  { kind: 'stdout', text: '✓ phase left-rail        DEPENDS [scenario-tour]' },
  { kind: 'stdout', text: '✓ phase right-rail       DEPENDS [left-rail]' },
  { kind: 'stdout', text: '✓ phase terminal-drawer  DEPENDS [right-rail]' },
  { kind: 'info',   text: '5 phases · 0 review phases · estimated $4.20' },
  { kind: 'cmd',    text: 'npm run build' },
  { kind: 'stdout', text: '✓ built in 542ms · 0 errors · 0 warnings' },
]

export const MOCK_TERMINAL_PROMPT: TerminalPrompt = {
  user:      'joeyhipolito',
  host:      'joeyhipolitomac',
  path:      '~/nanika',
  branch:    'main',
  gitStatus: 'S * ?',
  timestamp: '[HH:MM:SS]',
}
