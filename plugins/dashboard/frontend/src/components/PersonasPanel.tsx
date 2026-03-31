import { useState, useCallback, useEffect } from 'react'
import { motion, AnimatePresence } from 'motion/react'
import { usePersonas } from '../hooks/usePersonas'
import type { PersonaResponse, PersonaDetailResponse } from '../types'
import { getPersonaDetail } from '../lib/wails'
import { status } from '../colors'
import { Badge } from './ui/badge'
import { Button } from './ui/button'
import { Card } from './ui/card'

function formatDuration(s: number): string {
  if (s < 60) return `${s.toFixed(0)}s`
  if (s < 3_600) return `${(s / 60).toFixed(1)}m`
  return `${(s / 3_600).toFixed(1)}h`
}

function successColor(rate: number): string {
  if (rate >= 0.8) return status.success
  if (rate >= 0.5) return status.warning
  return status.error
}

function statusLabel(s: string): string {
  if (s === 'success') return 'completed'
  return s
}

// ── Expertise tag pill ────────────────────────────────────────────────────────

function ExpertiseTag({ label }: { label: string }) {
  return (
    <span
      className="inline-block text-[9px] font-medium px-1.5 py-0.5 rounded-sm tracking-wide"
      style={{
        background: 'var(--pill-bg)',
        color: 'var(--text-secondary)',
        border: '1px solid var(--pill-border)',
      }}
    >
      {label}
    </span>
  )
}

// ── Persona card ──────────────────────────────────────────────────────────────

interface PersonaCardProps {
  persona: PersonaResponse
  index: number
  onClick: (name: string) => void
}

function PersonaCard({ persona, index, onClick }: PersonaCardProps) {
  const tags = persona.expertise?.slice(0, 3) ?? []

  return (
    <motion.div
      initial={{ opacity: 0, y: 6 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.18, delay: index * 0.03 }}
    >
      <Card
        className="relative flex flex-col gap-1.5 p-3 h-full transition-colors cursor-pointer hover:border-[var(--accent)]"
        style={
          persona.currently_active
            ? {
                borderColor: 'var(--accent)',
                background: 'color-mix(in srgb, var(--mic-bg) 85%, var(--accent) 15%)',
              }
            : { background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }
        }
        onClick={() => onClick(persona.name)}
        role="button"
        tabIndex={0}
        aria-label={`View details for ${persona.name}`}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault()
            onClick(persona.name)
          }
        }}
      >
        {persona.currently_active && (
          <Badge
            variant="outline"
            className="absolute top-2 right-2 text-[9px] uppercase tracking-wider"
            aria-label="Currently active"
            style={{ color: 'var(--accent)', background: 'var(--accent-soft)', borderColor: 'var(--accent)' }}
          >
            active
          </Badge>
        )}
        <h3 className="persona-name">{persona.name}</h3>
        {persona.when_to_use[0] && (
          <p className="persona-use">{persona.when_to_use[0]}</p>
        )}
        {tags.length > 0 && (
          <div className="flex flex-wrap gap-1 mt-0.5">
            {tags.map((tag) => (
              <ExpertiseTag key={tag} label={tag} />
            ))}
          </div>
        )}
        <div className="persona-stats mt-auto pt-1">
          <div className="persona-stat">
            <span className="persona-stat-value">{persona.missions_assigned}</span>
            <span className="persona-stat-label">missions</span>
          </div>
          <div className="persona-stat">
            <span
              className="persona-stat-value"
              style={{ color: successColor(persona.success_rate) }}
            >
              {(persona.success_rate * 100).toFixed(0)}%
            </span>
            <span className="persona-stat-label">success</span>
          </div>
          <div className="persona-stat">
            <span className="persona-stat-value">{formatDuration(persona.avg_duration_seconds)}</span>
            <span className="persona-stat-label">avg</span>
          </div>
        </div>
      </Card>
    </motion.div>
  )
}

// ── Trend sparkline ───────────────────────────────────────────────────────────

interface TrendSparklineProps {
  trend: PersonaDetailResponse['success_trend']
}

function TrendSparkline({ trend }: TrendSparklineProps) {
  if (trend.length === 0) {
    return <span className="text-xs" style={{ color: 'var(--text-secondary)' }}>no trend data</span>
  }

  const max = Math.max(...trend.map((p) => p.total), 1)
  const h = 32
  const w = 8
  const gap = 2

  return (
    <svg
      width={trend.length * (w + gap) - gap}
      height={h}
      aria-label="Weekly success rate trend"
      role="img"
    >
      {trend.map((pt, i) => {
        const barH = Math.max(2, (pt.total / max) * h)
        const successH = Math.max(0, (pt.succeeded / max) * h)
        const x = i * (w + gap)
        return (
          <g key={pt.week}>
            <title>{`${pt.week}: ${pt.succeeded}/${pt.total} (${(pt.success_rate * 100).toFixed(0)}%)`}</title>
            {/* total bar (background) */}
            <rect x={x} y={h - barH} width={w} height={barH} rx={1} fill="var(--pill-border)" />
            {/* success portion */}
            <rect
              x={x}
              y={h - successH}
              width={w}
              height={successH}
              rx={1}
              fill={successColor(pt.success_rate)}
            />
          </g>
        )
      })}
    </svg>
  )
}

// ── Persona detail view ───────────────────────────────────────────────────────

interface PersonaDetailProps {
  name: string
  onBack: () => void
}

function PersonaDetail({ name, onBack }: PersonaDetailProps) {
  const [detail, setDetail] = useState<PersonaDetailResponse | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const data = await getPersonaDetail(name)
      setDetail(data)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load persona')
    } finally {
      setLoading(false)
    }
  }, [name])

  // Load on mount
  useEffect(() => { void load() }, [load])

  if (loading) {
    return (
      <div className="personas-detail">
        <button className="personas-back" onClick={onBack} aria-label="Back to personas list">← back</button>
        <div className="panel-empty">Loading {name}…</div>
      </div>
    )
  }

  if (error || !detail) {
    return (
      <div className="personas-detail">
        <button className="personas-back" onClick={onBack} aria-label="Back to personas list">← back</button>
        <div className="panel-empty panel-empty--error">{error ?? 'Not found'}</div>
      </div>
    )
  }

  return (
    <motion.div
      className="personas-detail"
      initial={{ opacity: 0, x: 12 }}
      animate={{ opacity: 1, x: 0 }}
      transition={{ duration: 0.18 }}
    >
      {/* Header */}
      <div className="personas-detail-header">
        <button className="personas-back" onClick={onBack} aria-label="Back to personas list">
          ← back
        </button>
        <div className="flex items-center gap-2">
          <h2
            className="text-sm font-semibold"
            style={{ color: detail.color || 'var(--text-primary)' }}
          >
            {detail.name}
          </h2>
          {detail.currently_active && (
            <Badge
              variant="outline"
              className="text-[9px] uppercase tracking-wider"
              style={{ color: 'var(--accent)', background: 'var(--accent-soft)', borderColor: 'var(--accent)' }}
            >
              active
            </Badge>
          )}
        </div>
      </div>

      {/* Stats row */}
      <div className="persona-stats mt-2">
        <div className="persona-stat">
          <span className="persona-stat-value">{detail.missions_assigned}</span>
          <span className="persona-stat-label">missions</span>
        </div>
        <div className="persona-stat">
          <span className="persona-stat-value" style={{ color: successColor(detail.success_rate) }}>
            {(detail.success_rate * 100).toFixed(0)}%
          </span>
          <span className="persona-stat-label">success</span>
        </div>
        <div className="persona-stat">
          <span className="persona-stat-value">{formatDuration(detail.avg_duration_seconds)}</span>
          <span className="persona-stat-label">avg duration</span>
        </div>
      </div>

      {/* Expertise tags */}
      {detail.expertise.length > 0 && (
        <section className="mt-3">
          <p className="personas-section-label">expertise</p>
          <div className="flex flex-wrap gap-1.5 mt-1">
            {detail.expertise.map((tag) => (
              <ExpertiseTag key={tag} label={tag} />
            ))}
          </div>
        </section>
      )}

      {/* Success trend */}
      <section className="mt-3">
        <p className="personas-section-label">weekly success trend</p>
        <div className="mt-2">
          <TrendSparkline trend={detail.success_trend} />
        </div>
      </section>

      {/* Recent missions */}
      <section className="mt-3 flex-1 min-h-0">
        <p className="personas-section-label">recent missions</p>
        {detail.recent_missions.length === 0 ? (
          <p className="text-xs mt-2" style={{ color: 'var(--text-secondary)' }}>No recorded missions.</p>
        ) : (
          <ul className="mt-1 space-y-1 overflow-y-auto max-h-48">
            {detail.recent_missions.map((m) => (
              <li
                key={m.workspace_id}
                className="flex items-start gap-2 py-1.5 border-t"
                style={{ borderColor: 'var(--pill-border)' }}
              >
                <span
                  className="text-[10px] font-medium mt-0.5 shrink-0"
                  style={{ color: m.status === 'success' ? status.success : status.error }}
                >
                  {statusLabel(m.status)}
                </span>
                <span className="text-[11px] leading-snug flex-1 min-w-0 truncate" style={{ color: 'var(--text-primary)' }}>
                  {m.task || m.workspace_id}
                </span>
                <span className="text-[10px] shrink-0" style={{ color: 'var(--text-secondary)' }}>
                  {formatDuration(m.duration_s)}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>
    </motion.div>
  )
}

// ── Main panel ────────────────────────────────────────────────────────────────

export function PersonasPanel() {
  const { personas, loading, error, reloading, refresh, reload } = usePersonas()
  const [selectedPersona, setSelectedPersona] = useState<string | null>(null)

  const handleCardClick = useCallback((name: string) => {
    setSelectedPersona(name)
  }, [])

  const handleBack = useCallback(() => {
    setSelectedPersona(null)
  }, [])

  return (
    <div className="personas-panel">
      <AnimatePresence mode="wait">
        {selectedPersona ? (
          <motion.div
            key="detail"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.12 }}
          >
            <PersonaDetail name={selectedPersona} onBack={handleBack} />
          </motion.div>
        ) : (
          <motion.div
            key="list"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.12 }}
          >
            <div className="personas-toolbar">
              <span className="personas-count">
                {personas.length} persona{personas.length !== 1 ? 's' : ''}
              </span>
              <Button
                variant="ghost"
                size="sm"
                onClick={refresh}
                aria-label="Refresh personas"
                disabled={loading}
                className="h-auto py-0.5 px-1.5 text-sm"
                style={{ color: 'var(--text-secondary)' }}
              >
                ↻
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={reload}
                disabled={reloading || loading}
                className="h-auto py-1 px-2.5 text-[11px]"
                style={
                  reloading
                    ? { color: 'var(--accent)', borderColor: 'var(--accent)', fontStyle: 'italic' }
                    : { color: 'var(--text-secondary)', borderColor: 'var(--pill-border)' }
                }
              >
                {reloading ? 'reloading…' : 'reload from disk'}
              </Button>
            </div>

            {error && <div className="panel-empty panel-empty--error">{error}</div>}

            {loading && !personas.length && (
              <div className="panel-empty">Loading personas…</div>
            )}

            {!loading && !error && personas.length === 0 && (
              <div className="panel-empty">No personas found.</div>
            )}

            {personas.length > 0 && (
              <div className="personas-grid">
                {personas.map((p, i) => (
                  <PersonaCard key={p.name} persona={p} index={i} onClick={handleCardClick} />
                ))}
              </div>
            )}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  )
}
