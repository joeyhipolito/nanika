export interface ActionPaletteItem {
  id: string
  title: string
  subtitle?: string
}

export interface ActionPaletteAction {
  id: string
  verb: string
  key: string
}

export const MOCK_ACTION_ITEM: ActionPaletteItem = {
  id: 'trk-573',
  title: 'TRK-573 · CodeDiff apply_hunk algorithm',
  subtitle: 'P0 · in_progress',
}

export const MOCK_ACTION_PALETTE_ACTIONS: ActionPaletteAction[] = [
  { id: 'open',    verb: 'Open',         key: 'o' },
  { id: 'assign',  verb: 'Assign to me', key: 'a' },
  { id: 'close',   verb: 'Close issue',  key: 'x' },
  { id: 'copy',    verb: 'Copy link',    key: 'y' },
  { id: 'archive', verb: 'Archive',      key: 'e' },
]
