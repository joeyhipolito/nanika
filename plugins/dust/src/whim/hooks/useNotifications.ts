import { useCallback, useEffect, useMemo, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { CanvasNotification } from '../types'

const CLIENT_FIFO_CAP = 500

export interface UseNotificationsReturn {
  notifications: CanvasNotification[]
  unread:        number
  dismiss:       (id: string) => Promise<void>
  dismissAll:    () => Promise<void>
  loading:       boolean
  error:         string | null
}

export function useNotifications(): UseNotificationsReturn {
  const [notifications, setNotifications] = useState<CanvasNotification[]>([])
  const [loading, setLoading] = useState(true)
  const [error,   setError]   = useState<string | null>(null)

  // ── Initial fetch ────────────────────────────────────────────────────────

  useEffect(() => {
    let cancelled = false
    invoke<CanvasNotification[]>('list_notifications')
      .then(list => {
        if (cancelled) return
        setNotifications(list.slice(0, CLIENT_FIFO_CAP))
        setLoading(false)
      })
      .catch(err => {
        if (cancelled) return
        setError(String(err))
        setLoading(false)
      })
    return () => { cancelled = true }
  }, [])

  // ── Append listener (whim://notification) ────────────────────────────────

  useEffect(() => {
    let unlisten: (() => void) | null = null
    let cancelled = false

    listen<CanvasNotification>('whim://notification', ({ payload }) => {
      if (cancelled) return
      setNotifications(curr => {
        const next = [payload, ...curr.filter(n => n.id !== payload.id)]
        return next.length > CLIENT_FIFO_CAP ? next.slice(0, CLIENT_FIFO_CAP) : next
      })
    })
      .then(f => {
        if (cancelled) { f(); return }
        unlisten = f
      })

    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [])

  // ── Update listener (whim://notification-update) ─────────────────────────
  // Replaces the matching id in place (atomic — produces a new array).

  useEffect(() => {
    let unlisten: (() => void) | null = null
    let cancelled = false

    listen<CanvasNotification>('whim://notification-update', ({ payload }) => {
      if (cancelled) return
      setNotifications(curr =>
        curr.map(n => (n.id === payload.id ? payload : n)),
      )
    })
      .then(f => {
        if (cancelled) { f(); return }
        unlisten = f
      })

    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [])

  // ── Mutators ─────────────────────────────────────────────────────────────

  const dismiss = useCallback(async (id: string) => {
    await invoke('notification_dismiss', { id })
    // Optimistic local replace — backend persists the same change.
    setNotifications(curr =>
      curr.map(n => (n.id === id ? { ...n, read: true } : n)),
    )
  }, [])

  const dismissAll = useCallback(async () => {
    await invoke('notification_dismiss_all')
    setNotifications(curr => curr.map(n => ({ ...n, read: true })))
  }, [])

  const unread = useMemo(
    () => notifications.reduce((acc, n) => acc + (n.read ? 0 : 1), 0),
    [notifications],
  )

  return { notifications, unread, dismiss, dismissAll, loading, error }
}
