import type { CompositeCanvasState } from '../components/CompositeCanvas'
import { ALL_NOTIFICATIONS } from './notifications'

export const stateDefault: CompositeCanvasState = {
  railVariant:         'projects-tree',
  terminalOpen:        false,
  rightRailOpen:       false,
  rightRailMode:       'files',
  selectedFile:        null,
  conversationFixture: 'baseline',
  composerState:       'idle',
  errorMessage:        null,
  notifications:       [],
  missionRun:          null,
  pluginInline:        null,
  streamingTurn:       null,
  workingForMs:        null,
  reviewGate:          false,
  voiceTranscript:     null,
}

// A — three columns + bottom dock
export const stateAllOpen: CompositeCanvasState = {
  ...stateDefault,
  terminalOpen:  true,
  rightRailOpen: true,
  rightRailMode: 'files',
}

// B — left-rail variant instead of projects-tree
export const stateLeftrail: CompositeCanvasState = {
  ...stateDefault,
  railVariant: 'left-rail',
}

// C — empty conversation, minimal state
export const stateEmpty: CompositeCanvasState = {
  ...stateDefault,
  conversationFixture: 'empty',
}

// D — base for streaming scene (scene layer manages streamingTurn/workingForMs)
export const stateStreaming: CompositeCanvasState = {
  ...stateDefault,
  conversationFixture: 'empty',
  streamingTurn:       null,
  workingForMs:        null,
}

// E — base for mission-running scene (scene layer updates elapsedMs)
export const stateMissionRunning: CompositeCanvasState = {
  ...stateDefault,
  conversationFixture: 'empty',
  missionRun: {
    missionId:     'trk-558',
    title:         'Nanika → T3 Code parity',
    activePhaseId: 'implement',
    elapsedMs:     0,
    phases: [
      { id: 'architect',  persona: 'staff-architect',           status: 'done',    expectedMs: 45_000 },
      { id: 'implement',  persona: 'senior-backend-engineer',   status: 'running', expectedMs: 120_000 },
      { id: 'review',     persona: 'staff-code-reviewer',       status: 'pending', expectedMs: 30_000 },
    ],
  },
}

// F — review gate: Changed Files card + View diff button
export const stateReviewGate: CompositeCanvasState = {
  ...stateDefault,
  conversationFixture: 'baseline',
  reviewGate:          true,
}

// G — error state: red border on composer, inline error label, red ToolBeat
export const stateError: CompositeCanvasState = {
  ...stateDefault,
  conversationFixture: 'baseline',
  composerState:       'error',
  errorMessage: {
    kind:       'phase-failed',
    body:       'Phase "implement" failed — permission denied writing to src/components/ToolBeat.tsx',
    retryLabel: 'Retry phase',
  },
}

// H — voice overlay: 9-bin waveform + transcript preview; Esc → idle (scene manages)
export const stateVoice: CompositeCanvasState = {
  ...stateDefault,
  conversationFixture: 'baseline',
  composerState:       'recording-voice',
  voiceTranscript:     'Run tracker list and show me the open P0 issues',
}

// I — files panel open + FileViewer above composer; scene manages selectedFile state
export const stateFilesAndViewer: CompositeCanvasState = {
  ...stateDefault,
  conversationFixture: 'baseline',
  rightRailOpen:       true,
  rightRailMode:       'files',
  selectedFile:        { path: 'src/components/PaletteShell.tsx' },
}

// J — plugin inline block (tracker issue card) injected after turn 2
export const statePluginInline: CompositeCanvasState = {
  ...stateDefault,
  conversationFixture: 'baseline',
  pluginInline: {
    prefix:         'tracker',
    afterTurnIndex: 2,
    body: [
      { kind: 'issue', id: 'TRK-573', title: 'CodeDiff: fix apply_hunk algorithm + persist applied state', status: 'in_progress', priority: 'P1' },
      { kind: 'issue', id: 'TRK-558', title: 'Nanika → T3 Code parity', status: 'in_progress', priority: 'P0' },
      { kind: 'issue', id: 'TRK-575', title: 'Tool-use: switch from --tools to --mcp-config', status: 'open', priority: 'P1' },
      { kind: 'text',  content: '3 open issues · 2 blocked · last synced 12s ago' },
    ],
  },
}

// K — all three notification surfaces: ambient HUD, banner, footer badges
export const stateNotifications: CompositeCanvasState = {
  ...stateDefault,
  conversationFixture: 'baseline',
  notifications:       ALL_NOTIFICATIONS,
}
