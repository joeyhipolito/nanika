export type PillMode = 'idle' | 'hover' | 'type' | 'voice'

export interface PillState {
  mode: PillMode
  recording: boolean
  fanOpen: boolean
}

export type PillEvent =
  | { type: 'HOVER' }
  | { type: 'UNHOVER' }
  | { type: 'FOCUS' }
  | { type: 'BLUR' }
  | { type: 'VOICE_START' }
  | { type: 'VOICE_COMMIT' }
  | { type: 'VOICE_DISCARD' }
  | { type: 'FAN_TOGGLE' }
  | { type: 'RESET' }

export function pillTransition(state: PillState, event: PillEvent): PillState {
  switch (event.type) {
    case 'HOVER':
      return state.mode === 'idle' ? { ...state, mode: 'hover' } : state
    case 'UNHOVER':
      return state.mode === 'hover' ? { ...state, mode: 'idle' } : state
    case 'FOCUS':
      return { ...state, mode: 'type', fanOpen: false }
    case 'BLUR':
      return { mode: 'idle', recording: false, fanOpen: false }
    case 'VOICE_START':
      return state.mode === 'type'
        ? { ...state, recording: true }
        : { ...state, mode: 'voice', recording: true }
    case 'VOICE_COMMIT':
    case 'VOICE_DISCARD':
      return state.mode === 'voice'
        ? { ...state, mode: 'idle', recording: false }
        : { ...state, recording: false }
    case 'FAN_TOGGLE':
      return { ...state, fanOpen: !state.fanOpen }
    case 'RESET':
      return { mode: 'idle', recording: false, fanOpen: false }
    default:
      return state
  }
}

export const initialPillState: PillState = { mode: 'idle', recording: false, fanOpen: false }
