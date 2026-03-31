import { useEffect, useRef, useState } from 'react'
import type { DashboardPage } from '../types'
import type { ToastType } from '../components/Toast'

export interface CommandHandlers {
  onNavigate: (page: DashboardPage) => void
  onFocus: (missionId: string) => void
  onDismiss: () => void
  onSetInput: (text: string) => void
  onNotify: (text: string, type?: ToastType) => void
  onConversationReply?: (text: string) => void
}

const SSE_URL = '/api/sse'

const TOAST_TYPES = new Set<string>(['info', 'success', 'warning', 'error'])

function toToastType(value: string | undefined): ToastType | undefined {
  if (value && TOAST_TYPES.has(value)) return value as ToastType
  return undefined
}

type SseEvent =
  | { type: 'navigate'; payload: { page: string } }
  | { type: 'notify'; payload: { text: string; type?: string } }
  | { type: 'conversation.reply'; payload: { text: string } }

function parseSseEvent(raw: string): SseEvent | null {
  try {
    const parsed = JSON.parse(raw)
    if (!parsed || typeof parsed !== 'object') return null
    if (parsed.type === 'navigate' && typeof parsed.payload?.page === 'string') return parsed as SseEvent
    if (parsed.type === 'notify' && typeof parsed.payload?.text === 'string') {
      if (parsed.payload.type !== undefined && typeof parsed.payload.type !== 'string') return null
      return parsed as SseEvent
    }
    if (parsed.type === 'conversation.reply' && typeof parsed.payload?.text === 'string') return parsed as SseEvent
    return null
  } catch {
    return null
  }
}

// PATTERN: Use a ref for handlers so the effect runs once and never re-registers.
// This avoids stale closure issues while keeping the listener stable.
export function useCommandBridge(handlers: CommandHandlers): { isConnected: boolean } {
  const handlersRef = useRef(handlers)
  handlersRef.current = handlers

  const [isConnected, setIsConnected] = useState(false)

  useEffect(() => {
    let es: EventSource | null = null
    let retryTimeout: ReturnType<typeof setTimeout> | null = null
    let cancelled = false

    function attachHandlers(source: EventSource) {
      source.onopen = () => setIsConnected(true)

      source.onmessage = (event: MessageEvent<string>) => {
        const evt = parseSseEvent(event.data)
        if (!evt) return
        const h = handlersRef.current
        switch (evt.type) {
          case 'navigate': {
            const page = evt.payload.page
            if (page === 'missions' || page === 'conversations') {
              h.onNavigate(page)
            } else {
              console.warn(`useCommandBridge: unknown navigation page "${page}"`)
            }
            break
          }
          case 'notify':
            h.onNotify(evt.payload.text, toToastType(evt.payload.type))
            break
          case 'conversation.reply':
            h.onConversationReply?.(evt.payload.text)
            break
        }
      }

      source.onerror = () => {
        setIsConnected(false)
        source.close()
        es = null
        // Channel bridge not available — retry probe after 30s.
        if (!cancelled) {
          retryTimeout = setTimeout(tryConnect, 30_000)
        }
      }
    }

    async function tryConnect() {
      if (cancelled) return
      try {
        const ctrl = new AbortController()
        const timer = setTimeout(() => ctrl.abort(), 1500)
        const probe = await fetch(SSE_URL, { method: 'GET', signal: ctrl.signal })
        clearTimeout(timer)
        // If the response is an SSE stream the channel bridge is live.
        if (!probe.headers.get('Content-Type')?.includes('text/event-stream')) return
        // Abort the probe body — we only needed the headers.
        probe.body?.cancel()
      } catch {
        // Channel bridge not reachable — try again in 30s.
        if (!cancelled) {
          retryTimeout = setTimeout(tryConnect, 30_000)
        }
        return
      }
      if (cancelled) return
      es = new EventSource(SSE_URL)
      attachHandlers(es)
    }

    tryConnect()

    return () => {
      cancelled = true
      if (retryTimeout !== null) clearTimeout(retryTimeout)
      es?.close()
      setIsConnected(false)
    }
  }, [])

  return { isConnected }
}
