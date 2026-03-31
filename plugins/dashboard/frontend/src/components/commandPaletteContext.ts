import { createContext, useContext } from 'react'
import type { Message } from '../types'

export type PaletteMode = 'command' | 'conversation'

export interface ConversationBridge {
  submit: (text: string) => void
  messages: Message[]
}

export interface CommandPaletteContextValue {
  isOpen: boolean
  initialQuery: string
  initialMode: PaletteMode
  open: () => void
  openConversation: (query?: string) => void
  close: (skipHide?: boolean) => void
  toggle: () => void
  openWithQuery: (query: string) => void
}

export const CommandPaletteContext = createContext<CommandPaletteContextValue>({
  isOpen: false,
  initialQuery: '',
  initialMode: 'command',
  open: () => {},
  openConversation: () => {},
  close: () => {},
  toggle: () => {},
  openWithQuery: () => {},
})

export function useCommandPalette(): CommandPaletteContextValue {
  return useContext(CommandPaletteContext)
}
