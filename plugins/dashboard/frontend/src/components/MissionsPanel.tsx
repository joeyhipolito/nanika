import { useState, useCallback, useEffect } from 'react'
import { motion, AnimatePresence } from 'motion/react'
import { useMissions } from '../hooks/useMissions'
import type { OrchestratorMission, OrchestratorEvent, MissionStatus, DAGResponse } from '../types'
import { LiveWorkerPanel } from './LiveWorkerPanel'
import { Badge } from './ui/badge'
import { Button } from './ui/button'
import { Card } from './ui/card'

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function stripFrontmatter(text: string): string {
  return text.replace(/^---\n[\s\S]*?\n---\n/, '')
}

function extractFirstHeading(text: string): string | null {
  const stripped = stripFrontmatter(text)
  const match = stripped.match(/^# (.+)$/m)
  return match ? match[1].trim() : null
}

function markdownToHtml(text: string): string {
  if (!text) return ''

  // Strip YAML frontmatter before rendering
  let html = stripFrontmatter(text)

  // Escape HTML special chars first
  html = html
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;')

  // Headers: ## text → <h2>text</h2>
  html = html.replace(/^## (.+)$/gm, '<h2>$1</h2>')

  // Bold: **text** → <strong>text</strong>
  html = html.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')

  // Backticks: `text` → <code>text</code>
  const codeStyle = `font-family:'SF Mono','Fira Code','Cascadia Code',monospace;white-space:pre;overflow-x:auto`
  html = html.replace(/`([^`]+)`/g, `<code style="${codeStyle}">$1</code>`)

  // Lists: - item → <ul><li>item</li></ul>
  const lines = html.split('\n')
  let inList = false
  const result: string[] = []

  for (const line of lines) {
    const match = line.match(/^- (.+)/)
    if (match) {
      if (!inList) {
        result.push('<ul>')
        inList = true
      }
      result.push(`<li>${match[1]}</li>`)
    } else {
      if (inList) {
        result.push('</ul>')
        inList = false
      }
      result.push(line)
    }
  }

  if (inList) {
    result.push('</ul>')
  }

  html = result.join('\n')

  // Newlines → <br>
  html = html.replace(/\n/g, '<br/>')

  return html
}

function postChannelEvent(action: string, context: Record<string, unknown> = {}) {
  fetch('/api/channel', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ action, context }),
  }).catch(() => {
    // channel bridge not running — silently ignore
  })
}

const STATUS_LABELS: Record<MissionStatus | '', string> = {
  in_progress: 'running',
  completed: 'done',
  failed: 'failed',
  stalled: 'stalled',
  cancelled: 'cancelled',
  '': 'unknown',
}

const PHASE_STATUS_LABELS: Record<string, string> = {
  completed: 'done',
  failed: 'failed',
  in_progress: 'running',
  pending: 'pending',
}

function statusBadgeStyle(s: MissionStatus | ''): React.CSSProperties {
  switch (s) {
    case 'in_progress': return { color: 'var(--accent)', borderColor: 'var(--accent)' }
    case 'completed':   return { color: 'var(--color-success)', borderColor: 'var(--color-success)' }
    case 'failed':      return { color: 'var(--color-error)', borderColor: 'var(--color-error)' }
    case 'stalled':     return { color: 'var(--color-warning)', borderColor: 'var(--color-warning)' }
    default:            return { color: 'var(--text-secondary)', borderColor: 'var(--pill-border)' }
  }
}

function phaseStatusStyle(s: string): React.CSSProperties {
  switch (s) {
    case 'completed':  return { color: 'var(--color-success)', borderColor: 'var(--color-success)' }
    case 'failed':     return { color: 'var(--color-error)', borderColor: 'var(--color-error)' }
    case 'in_progress': return { color: 'var(--accent)', borderColor: 'var(--accent)' }
    default:           return { color: 'var(--text-secondary)', borderColor: 'var(--pill-border)' }
  }
}

function formatMissionId(id: string): string {
  const dashIdx = id.lastIndexOf('-')
  return dashIdx !== -1 ? id.slice(dashIdx + 1) : id.slice(-8)
}

function formatRelativeTime(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime()
  if (diff < 60_000) return 'just now'
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`
  return `${Math.floor(diff / 86_400_000)}d ago`
}

function formatTimestamp(iso: string): string {
  return new Date(iso).toLocaleString([], {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  })
}

function extractPhaseDurations(events: OrchestratorEvent[]): Map<string, string> {
  const durations = new Map<string, string>()
  for (const ev of events) {
    if ((ev.type === 'phase.completed' || ev.type === 'phase.failed') && ev.phase_id) {
      const dur = ev.data?.duration as string | undefined
      if (dur) durations.set(ev.phase_id, dur)
    }
  }
  return durations
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

interface MissionDetailHeaderProps {
  mission: OrchestratorMission
}

function MissionDetailHeader({ mission: m }: MissionDetailHeaderProps) {
  return (
    <div
      className="flex flex-col gap-2 pb-3 mb-3"
      style={{ borderBottom: '1px solid var(--pill-border)' }}
    >
      {m.task && (
        <div
          className="text-[12px] font-medium leading-snug prose prose-sm"
          style={{ color: 'var(--text-primary)' }}
          dangerouslySetInnerHTML={{ __html: markdownToHtml(m.task) }}
        />
      )}
      <div className="flex items-center gap-2 flex-wrap">
        <Badge
          variant="outline"
          style={{ fontSize: 10, ...statusBadgeStyle(m.status) }}
        >
          {STATUS_LABELS[m.status]}
        </Badge>
        <span className="text-[10px] font-mono" style={{ color: 'var(--text-secondary)' }}>
          {formatMissionId(m.mission_id)}
        </span>
      </div>
      <div className="flex items-center gap-3 flex-wrap">
        <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
          updated {formatTimestamp(m.modified_at)} · {formatRelativeTime(m.modified_at)}
        </span>
        {(m.phases ?? 0) > 0 && (
          <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
            {m.phases} phases
          </span>
        )}
        {m.event_count > 0 && (
          <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
            {m.event_count} events
          </span>
        )}
      </div>
    </div>
  )
}

interface PhaseProgressTableProps {
  dag: DAGResponse
  durations: Map<string, string>
}

function PhaseProgressTable({ dag, durations }: PhaseProgressTableProps) {
  if (dag.nodes.length === 0) return null

  return (
    <div role="list" aria-label="Phase progress">
      {dag.nodes.map((node, i) => {
        const duration = durations.get(node.id) ?? null
        const statusLabel = PHASE_STATUS_LABELS[node.status] ?? node.status

        return (
          <div
            key={node.id}
            role="listitem"
            className="flex items-center gap-2 py-1.5 text-[11px]"
            style={{
              borderBottom:
                i < dag.nodes.length - 1 ? '1px solid var(--pill-border)' : undefined,
            }}
          >
            <span
              className="flex-1 min-w-0 truncate font-mono"
              style={{ color: 'var(--text-primary)' }}
              title={node.name || node.id}
            >
              {node.name || node.id}
            </span>
            <Badge
              variant="outline"
              style={{ fontSize: 9, flexShrink: 0, ...phaseStatusStyle(node.status) }}
            >
              {statusLabel}
            </Badge>
            {duration && (
              <span
                className="text-[10px] font-mono flex-shrink-0"
                style={{ color: 'var(--text-secondary)' }}
              >
                {duration}
              </span>
            )}
            {node.persona && (
              <span
                className="text-[10px] truncate max-w-[96px] flex-shrink-0"
                style={{ color: 'var(--accent)', opacity: 0.8 }}
                title={node.persona}
              >
                {node.persona}
              </span>
            )}
          </div>
        )
      })}
    </div>
  )
}

interface MissionDetailViewProps {
  mission: OrchestratorMission
  dag: DAGResponse | null
  dagLoading: boolean
  getMissionEvents: (id: string) => Promise<OrchestratorEvent[]>
}

function MissionDetailView({
  mission,
  dag,
  dagLoading,
  getMissionEvents,
}: MissionDetailViewProps) {
  const [durations, setDurations] = useState<Map<string, string>>(new Map())

  useEffect(() => {
    getMissionEvents(mission.mission_id).then(evs => {
      setDurations(extractPhaseDurations(evs))
    })
  }, [mission.mission_id, getMissionEvents])

  const isRunning = mission.status === 'in_progress'

  return (
    <div className="mission-detail-inner">
      <MissionDetailHeader mission={mission} />

      {dagLoading && (
        <div className="mission-dag-loading">Loading phases…</div>
      )}

      {!dagLoading && dag && dag.nodes.length > 0 && (
        <div className="mb-3">
          <h5
            className="text-[10px] font-semibold uppercase tracking-wider mb-2"
            style={{ color: 'var(--text-secondary)' }}
          >
            Phases
          </h5>
          <PhaseProgressTable dag={dag} durations={durations} />
        </div>
      )}

      {!dagLoading && !dag && (
        <div className="mission-dag-empty">No phase data available.</div>
      )}

      {isRunning && <LiveWorkerPanel missionId={mission.mission_id} dag={dag} />}
    </div>
  )
}

interface MissionRowProps {
  mission: OrchestratorMission
  isExpanded: boolean
  dag: DAGResponse | null
  dagLoading: boolean
  cancelling: boolean
  onToggle: () => void
  onCancel: (e: React.MouseEvent) => void
  getMissionEvents: (id: string) => Promise<OrchestratorEvent[]>
}

function MissionRow({
  mission: m,
  isExpanded,
  dag,
  dagLoading,
  cancelling,
  onToggle,
  onCancel,
  getMissionEvents,
}: MissionRowProps) {
  return (
    <li>
      <Card
        className="flex flex-col gap-1 p-3 cursor-pointer transition-colors"
        onClick={onToggle}
        role="button"
        tabIndex={0}
        onKeyDown={e => { if (e.key === 'Enter' || e.key === ' ') onToggle() }}
        aria-expanded={isExpanded}
        style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
      >
        <div className="missions-list-item-row">
          <code className="missions-list-item-id">{formatMissionId(m.mission_id)}</code>
          <Badge
            variant="outline"
            className="flex-shrink-0"
            style={{ fontSize: 10, ...statusBadgeStyle(m.status) }}
          >
            {STATUS_LABELS[m.status]}
          </Badge>
          {m.status === 'in_progress' && (
            <Button
              variant="destructive"
              size="sm"
              onClick={onCancel}
              disabled={cancelling}
              aria-label="Cancel mission"
              className="h-auto py-0 px-1.5 text-[11px] flex-shrink-0"
            >
              {cancelling ? '…' : '✕'}
            </Button>
          )}
          <span className="missions-expand-caret" aria-hidden="true">
            {isExpanded ? '▲' : '▼'}
          </span>
        </div>
        {m.task && (
          <p className="missions-list-item-task">
            {extractFirstHeading(m.task) ?? stripFrontmatter(m.task).trim().split('\n')[0]}
          </p>
        )}
        <div className="missions-list-item-meta">
          {(m.phases ?? 0) > 0 && <span>{m.phases} phases</span>}
          <span>{formatRelativeTime(m.modified_at)}</span>
        </div>
      </Card>

      <AnimatePresence>
        {isExpanded && (
          <motion.div
            className="mission-detail"
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: 'auto', opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.2 }}
            style={{ overflow: 'hidden' }}
          >
            <MissionDetailView
              mission={m}
              dag={dag}
              dagLoading={dagLoading}
              getMissionEvents={getMissionEvents}
            />
          </motion.div>
        )}
      </AnimatePresence>
    </li>
  )
}

// ---------------------------------------------------------------------------
// Main panel
// ---------------------------------------------------------------------------

interface MissionsPanelProps {
  isConnected?: boolean
}

export function MissionsPanel({ isConnected = false }: MissionsPanelProps) {
  const { missions, loading, error, refresh, getMissionDAG, getMissionEvents, cancelMission } = useMissions()
  const [expandedId, setExpandedId] = useState<string | null>(null)
  const [dags, setDags] = useState<Record<string, DAGResponse | null>>({})
  const [dagLoading, setDagLoading] = useState<string | null>(null)
  const [cancelling, setCancelling] = useState<string | null>(null)
  const [statusFilter, setStatusFilter] = useState<'all' | 'running' | 'completed' | 'failed'>('all')
  const [searchQuery, setSearchQuery] = useState('')

  // Filter missions by status and search query
  const filtered = missions.filter(m => {
    // Status filter
    if (statusFilter === 'running' && m.status !== 'in_progress') return false
    if (statusFilter === 'completed' && m.status !== 'completed') return false
    if (statusFilter === 'failed' && m.status !== 'failed') return false

    // Search filter (match against task)
    if (searchQuery && !m.task.toLowerCase().includes(searchQuery.toLowerCase())) return false

    return true
  })

  // Sort with running missions first, then by recency
  const sorted = [...filtered].sort((a, b) => {
    // Running missions first
    if (a.status === 'in_progress' && b.status !== 'in_progress') return -1
    if (a.status !== 'in_progress' && b.status === 'in_progress') return 1
    // Then by recency
    return new Date(b.modified_at).getTime() - new Date(a.modified_at).getTime()
  })

  const handleToggle = useCallback(async (missionId: string) => {
    if (expandedId === missionId) {
      setExpandedId(null)
      return
    }
    setExpandedId(missionId)
    postChannelEvent('mission.select', { mission_id: missionId })

    if (!(missionId in dags)) {
      setDagLoading(missionId)
      const dag = await getMissionDAG(missionId)
      setDags(prev => ({ ...prev, [missionId]: dag }))
      setDagLoading(null)
    }
  }, [expandedId, dags, getMissionDAG])

  const handleCancel = useCallback(async (e: React.MouseEvent, missionId: string) => {
    e.stopPropagation()
    setCancelling(missionId)
    await cancelMission(missionId)
    setCancelling(null)
    postChannelEvent('mission.cancel', { mission_id: missionId })
  }, [cancelMission])

  return (
    <div className="missions-panel">
      <div className="missions-panel-toolbar">
        <span
          className={`missions-daemon-dot missions-daemon-dot--${isConnected ? 'on' : 'off'}`}
          title={isConnected ? 'Daemon connected' : 'Daemon offline'}
          aria-label={isConnected ? 'Daemon connected' : 'Daemon offline'}
        />
        <span className="missions-panel-count">
          {sorted.length} {sorted.length === 1 ? 'mission' : 'missions'}
        </span>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => { refresh(); postChannelEvent('missions.refresh') }}
          aria-label="Refresh missions"
          disabled={loading}
          className="h-auto py-0.5 px-1.5 text-base"
          style={{ color: 'var(--text-secondary)' }}
        >
          ↻
        </Button>
      </div>

      {/* Filter bar */}
      <div
        className="flex flex-col gap-3 p-3"
        style={{ borderBottom: '1px solid var(--pill-border)' }}
      >
        {/* Status filter buttons */}
        <div className="flex gap-2 flex-wrap">
          {(['all', 'running', 'completed', 'failed'] as const).map(status => (
            <Button
              key={status}
              variant={statusFilter === status ? 'default' : 'outline'}
              size="sm"
              onClick={() => setStatusFilter(status)}
              className="h-auto py-1 px-2.5 text-xs capitalize"
            >
              {status}
            </Button>
          ))}
        </div>

        {/* Search input */}
        <input
          type="text"
          placeholder="Search missions…"
          value={searchQuery}
          onChange={e => setSearchQuery(e.target.value)}
          className="w-full px-2.5 py-1.5 text-sm rounded border"
          style={{
            borderColor: 'var(--pill-border)',
            backgroundColor: 'var(--mic-bg)',
            color: 'var(--text-primary)',
          }}
          aria-label="Search missions by title"
        />
      </div>

      {error && <div className="panel-empty panel-empty--error">{error}</div>}

      {sorted.length === 0 ? (
        <div className="missions-panel-empty">
          {loading
            ? 'Loading…'
            : missions.length === 0
              ? isConnected
                ? 'No missions found.'
                : 'Daemon offline — start with: orchestrator daemon start'
              : 'No missions match your filter.'}
        </div>
      ) : (
        <ul className="missions-list" role="list">
          {sorted.map(m => (
            <MissionRow
              key={m.mission_id}
              mission={m}
              isExpanded={expandedId === m.mission_id}
              dag={dags[m.mission_id] ?? null}
              dagLoading={dagLoading === m.mission_id}
              cancelling={cancelling === m.mission_id}
              onToggle={() => handleToggle(m.mission_id)}
              onCancel={e => handleCancel(e, m.mission_id)}
              getMissionEvents={getMissionEvents}
            />
          ))}
        </ul>
      )}
    </div>
  )
}
