import { useMemo } from 'react'
import { useNotifications } from '../hooks/useNotifications'
import { CompositeCanvas, type CompositeCanvasState } from './CompositeCanvas'
import type { CanvasNotification } from '../types'

// LiveNotificationsCanvas — reads live notifications from the Tauri backend
// (`list_notifications` + the two whim:// channels) and feeds them into a
// CompositeCanvas as the notifications slot. Read notifications are filtered
// out so the canvas surface mirrors what's actually unread.

function toCanvasNotification(n: CanvasNotification) {
  return {
    id:      n.id,
    kind:    'info' as const,
    surface: 'hud' as const,
    body:    n.message || n.title,
  }
}

export function LiveNotificationsCanvas() {
  const { notifications } = useNotifications()

  const liveState = useMemo<CompositeCanvasState>(() => {
    const unread = notifications.filter(n => !n.read).slice(0, 5)
    return {
      railVariant:         'projects-tree',
      terminalOpen:        false,
      rightRailOpen:       false,
      rightRailMode:       'files',
      selectedFile:        null,
      conversationFixture: 'empty',
      composerState:       'idle',
      errorMessage:        null,
      notifications:       unread.map(toCanvasNotification),
      missionRun:          null,
      pluginInline:        null,
      streamingTurn:       null,
      workingForMs:        null,
      reviewGate:          false,
      voiceTranscript:     null,
    }
  }, [notifications])

  return <CompositeCanvas state={liveState} />
}
