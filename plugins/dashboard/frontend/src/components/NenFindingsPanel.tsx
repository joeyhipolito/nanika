import { useState, useMemo, useCallback } from 'react'
import { motion, AnimatePresence } from 'motion/react'
import { useFindings } from '../hooks/useFindings'
import { useDecompositionFindings } from '../hooks/useDecompositionFindings'
import type { Finding, FindingSeverity, DecompositionFinding } from '../types'
import { neutral, status } from '../colors'
import { Badge } from './ui/badge'
import { Button } from './ui/button'

const SEVERITY_ORDER: FindingSeverity[] = ['critical', 'high', 'medium', 'low', 'info']
const SEVERITY_LS_KEY = 'nen-severity-filter'

function severityColor(s: FindingSeverity): string {
  if (s === 'critical') return status.error
  if (s === 'high') return 'oklch(0.68 0.18 40)'
  if (s === 'medium') return status.warning
  if (s === 'low') return status.info
  return neutral.textSecondary
}

function formatRelativeTime(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime()
  if (diff < 60_000) return 'just now'
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`
  if (diff < 604_800_000) return `${Math.floor(diff / 86_400_000)}d ago`
  return new Date(iso).toLocaleDateString([], { month: 'short', day: 'numeric' })
}

function readStoredSeverity(): FindingSeverity | undefined {
  try {
    const v = localStorage.getItem(SEVERITY_LS_KEY)
    if (v && SEVERITY_ORDER.includes(v as FindingSeverity)) return v as FindingSeverity
  } catch { /* ignore */ }
  return undefined
}

function groupByCategory(findings: Finding[]): Record<string, Finding[]> {
  return findings.reduce<Record<string, Finding[]>>((acc, f) => {
    const key = f.category || f.ability || 'unknown'
    if (!acc[key]) acc[key] = []
    acc[key].push(f)
    return acc
  }, {})
}

function decompositionScoreColor(score: number): string {
  if (score >= 0.8) return status.success
  if (score >= 0.5) return status.warning
  return status.error
}

// ---------------------------------------------------------------------------
// Nen finding card
// ---------------------------------------------------------------------------

interface FindingCardProps {
  finding: Finding
  onDismiss: (id: string) => void
  onResolve: (id: string) => void
}

function FindingCard({ finding: f, onDismiss, onResolve }: FindingCardProps) {
  const [open, setOpen] = useState(false)
  const color = severityColor(f.severity)
  const timestamp = f.found_at || f.created_at

  return (
    <motion.li
      className={`nen-finding nen-finding--${f.severity}`}
      style={{ borderLeftColor: color }}
      layout
    >
      <div
        className="nen-finding-header"
        role="button"
        tabIndex={0}
        onClick={() => setOpen(o => !o)}
        onKeyDown={e => { if (e.key === 'Enter' || e.key === ' ') setOpen(o => !o) }}
        aria-expanded={open}
      >
        <Badge
          variant="outline"
          className="flex-shrink-0 uppercase tracking-wide"
          style={{ fontSize: 10, color, borderColor: color }}
        >
          {f.severity}
        </Badge>
        <span className="nen-finding-title-inline">{f.title}</span>
        <time
          className="nen-finding-time"
          dateTime={timestamp}
          title={new Date(timestamp).toLocaleString()}
        >
          {formatRelativeTime(timestamp)}
        </time>
        <span className="nen-finding-caret" aria-hidden="true">{open ? '▲' : '▼'}</span>
      </div>

      <AnimatePresence>
        {open && (
          <motion.div
            className="nen-finding-detail"
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: 'auto', opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.18 }}
            style={{ overflow: 'hidden' }}
          >
            <p className="nen-finding-desc">{f.description}</p>
            {f.scope.value && (
              <p className="nen-finding-scope">
                <span className="nen-finding-scope-kind">{f.scope.kind}</span>
                <span className="nen-finding-scope-value">{f.scope.value}</span>
              </p>
            )}
            {f.evidence.length > 0 && (
              <ul className="nen-finding-evidence">
                {f.evidence.slice(0, 3).map((ev, i) => (
                  <li key={i} className="nen-finding-evidence-item">
                    <span className="nen-finding-evidence-kind">{ev.kind}</span>
                    <span className="nen-finding-evidence-raw">{ev.raw.slice(0, 120)}</span>
                  </li>
                ))}
              </ul>
            )}
            <div className="nen-finding-actions">
              <button
                type="button"
                className="nen-finding-action-btn nen-finding-action-btn--resolve"
                onClick={e => { e.stopPropagation(); onResolve(f.id) }}
                aria-label={`Mark "${f.title}" as resolved`}
              >
                ✓ resolve
              </button>
              <button
                type="button"
                className="nen-finding-action-btn nen-finding-action-btn--dismiss"
                onClick={e => { e.stopPropagation(); onDismiss(f.id) }}
                aria-label={`Dismiss "${f.title}"`}
              >
                × dismiss
              </button>
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </motion.li>
  )
}

// ---------------------------------------------------------------------------
// Category section (collapsible)
// ---------------------------------------------------------------------------

interface CategorySectionProps {
  category: string
  findings: Finding[]
  onDismiss: (id: string) => void
  onResolve: (id: string) => void
}

function CategorySection({ category, findings, onDismiss, onResolve }: CategorySectionProps) {
  const [collapsed, setCollapsed] = useState(false)
  const sorted = useMemo(
    () => [...findings].sort((a, b) => SEVERITY_ORDER.indexOf(a.severity) - SEVERITY_ORDER.indexOf(b.severity)),
    [findings]
  )

  return (
    <section className="nen-findings-group">
      <button
        type="button"
        className="nen-findings-group-title nen-findings-group-toggle"
        onClick={() => setCollapsed(c => !c)}
        aria-expanded={!collapsed}
      >
        <span className="nen-findings-group-caret" aria-hidden="true">
          {collapsed ? '▶' : '▼'}
        </span>
        {category}
        <Badge variant="secondary" className="font-normal" style={{ fontSize: 10 }}>
          {findings.length}
        </Badge>
      </button>
      <AnimatePresence>
        {!collapsed && (
          <motion.ul
            className="nen-findings-list"
            role="list"
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: 'auto', opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.18 }}
            style={{ overflow: 'hidden' }}
          >
            {sorted.map(f => (
              <FindingCard key={f.id} finding={f} onDismiss={onDismiss} onResolve={onResolve} />
            ))}
          </motion.ul>
        )}
      </AnimatePresence>
    </section>
  )
}

// ---------------------------------------------------------------------------
// Decomposition finding row
// ---------------------------------------------------------------------------

function DecompFindingRow({ finding: f }: { finding: DecompositionFinding }) {
  const [open, setOpen] = useState(false)
  const scoreColor = decompositionScoreColor(f.audit_score)

  return (
    <li className="rounded-md overflow-hidden" style={{ background: 'var(--mic-bg)' }}>
      <div
        className="flex items-center gap-2 px-3 py-2 cursor-pointer select-none"
        role="button"
        tabIndex={0}
        onClick={() => setOpen(o => !o)}
        onKeyDown={e => { if (e.key === 'Enter' || e.key === ' ') setOpen(o => !o) }}
        aria-expanded={open}
      >
        <Badge variant="secondary" className="capitalize flex-shrink-0" style={{ fontSize: 10 }}>
          {f.finding_type}
        </Badge>
        <span
          className="flex-1 overflow-hidden text-ellipsis whitespace-nowrap"
          style={{ fontSize: 12, color: 'var(--text-primary)' }}
        >
          {f.phase_name}
        </span>
        <span
          className="flex-shrink-0 font-semibold"
          style={{ fontSize: 12, fontFamily: 'monospace', color: scoreColor }}
        >
          {(f.audit_score * 100).toFixed(0)}%
        </span>
        <span style={{ fontSize: 9, color: 'var(--text-secondary)', flexShrink: 0 }} aria-hidden="true">
          {open ? '▲' : '▼'}
        </span>
      </div>
      <AnimatePresence>
        {open && (
          <motion.div
            className="px-3 pb-3"
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: 'auto', opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.18 }}
            style={{ overflow: 'hidden' }}
          >
            <p style={{ fontSize: 12, color: 'var(--text-secondary)', lineHeight: 1.5, marginBottom: 4 }}>
              {f.detail}
            </p>
            <p style={{ fontSize: 10, color: 'var(--text-secondary)', opacity: 0.7 }}>
              source: {f.decomp_source} · workspace: {f.workspace_id.slice(-8)}
            </p>
          </motion.div>
        )}
      </AnimatePresence>
    </li>
  )
}

// ---------------------------------------------------------------------------
// Panel tabs
// ---------------------------------------------------------------------------

type Tab = 'nen' | 'decomp'

const SEVERITY_FILTERS: Array<FindingSeverity | 'all'> = ['all', 'critical', 'high', 'medium', 'low', 'info']

export function NenFindingsPanel() {
  const [tab, setTab] = useState<Tab>('nen')
  const [severityFilter, setSeverityFilterState] = useState<FindingSeverity | undefined>(readStoredSeverity)
  const [dismissedIds, setDismissedIds] = useState<ReadonlySet<string>>(new Set())
  const [resolvedIds, setResolvedIds] = useState<ReadonlySet<string>>(new Set())

  const setSeverityFilter = useCallback((val: FindingSeverity | undefined) => {
    setSeverityFilterState(val)
    try {
      if (val) localStorage.setItem(SEVERITY_LS_KEY, val)
      else localStorage.removeItem(SEVERITY_LS_KEY)
    } catch { /* ignore */ }
  }, [])

  const handleDismiss = useCallback((id: string) => {
    setDismissedIds(s => new Set([...s, id]))
  }, [])

  const handleResolve = useCallback((id: string) => {
    setResolvedIds(s => new Set([...s, id]))
  }, [])

  const { findings: rawFindings, loading: nenLoading, error: nenError, refresh: refreshNen } = useFindings({
    severity: severityFilter,
    limit: 100,
  })

  const findings = useMemo(
    () => rawFindings.filter(f => !dismissedIds.has(f.id) && !resolvedIds.has(f.id)),
    [rawFindings, dismissedIds, resolvedIds]
  )

  const { data: decompData, loading: decompLoading, error: decompError, refresh: refreshDecomp } = useDecompositionFindings()

  const grouped = useMemo(() => groupByCategory(findings), [findings])
  const categories = useMemo(
    () => Object.keys(grouped).sort((a, b) => grouped[b].length - grouped[a].length),
    [grouped]
  )

  const decompFindings: DecompositionFinding[] = decompData?.recent ?? []
  const decompCounts = decompData?.counts ?? []

  return (
    <div className="nen-findings-panel">
      {/* Tab bar */}
      <div className="findings-tab-bar">
        <button
          type="button"
          className={`findings-tab${tab === 'nen' ? ' findings-tab--active' : ''}`}
          onClick={() => setTab('nen')}
        >
          nen
          {findings.length > 0 && (
            <span className="findings-tab-count">{findings.length}</span>
          )}
        </button>
        <button
          type="button"
          className={`findings-tab${tab === 'decomp' ? ' findings-tab--active' : ''}`}
          onClick={() => setTab('decomp')}
        >
          decomposition
          {decompFindings.length > 0 && (
            <span className="findings-tab-count">{decompFindings.length}</span>
          )}
        </button>
      </div>

      {/* Nen findings tab */}
      {tab === 'nen' && (
        <>
          <div className="nen-findings-toolbar">
            <div className="nen-findings-filters">
              {SEVERITY_FILTERS.map(s => (
                <button
                  key={s}
                  type="button"
                  className={`nen-filter-btn${(s === 'all' ? !severityFilter : severityFilter === s) ? ' nen-filter-btn--active' : ''}`}
                  style={
                    s !== 'all' && severityFilter === s
                      ? { color: severityColor(s as FindingSeverity), borderColor: severityColor(s as FindingSeverity) }
                      : undefined
                  }
                  onClick={() => setSeverityFilter(s === 'all' ? undefined : (s as FindingSeverity))}
                >
                  {s}
                </button>
              ))}
            </div>
            <Button
              variant="ghost"
              size="sm"
              onClick={refreshNen}
              aria-label="Refresh findings"
              disabled={nenLoading}
              className="h-auto py-0.5 px-1.5 text-sm"
              style={{ color: 'var(--text-secondary)' }}
            >
              ↻
            </Button>
          </div>

          {nenLoading && <div className="panel-empty">Loading findings…</div>}
          {nenError && <div className="panel-empty panel-empty--error">{nenError}</div>}

          {!nenLoading && findings.length === 0 && (
            <div className="nen-findings-panel--empty">
              <div className="nen-findings-empty-icon">◎</div>
              <p className="nen-findings-empty-text">No findings.</p>
              <p className="nen-findings-empty-hint">
                Persistent findings are read from <code>findings.db</code>.
                Run a scanner or wait for SSE events to populate.
              </p>
            </div>
          )}

          {categories.length > 0 && (
            <div className="nen-findings-groups">
              {categories.map(category => (
                <CategorySection
                  key={category}
                  category={category}
                  findings={grouped[category]}
                  onDismiss={handleDismiss}
                  onResolve={handleResolve}
                />
              ))}
            </div>
          )}
        </>
      )}

      {/* Decomposition findings tab */}
      {tab === 'decomp' && (
        <>
          <div className="nen-findings-toolbar">
            <span className="flex-1" style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
              {decompFindings.length} recent finding{decompFindings.length !== 1 ? 's' : ''}
            </span>
            <Button
              variant="ghost"
              size="sm"
              onClick={refreshDecomp}
              aria-label="Refresh decomposition findings"
              disabled={decompLoading}
              className="h-auto py-0.5 px-1.5 text-sm"
              style={{ color: 'var(--text-secondary)' }}
            >
              ↻
            </Button>
          </div>

          {decompLoading && <div className="panel-empty">Loading decomposition findings…</div>}
          {decompError && <div className="panel-empty panel-empty--error">{decompError}</div>}

          {!decompLoading && decompFindings.length === 0 && (
            <div className="nen-findings-panel--empty">
              <div className="nen-findings-empty-icon">◎</div>
              <p className="nen-findings-empty-text">No decomposition findings.</p>
            </div>
          )}

          {decompCounts.length > 0 && (
            <div className="flex flex-wrap gap-1.5 mb-3 flex-shrink-0">
              {decompCounts.map(c => (
                <Badge key={c.finding_type} variant="outline" className="gap-1 capitalize" style={{ fontSize: 10 }}>
                  {c.finding_type}
                  <strong className="font-semibold">{c.count}</strong>
                </Badge>
              ))}
            </div>
          )}

          {decompFindings.length > 0 && (
            <ul className="flex flex-col gap-1.5 overflow-y-auto flex-1 min-h-0" role="list">
              {decompFindings.map(f => (
                <DecompFindingRow key={f.id} finding={f} />
              ))}
            </ul>
          )}
        </>
      )}
    </div>
  )
}
