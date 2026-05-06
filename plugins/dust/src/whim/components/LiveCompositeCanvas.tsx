"use client"

import { useMemo } from 'react'
import { useChat } from '../hooks/useChat'
import { CompositeCanvas } from './CompositeCanvas'
import type { CompositeCanvasState } from './CompositeCanvas'
import { stateDefault } from '../mocks/canvasStates'
import type { ConvItem } from '../types'
import type { Component } from '../types'

// ─── Component → ConvItem mapper ─────────────────────────────────────────────
// Maps the dust Component[] (backend wire format) to the simpler ConvItem[]
// display union that CompositeCanvas renders. Unknown component types fall back
// to a doc-turn so nothing silently vanishes.

function componentToConvItem(c: Component): ConvItem | null {
  if (c.type === 'agent_turn') {
    if (c.role === 'user') return { kind: 'user', text: c.content }
    return { kind: 'doc-turn', content: c.content }
  }
  if (c.type === 'tool_call_beat') {
    const body = c.result != null ? JSON.stringify(c.result, null, 2) : undefined
    return { kind: 'tool-beat', summary: c.name, body }
  }
  if (c.type === 'markdown') return { kind: 'doc-turn', content: c.content }
  if (c.type === 'text') return { kind: 'doc-turn', content: c.content }
  return null
}

// ─── LiveCompositeCanvas ──────────────────────────────────────────────────────

export interface LiveCompositeCanvasProps {
  threadId?: string
}

export function LiveCompositeCanvas({ threadId }: LiveCompositeCanvasProps) {
  const chat = useChat(threadId)

  const liveConversation = useMemo<ConvItem[]>(
    () => chat.conversation.flatMap(c => {
      const item = componentToConvItem(c)
      return item ? [item] : []
    }),
    [chat.conversation],
  )

  const liveState = useMemo<CompositeCanvasState>(
    () => ({
      ...stateDefault,
      liveConversation,
      composerState: chat.loading ? 'drafting' : 'idle',
      errorMessage: chat.error
        ? { kind: 'connectivity', body: chat.error }
        : null,
    }),
    [liveConversation, chat.loading, chat.error],
  )

  return (
    <CompositeCanvas
      state={liveState}
      onComposerSubmit={chat.ask}
    />
  )
}
