import { useState, useCallback, useRef, useEffect } from 'react'
import type { Message, BackendMessage } from '../types'
import { isWails, wailsRuntime } from '../lib/wails'

const DEV_BASE = '/api/orchestrator'
const DAEMON_BASE = 'http://127.0.0.1:7331/api'
const CONV_ID_KEY = 'nanika.chat.convId'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const wailsApp = (): any => (window as any)?.go?.main?.App

function appendToLastAssistant(messages: Message[], chunk: string): Message[] {
  const next = [...messages]
  const last = next[next.length - 1]
  if (!last || last.role !== 'assistant') return messages
  next[next.length - 1] = { ...last, text: last.text + chunk }
  return next
}

function setLastAssistantError(messages: Message[], text: string): Message[] {
  const next = [...messages]
  const last = next[next.length - 1]
  if (!last || last.role !== 'assistant') return messages
  next[next.length - 1] = { ...last, text }
  return next
}

export function useChat() {
  const [messages, setMessages] = useState<Message[]>([])
  const [isStreaming, setIsStreaming] = useState(false)
  const [isThinking, setIsThinking] = useState(false)
  const convIdRef = useRef<string | null>(null)

  // Restore last conversation from backend on mount
  useEffect(() => {
    const savedId = localStorage.getItem(CONV_ID_KEY)
    if (!savedId) return
    convIdRef.current = savedId

    const load = async () => {
      try {
        let data: { messages: BackendMessage[] } | null = null
        if (isWails()) {
          const json: string = await wailsApp().GetConversation(savedId) as string
          data = JSON.parse(json) as { messages: BackendMessage[] }
        } else {
          const r = await fetch(`${DAEMON_BASE}/chat/${savedId}`)
          data = r.ok ? (await r.json() as { messages: BackendMessage[] }) : null
        }
        if (!data?.messages?.length) return
        setMessages(data.messages.map(m => ({
          role: m.role as 'user' | 'assistant',
          text: m.content,
        })))
      } catch { /* ignore */ }
    }
    void load()
  }, [])

  const handleSubmit = useCallback(async (text: string) => {
    const msg = text.trim()
    if (!msg) return

    setMessages(prev => [
      ...prev,
      { role: 'user', text: msg },
      { role: 'assistant', text: '' },
    ])
    setIsStreaming(true)
    setIsThinking(true)

    if (isWails()) {
      // ── Wails path: Go bindings proxy to daemon, events via Wails runtime ──
      let convId: string
      try {
        convId = await wailsApp().StartChat(msg, convIdRef.current ?? '') as string
        convIdRef.current = convId
        localStorage.setItem(CONV_ID_KEY, convId)
      } catch {
        setMessages(prev => setLastAssistantError(prev, 'Failed to reach daemon.'))
        setIsStreaming(false)
        setIsThinking(false)
        return
      }

      const rt = wailsRuntime()
      const cleanup = () => {
        rt?.EventsOff('chat:token')
        rt?.EventsOff('chat:done')
        rt?.EventsOff('chat:error')
      }

      rt?.EventsOn('chat:token', (chunk: string) => {
        setIsThinking(false)
        setMessages(prev => appendToLastAssistant(prev, chunk))
      })
      rt?.EventsOn('chat:done', () => {
        setIsStreaming(false)
        setIsThinking(false)
        cleanup()
      })
      rt?.EventsOn('chat:error', (errMsg: string) => {
        setMessages(prev => setLastAssistantError(prev, errMsg || 'Unknown error'))
        setIsStreaming(false)
        setIsThinking(false)
        cleanup()
      })

      // Fire-and-forget: Go goroutine streams tokens back via Wails events.
      wailsApp().StreamChat(convId)
      return
    }

    // ── HTTP fallback (dev mode / standalone Vite) ──────────────────────────
    let convId: string
    try {
      const res = await fetch(`${DEV_BASE}/chat`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ message: msg, conversation_id: convIdRef.current ?? undefined }),
      })
      if (!res.ok) throw new Error(`chat start failed: ${res.status}`)
      const body = await res.json() as { conversation_id: string }
      convId = body.conversation_id
      convIdRef.current = convId
      localStorage.setItem(CONV_ID_KEY, convId)
    } catch {
      setMessages(prev => setLastAssistantError(prev, 'Failed to reach daemon.'))
      setIsStreaming(false)
      setIsThinking(false)
      return
    }

    const es = new EventSource(`${DEV_BASE}/chat/${convId}/stream`)

    es.addEventListener('token', (e: MessageEvent<string>) => {
      const { text: chunk } = JSON.parse(e.data) as { text: string }
      setIsThinking(false)
      setMessages(prev => appendToLastAssistant(prev, chunk))
    })

    es.addEventListener('done', () => {
      setIsStreaming(false)
      setIsThinking(false)
      es.close()
    })

    es.addEventListener('error', (e: MessageEvent<string>) => {
      const errMsg = e.data
        ? (JSON.parse(e.data) as { message: string }).message
        : 'Unknown error'
      setMessages(prev => setLastAssistantError(prev, errMsg))
      setIsStreaming(false)
      setIsThinking(false)
      es.close()
    })

    es.onerror = () => {
      setMessages(prev => setLastAssistantError(prev, 'Stream disconnected.'))
      setIsStreaming(false)
      setIsThinking(false)
      es.close()
    }
  }, [])

  const resetConversation = useCallback(() => {
    convIdRef.current = null
    localStorage.removeItem(CONV_ID_KEY)
    setMessages([])
    setIsStreaming(false)
    setIsThinking(false)
  }, [])

  return { messages, handleSubmit, isStreaming, isThinking, resetConversation }
}
