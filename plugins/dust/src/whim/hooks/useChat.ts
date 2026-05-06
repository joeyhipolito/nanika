import { useState, useEffect, useRef, useCallback } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { Component } from '../types'
import type { ChatEvent, ThreadMeta, StoredMessage } from '../types'
import { unwrapData } from '../types'

export interface UseChatReturn {
  conversation: Component[]
  threads: ThreadMeta[]
  activeThreadId: string | null
  setActiveThreadId: (id: string) => void
  ask: (prompt: string, opts?: { model?: string; mode?: string; permissions?: string[] }) => void
  newThread: (initialPrompt?: string) => void
  loading: boolean
  error: string | null
}

// chat_load_thread: skipping the Rust wrapper — dispatch_action(chat, list_messages)
// returns StoredMessage[] which we map to Component[] here. This avoids adding a
// new Tauri command for functionality already expressible from the frontend.

export function useChat(threadId?: string): UseChatReturn {
  const [conversation, setConversation] = useState<Component[]>([])
  const [threads, setThreads] = useState<ThreadMeta[]>([])
  const [activeThreadId, setActiveThreadIdState] = useState<string | null>(threadId ?? null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // Synchronously-updated ref mirrors activeThreadId for the stale-thread guard
  // in the event listener (matches App.tsx:665 subscribedThreadRef pattern).
  const activeThreadIdRef = useRef<string | null>(threadId ?? null)

  // ── Fetch thread list on mount ────────────────────────────────────────────

  useEffect(() => {
    invoke<unknown>('dispatch_action', {
      pluginId: 'chat',
      capabilityId: 'ask',
      actionId: 'list_threads',
      params: {},
    })
      .then(res => {
        const data = unwrapData<unknown>(res)
        if (Array.isArray(data)) setThreads(data as ThreadMeta[])
      })
      .catch(err => setError(String(err)))
  }, [])

  // ── Single persistent event listener (no deps) ───────────────────────────
  // Subscription lifecycle is managed by setActiveThreadId / newThread / ask,
  // not by this effect — matches the App.tsx:665 pattern.

  useEffect(() => {
    let unlisten: (() => void) | null = null

    listen<ChatEvent>('dust://chat-event', ({ payload }) => {
      // Stale-thread guard — drop events that don't belong to the current
      // subscription. A null incoming thread_id is only accepted when we're
      // also in null-thread state (first delta on a new_thread before the
      // server has assigned an id).
      const subscribed = activeThreadIdRef.current
      if (
        payload.thread_id !== null &&
        subscribed !== null &&
        payload.thread_id !== subscribed
      ) {
        return
      }

      // First delta carrying a thread_id pins the active thread when we're
      // in the null-state (subscribed with null for a fresh new_thread).
      if (payload.thread_id && subscribed === null) {
        activeThreadIdRef.current = payload.thread_id
        setActiveThreadIdState(payload.thread_id)
      }

      if (payload.event_type === 'data_updated') {
        // Atomic replace — never append. Backend sends the full Component[].
        setConversation(payload.data as Component[])
        setLoading(false)
      } else if (payload.event_type === 'error') {
        const msg =
          typeof payload.data === 'string'
            ? payload.data
            : ((payload.data as { message?: string })?.message ?? 'An error occurred')
        setError(msg)
        setLoading(false)
      }
    })
      .then(f => { unlisten = f })

    return () => {
      unlisten?.()
      invoke('chat_unsubscribe').catch(console.error)
    }
  }, []) // eslint-disable-line react-hooks/exhaustive-deps

  // ── Subscribe to initial threadId arg on mount ───────────────────────────

  useEffect(() => {
    if (threadId !== undefined) {
      setActiveThreadId(threadId)
    }
  }, []) // eslint-disable-line react-hooks/exhaustive-deps

  // ── setActiveThreadId ─────────────────────────────────────────────────────
  // Synchronously updates the ref (before the async subscribe) so the event
  // listener never accepts events from the old thread during the switchover.

  const setActiveThreadId = useCallback((id: string) => {
    // Update ref first — event listener reads this synchronously.
    activeThreadIdRef.current = id
    setActiveThreadIdState(id)
    setLoading(true)
    setError(null)

    invoke('chat_unsubscribe')
      .then(() => invoke('chat_subscribe', { threadId: id }))
      .then(() =>
        invoke<unknown>('dispatch_action', {
          pluginId: 'chat',
          capabilityId: 'ask',
          actionId: 'list_messages',
          params: { thread_id: id },
        }),
      )
      .then(res => {
        const data = unwrapData<unknown>(res)
        const msgs = Array.isArray(data) ? (data as StoredMessage[]) : []
        setConversation(
          msgs.map(m => ({
            type: 'agent_turn' as const,
            role: m.role,
            content: m.content,
            streaming: false,
            timestamp: m.created_at,
          })),
        )
        setLoading(false)
      })
      .catch(err => {
        setError(String(err))
        setLoading(false)
      })
  }, [])

  // ── ask ───────────────────────────────────────────────────────────────────

  const ask = useCallback(
    (prompt: string, opts?: { model?: string; mode?: string; permissions?: string[] }) => {
      setLoading(true)
      setError(null)
      invoke('dispatch_action', {
        pluginId: 'chat',
        capabilityId: 'ask',
        actionId: 'ask',
        params: {
          thread_id: activeThreadIdRef.current,
          prompt,
          ...(opts?.model ? { model: opts.model } : {}),
          ...(opts?.mode ? { mode: opts.mode } : {}),
        },
      }).catch(err => {
        setError(String(err))
        setLoading(false)
      })
    },
    [],
  )

  // ── newThread ─────────────────────────────────────────────────────────────
  // Clears the active thread id ref to null before subscribing so the first
  // data_updated event (carrying the server-assigned thread_id) pins the thread.

  const newThread = useCallback((initialPrompt?: string) => {
    setConversation([])
    activeThreadIdRef.current = null
    setActiveThreadIdState(null)
    setLoading(true)
    setError(null)

    invoke('chat_subscribe', { threadId: null })
      .then(() =>
        invoke('dispatch_action', {
          pluginId: 'chat',
          capabilityId: 'ask',
          actionId: 'new_thread',
          params: initialPrompt ? { initial_prompt: initialPrompt } : {},
        }),
      )
      .catch(err => {
        setError(String(err))
        setLoading(false)
      })
  }, [])

  return {
    conversation,
    threads,
    activeThreadId,
    setActiveThreadId,
    ask,
    newThread,
    loading,
    error,
  }
}
