import { useCallback, useEffect, useRef, useState, type CSSProperties } from 'react'
import type { DAGResponse, OrchestratorEvent } from '../types'
import { neutral, status as statusColors } from '../colors'
import { Badge } from './ui/badge'
import { Card } from './ui/card'
import { isWails, wailsRuntime } from '../lib/wails'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const wailsApp = (): any => (window as any)?.go?.main?.App

const BASE = '/api/orchestrator'

const TERMINAL_EVENTS = new Set([
  'mission.completed',
  'mission.failed',
  'mission.cancelled',
  'mission.stalled',
])

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface LiveEvent {
  mission_id?: string
  phase: string
  persona: string
  tool_name?: string
  file_path?: string
  chunk?: string
  streaming: boolean
  duration?: string
}

type PhaseStatus = 'running' | 'completed' | 'failed'

interface PhaseState {
  id: string
  persona: string
  status: PhaseStatus
  startedAt: number
  duration: string | null
  currentTool: string | null
  currentFilePath: string | null
  lines: string[]
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function phaseBadgeStyle(s: PhaseStatus): CSSProperties {
  switch (s) {
    case 'running':   return { color: statusColors.accent, borderColor: statusColors.accent }
    case 'completed': return { color: statusColors.success, borderColor: statusColors.success }
    case 'failed':    return { color: statusColors.error, borderColor: statusColors.error }
  }
}

function formatElapsed(ms: number): string {
  const s = Math.floor(ms / 1000)
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  return `${m}m ${s % 60}s`
}

function toolIcon(toolName: string): string {
  const t = toolName.toLowerCase()
  if (t.includes('bash') || t.includes('shell') || t.includes('exec')) return '⚙'
  if (t.includes('write') || t.includes('edit'))  return '✏'
  if (t.includes('read'))  return '📄'
  if (t.includes('grep') || t.includes('search') || t.includes('glob')) return '🔍'
  if (t.includes('web'))   return '🌐'
  return '◈'
}

// ---------------------------------------------------------------------------
// PhaseCard
// ---------------------------------------------------------------------------

interface PhaseCardProps {
  phase: PhaseState
  name: string | undefined
  now: number
}

function PhaseCard({ phase, name, now }: PhaseCardProps) {
  const tailRef = useRef<HTMLPreElement>(null)

  // Auto-scroll output tail whenever lines update.
  useEffect(() => {
    const el = tailRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [phase.lines])

  const elapsed = phase.status === 'running' ? formatElapsed(now - phase.startedAt) : null
  const durationLabel = phase.duration ?? elapsed

  return (
    <Card
      className="flex flex-col gap-1.5 p-3"
      style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
    >
      {/* Header */}
      <div className="flex items-center gap-1.5 min-w-0">
        <span
          className="text-[11px] font-mono font-semibold truncate flex-1 min-w-0"
          style={{ color: 'var(--text-primary)' }}
        >
          {name ?? phase.id}
        </span>
        <Badge
          variant="outline"
          style={{ fontSize: 10, flexShrink: 0, ...phaseBadgeStyle(phase.status) }}
        >
          {phase.status}
        </Badge>
        {phase.persona && (
          <Badge
            variant="outline"
            className="flex-shrink-0"
            style={{ fontSize: 9, color: 'var(--accent)', borderColor: 'var(--accent)', opacity: 0.8 }}
          >
            {phase.persona}
          </Badge>
        )}
        {durationLabel && (
          <span
            className="text-[10px] font-mono flex-shrink-0"
            style={{ color: 'var(--text-secondary)' }}
          >
            {durationLabel}
          </span>
        )}
      </div>

      {/* Current tool indicator — only shown while running */}
      {phase.currentTool && phase.status === 'running' && (
        <div
          className="flex flex-col gap-0.5"
          aria-live="polite"
          aria-atomic="true"
        >
          <div className="flex items-center gap-1.5 min-w-0">
            <span style={{ color: 'var(--accent)', fontSize: 11 }} aria-hidden="true">
              {toolIcon(phase.currentTool)}
            </span>
            <span
              className="text-[10px] font-mono truncate"
              style={{ color: 'var(--accent)' }}
            >
              {phase.currentTool}
            </span>
          </div>
          {phase.currentFilePath && (
            <span
              className="text-[9px] font-mono truncate pl-4"
              style={{ color: 'var(--text-secondary)', opacity: 0.8 }}
              title={phase.currentFilePath}
            >
              {phase.currentFilePath}
            </span>
          )}
        </div>
      )}

      {/* Live output tail — last 10 lines */}
      {phase.lines.length > 0 && (
        <pre
          ref={tailRef}
          className="rounded p-1.5 overflow-x-auto"
          style={{
            background: neutral.canvasBg,
            color: neutral.textSecondary,
            fontFamily: 'monospace',
            fontSize: 10,
            maxHeight: 120,
            overflowY: 'auto',
            margin: 0,
            whiteSpace: 'pre-wrap',
            wordBreak: 'break-all',
          }}
          aria-label={`Output for phase ${name ?? phase.id}`}
        >
          {phase.lines.join('\n')}
        </pre>
      )}
    </Card>
  )
}

// ---------------------------------------------------------------------------
// LiveWorkerPanel
// ---------------------------------------------------------------------------

export interface LiveWorkerPanelProps {
  missionId: string
  dag: DAGResponse | null
}

export function LiveWorkerPanel({ missionId, dag }: LiveWorkerPanelProps) {
  const [phases, setPhases] = useState<Map<string, PhaseState>>(new Map())
  const [isDone, setIsDone] = useState(false)
  const [now, setNow] = useState(() => Date.now())

  // Tick every second for elapsed timers on running phases.
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(id)
  }, [])

  const applyEvent = useCallback((ev: LiveEvent) => {
    setPhases(prev => {
      const next = new Map(prev)
      const existing = next.get(ev.phase)

      const base: PhaseState = existing ?? {
        id: ev.phase,
        persona: ev.persona,
        status: 'running',
        startedAt: Date.now(),
        duration: null,
        currentTool: null,
        currentFilePath: null,
        lines: [],
      }

      let updated: PhaseState = { ...base }

      if (ev.persona && !updated.persona) {
        updated.persona = ev.persona
      }

      if (ev.tool_name) {
        updated.currentTool = ev.tool_name
      }

      if (ev.file_path !== undefined) {
        updated.currentFilePath = ev.file_path || null
      }

      if (ev.chunk) {
        const newLines = [...updated.lines, ...ev.chunk.split('\n').filter(l => l !== '')]
        updated.lines = newLines.slice(-10)
      }

      // duration field signals phase completion
      if (ev.duration) {
        updated.duration = ev.duration
        updated.status = 'completed'
        updated.currentTool = null
        updated.currentFilePath = null
      }

      next.set(ev.phase, updated)
      return next
    })
  }, [])

  // Subscribe to worker output for this mission.
  useEffect(() => {
    setPhases(new Map())
    setIsDone(false)

    if (isWails()) {
      // ---- Wails path ----
      // worker:output comes from startLiveBridge (live.go) — compact projected payload
      // with mission_id, phase, persona, tool_name, file_path, chunk, streaming, duration.
      const rt = wailsRuntime()

      const offOutput = rt?.EventsOn('worker:output', (payload: unknown) => {
        try {
          const raw = typeof payload === 'string' ? payload : JSON.stringify(payload)
          const ev = JSON.parse(raw) as LiveEvent
          // Global bridge emits for all missions; filter to this panel's mission.
          if (ev.mission_id && ev.mission_id !== missionId) return
          applyEvent(ev)
        } catch {
          // drop unparseable frames
        }
      })

      // Watch orchestrator:event for mission terminal lifecycle events.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const offEvent = rt?.EventsOn('orchestrator:event', (data: any) => {
        try {
          const raw = typeof data === 'string' ? data : JSON.stringify(data)
          const ev = JSON.parse(raw) as OrchestratorEvent
          if (ev.mission_id !== missionId) return
          if (TERMINAL_EVENTS.has(ev.type)) {
            setIsDone(true)
            setPhases(prev => {
              const next = new Map(prev)
              next.forEach((p, k) => {
                if (p.status === 'running') {
                  next.set(k, { ...p, status: 'completed', currentTool: null, currentFilePath: null })
                }
              })
              return next
            })
          }
        } catch {
          // drop unparseable frames
        }
      })

      return () => {
        if (typeof offOutput === 'function') offOutput()
        if (typeof offEvent === 'function') offEvent()
      }
    }

    // ---- SSE fallback: used when running with standalone Vite dev server ----
    const url = `${BASE}/missions/${encodeURIComponent(missionId)}/live`
    const es = new EventSource(url)

    es.addEventListener('stream:done', () => {
      setIsDone(true)
      setPhases(prev => {
        const next = new Map(prev)
        next.forEach((p, k) => {
          if (p.status === 'running') {
            next.set(k, { ...p, status: 'completed', currentTool: null, currentFilePath: null })
          }
        })
        return next
      })
      es.close()
    })

    es.onmessage = (e: MessageEvent) => {
      try {
        applyEvent(JSON.parse(e.data) as LiveEvent)
      } catch {
        // drop unparseable frames
      }
    }

    return () => es.close()
  }, [missionId, applyEvent])

  // Build phase name map from DAG so we can show human-readable names.
  const phaseNames = new Map<string, string>()
  if (dag) {
    for (const n of dag.nodes) {
      phaseNames.set(n.id, n.name || n.id)
    }
  }

  const phaseList = [...phases.values()]

  if (phaseList.length === 0) {
    return (
      <div className="flex items-center gap-1.5 py-2" aria-live="polite">
        <span
          className="w-1.5 h-1.5 rounded-full animate-pulse flex-shrink-0"
          style={{ background: 'var(--accent)' }}
          aria-hidden="true"
        />
        <span className="text-[11px]" style={{ color: 'var(--text-secondary)' }}>
          Waiting for worker output…
        </span>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-2 mt-2" aria-label="Live worker output">
      {phaseList.map(p => (
        <PhaseCard
          key={p.id}
          phase={p}
          name={phaseNames.get(p.id)}
          now={now}
        />
      ))}
      {!isDone && (
        <div className="flex items-center gap-1.5" aria-live="polite">
          <span
            className="w-1.5 h-1.5 rounded-full flex-shrink-0"
            style={{ background: 'var(--color-success)', animation: 'pulse 2s cubic-bezier(0.4,0,0.6,1) infinite' }}
            aria-hidden="true"
          />
          <span className="text-[10px]" style={{ color: 'var(--color-success)' }}>live</span>
        </div>
      )}
    </div>
  )
}
