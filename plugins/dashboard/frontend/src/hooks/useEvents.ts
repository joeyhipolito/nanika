import { useState, useEffect, useRef } from 'react'
import type { OrchestratorEvent } from '../types'
import { isWails, wailsRuntime } from '../lib/wails'

const BASE = '/api/orchestrator'
const MAX_EVENTS = 200
const RECONNECT_DELAY_MS = 3000

export type UseEventsOptions = {
  /** Filter to events for a specific mission. Maps to ?mission_id= on the stream. */
  missionId?: string
  /** Filter to specific event types. Maps to ?types= on the stream. */
  types?: string[]
  /** Maximum number of events to retain in the ring buffer. Defaults to 200. */
  maxEvents?: number
}

type EventsState = {
  events: OrchestratorEvent[]
  connected: boolean
}

export function useEvents(options: UseEventsOptions = {}) {
  const { missionId, types, maxEvents = MAX_EVENTS } = options

  const [state, setState] = useState<EventsState>({ events: [], connected: false })
  const lastSeqRef = useRef<number>(0)

  useEffect(() => {
    if (isWails()) {
      // ---- Wails path: listen to events forwarded by the Go bridge ----
      const rt = wailsRuntime()

      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const offEvent = rt?.EventsOn('orchestrator:event', (data: any) => {
        try {
          const raw = typeof data === 'string' ? data : JSON.stringify(data)
          const event = JSON.parse(raw) as OrchestratorEvent
          if (missionId && event.mission_id !== missionId) return
          if (types && types.length > 0 && !types.includes(event.type)) return
          if (event.sequence != null) lastSeqRef.current = event.sequence
          setState(s => ({
            ...s,
            events: [...s.events.slice(-(maxEvents - 1)), event],
          }))
        } catch {
          // drop unparseable frames
        }
      })

      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const offConnected = rt?.EventsOn('orchestrator:connected', (connected: any) => {
        setState(s => ({ ...s, connected: Boolean(connected) }))
      })

      return () => {
        if (typeof offEvent === 'function') offEvent()
        if (typeof offConnected === 'function') offConnected()
        setState({ events: [], connected: false })
      }
    }

    // ---- SSE fallback: used when running standalone with Vite dev server ----
    let es: EventSource | null = null
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null
    let cancelled = false

    const connect = () => {
      const params = new URLSearchParams()
      if (missionId) params.set('mission_id', missionId)
      if (types && types.length > 0) params.set('types', types.join(','))

      const url = `${BASE}/events${params.toString() ? `?${params.toString()}` : ''}`
      es = new EventSource(url)

      es.onopen = () => {
        if (!cancelled) setState(s => ({ ...s, connected: true }))
      }

      // The daemon always sets `event: <type>` on SSE frames, which means named
      // events only fire on addEventListener(type, ...) — NOT onmessage (which
      // only fires for the default "message" type). We handle both paths:
      // 1. Named events via addEventListener for all known event types.
      // 2. onmessage as a fallback for any unnamed frames.
      const knownEventTypes = [
        'mission.started', 'mission.completed', 'mission.failed', 'mission.cancelled',
        'phase.started', 'phase.completed', 'phase.failed', 'phase.skipped', 'phase.retrying',
        'worker.spawned', 'worker.output', 'worker.completed', 'worker.failed',
        'decompose.started', 'decompose.completed', 'decompose.fallback',
        'learning.extracted', 'learning.stored',
        'dag.dependency_resolved', 'dag.phase_dispatched',
        'role.handoff', 'contract.validated', 'contract.violated',
        'persona.contract_violation',
        'review.findings_emitted', 'review.external_requested',
        'git.worktree_created', 'git.committed', 'git.pr_created',
        'system.error', 'system.checkpoint_saved',
        'signal.scope_expansion', 'signal.replan_required', 'signal.human_decision_needed',
        'file_overlap.detected', 'security.invisible_chars_stripped',
        'nen.finding_critical', 'nen.finding_warning', 'security.injection_detected',
        // message is the SSE default type — included for unnamed frames
        'message',
      ]

      const handleSSEData = (data: string) => {
        if (cancelled) return
        try {
          const event = JSON.parse(data) as OrchestratorEvent
          if (event.sequence != null) lastSeqRef.current = event.sequence
          setState(s => ({
            ...s,
            events: [...s.events.slice(-(maxEvents - 1)), event],
          }))
        } catch {
          // drop unparseable frames
        }
      }

      const listeners: Array<[string, EventListener]> = []
      for (const evType of knownEventTypes) {
        const handler = (e: Event) => handleSSEData((e as MessageEvent).data)
        es.addEventListener(evType, handler)
        listeners.push([evType, handler])
      }

      // onmessage handles any unnamed SSE frames (type === 'message' or no event: field).
      es.onmessage = (e: MessageEvent) => handleSSEData(e.data)

      es.onerror = () => {
        if (cancelled) return
        setState(s => ({ ...s, connected: false }))
        // Remove all named event listeners before closing.
        for (const [evType, handler] of listeners) {
          es?.removeEventListener(evType, handler)
        }
        es?.close()
        reconnectTimer = setTimeout(() => {
          if (!cancelled) connect()
        }, RECONNECT_DELAY_MS)
      }
    }

    connect()

    return () => {
      cancelled = true
      if (reconnectTimer) clearTimeout(reconnectTimer)
      // EventSource.close() terminates the connection and clears all listeners.
      es?.close()
      es = null
      setState({ events: [], connected: false })
    }
  }, [missionId, types?.join(','), maxEvents]) // eslint-disable-line react-hooks/exhaustive-deps

  const clearEvents = () => setState(s => ({ ...s, events: [] }))

  return {
    events: state.events,
    connected: state.connected,
    clearEvents,
  }
}
