import type { PillMode } from '../components/PaletteShell'
import type { CompositeCanvasState } from '../components/CompositeCanvas'

export type DemoSurface = 'pill' | 'palette' | 'detail' | 'canvas' | 'diff'
export type DemoChapter = 'rest' | 'summon' | 'compose' | 'voice' | 'submit' | 'canvas' | 'diff' | 'wrap'

export interface DemoCallout {
  title:  string
  body?:  string
}

export interface DemoCursor {
  x:        number
  y:        number
  visible:  boolean
}

export interface DemoTick {
  atMs:   number
  patch:  Partial<DemoState>
}

export interface DemoStep {
  id:                          string
  chapter:                     DemoChapter
  chapterTitle:                string
  action:                      string
  narration:                   string
  explanation:                 string
  sceneMutation:               Partial<DemoState>
  callout?:                    DemoCallout
  durationMs?:                 number
  ticks?:                      DemoTick[]
  ['data-demo-coverage-tokens']: string[]
}

export interface DemoState {
  stepIndex:    number
  playing:      boolean
  surface:      DemoSurface
  pillMode:     PillMode
  query:        string
  caretIndex:   number
  cursor:       DemoCursor
  pressedKey:   string | null
  pressedAt:    number
  narration:    string
  explanation:  string
  chapter:      DemoChapter
  chapterTitle: string
  callout:      DemoCallout | null
  canvas:       CompositeCanvasState
  recording:    boolean
  reduceMotion: boolean
}

export const INITIAL_CANVAS_STATE: CompositeCanvasState = {
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

export const INITIAL_DEMO_STATE: DemoState = {
  stepIndex:    0,
  playing:      false,
  surface:      'pill',
  pillMode:     'idle',
  query:        '',
  caretIndex:   0,
  cursor:       { x: 88, y: 92, visible: false },
  pressedKey:   null,
  pressedAt:    0,
  narration:    'Whim sits at rest.',
  explanation:  '63×9 capsule on the bottom-left edge. No labels, no chrome. Discoverability comes from one global hotkey, not from chrome.',
  chapter:      'rest',
  chapterTitle: 'Rest',
  callout:      null,
  canvas:       INITIAL_CANVAS_STATE,
  recording:    false,
  reduceMotion: false,
}
