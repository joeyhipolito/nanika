import { useState, useMemo, useCallback } from 'react'
import { motion, AnimatePresence } from 'motion/react'
import { useFindings } from '../hooks/useFindings'
import { useNenHealth } from '../hooks/useNenHealth'
import { useScanners } from '../hooks/useScanners'
import { useRyuReport } from '../hooks/useRyuReport'
import { useKoResults } from '../hooks/useKoResults'
import type { Finding, FindingSeverity } from '../types'
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

// ─────────────────────────────────────────────────────────────────────────────
// Finding card (reused from NenFindingsPanel)
// ─────────────────────────────────────────────────────────────────────────────

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

// ─────────────────────────────────────────────────────────────────────────────
// Category section (collapsible, reused)
// ─────────────────────────────────────────────────────────────────────────────

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

// ─────────────────────────────────────────────────────────────────────────────
// Tab content: Overview
// ─────────────────────────────────────────────────────────────────────────────

function OverviewTab() {
  const { findings } = useFindings({ limit: 1000 })
  const nenHealth = useNenHealth()
  const { scanners } = useScanners()

  const severityCount = useMemo(() => {
    const counts = { critical: 0, high: 0, medium: 0, low: 0, info: 0 }
    findings.forEach(f => { counts[f.severity]++ })
    return counts
  }, [findings])

  const healthyScanners = scanners.filter(s => {
    // For now, assume all scanners are healthy if loaded
    return s.name.length > 0
  }).length

  const healthyAbilities = nenHealth.abilities.filter(a => a.status === 'healthy').length

  return (
    <div className="flex flex-col gap-4 p-4 flex-1 overflow-y-auto">
      {/* Findings count summary */}
      <div className="grid grid-cols-2 gap-2">
        <div
          className="rounded-lg border p-3"
          style={{ borderColor: neutral.pillBorder, background: neutral.pillBg }}
        >
          <div style={{ fontSize: 11, color: neutral.textSecondary, marginBottom: 4 }}>
            Total findings
          </div>
          <div style={{ fontSize: 16, fontWeight: 500, color: neutral.textPrimary }}>
            {findings.length}
          </div>
        </div>

        <div
          className="rounded-lg border p-3"
          style={{ borderColor: neutral.pillBorder, background: neutral.pillBg }}
        >
          <div style={{ fontSize: 11, color: neutral.textSecondary, marginBottom: 4 }}>
            Critical/High
          </div>
          <div style={{ fontSize: 16, fontWeight: 500, color: status.error }}>
            {severityCount.critical + severityCount.high}
          </div>
        </div>
      </div>

      {/* Severity breakdown */}
      <div
        className="rounded-lg border p-3"
        style={{ borderColor: neutral.pillBorder, background: neutral.pillBg }}
      >
        <div style={{ fontSize: 11, color: neutral.textSecondary, marginBottom: 3 }}>
          By severity
        </div>
        <div className="flex flex-col gap-1.5">
          {SEVERITY_ORDER.map(sev => (
            <div key={sev} className="flex items-center justify-between">
              <span
                className="capitalize"
                style={{ fontSize: 11, color: severityColor(sev) }}
              >
                {sev}
              </span>
              <span style={{ fontSize: 12, fontWeight: 500, color: neutral.textPrimary }}>
                {severityCount[sev]}
              </span>
            </div>
          ))}
        </div>
      </div>

      {/* Scanner health */}
      <div
        className="rounded-lg border p-3"
        style={{ borderColor: neutral.pillBorder, background: neutral.pillBg }}
      >
        <div style={{ fontSize: 11, color: neutral.textSecondary, marginBottom: 4 }}>
          Scanners
        </div>
        <div style={{ fontSize: 16, fontWeight: 500, color: neutral.textPrimary }}>
          {scanners.length} active
        </div>
        <div style={{ fontSize: 10, color: neutral.textSecondary, marginTop: 2 }}>
          {healthyScanners} healthy
        </div>
      </div>

      {/* Abilities health */}
      <div
        className="rounded-lg border p-3"
        style={{ borderColor: neutral.pillBorder, background: neutral.pillBg }}
      >
        <div style={{ fontSize: 11, color: neutral.textSecondary, marginBottom: 4 }}>
          Abilities
        </div>
        <div style={{ fontSize: 16, fontWeight: 500, color: neutral.textPrimary }}>
          {nenHealth.abilities.length} total
        </div>
        <div style={{ fontSize: 10, color: neutral.textSecondary, marginTop: 2 }}>
          {healthyAbilities} healthy
        </div>
      </div>
    </div>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab content: Findings (with severity filter)
// ─────────────────────────────────────────────────────────────────────────────

const SEVERITY_FILTERS: Array<FindingSeverity | 'all'> = ['all', 'critical', 'high', 'medium', 'low', 'info']

function FindingsTab() {
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

  const { findings: rawFindings, loading, error, refresh } = useFindings({
    severity: severityFilter,
    limit: 100,
  })

  const findings = useMemo(
    () => rawFindings.filter(f => !dismissedIds.has(f.id) && !resolvedIds.has(f.id)),
    [rawFindings, dismissedIds, resolvedIds]
  )

  const grouped = useMemo(() => groupByCategory(findings), [findings])
  const categories = useMemo(
    () => Object.keys(grouped).sort((a, b) => grouped[b].length - grouped[a].length),
    [grouped]
  )

  return (
    <div className="flex flex-col flex-1 overflow-hidden">
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
          onClick={refresh}
          aria-label="Refresh findings"
          disabled={loading}
          className="h-auto py-0.5 px-1.5 text-sm"
          style={{ color: 'var(--text-secondary)' }}
        >
          ↻
        </Button>
      </div>

      {loading && <div className="panel-empty">Loading findings…</div>}
      {error && <div className="panel-empty panel-empty--error">{error}</div>}

      {!loading && findings.length === 0 && (
        <div className="nen-findings-panel--empty">
          <div className="nen-findings-empty-icon">◎</div>
          <p className="nen-findings-empty-text">No findings.</p>
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
    </div>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab content: Scanners
// ─────────────────────────────────────────────────────────────────────────────

function ScannersTab() {
  const { scanners, loading, error, refresh } = useScanners()
  const nenHealth = useNenHealth()
  const { findings } = useFindings({ limit: 1000 })

  const findingsByScannerName: Record<string, number> = useMemo(() => {
    const counts: Record<string, number> = {}
    findings.forEach(f => {
      const scanner = f.ability || f.source || 'unknown'
      counts[scanner] = (counts[scanner] || 0) + 1
    })
    return counts
  }, [findings])

  return (
    <div className="flex flex-col gap-3 p-4 flex-1 overflow-y-auto">
      <div className="flex items-center justify-between">
        <span style={{ fontSize: 12, color: neutral.textSecondary }}>
          {scanners.length} scanner{scanners.length !== 1 ? 's' : ''}
        </span>
        <Button
          variant="ghost"
          size="sm"
          onClick={refresh}
          disabled={loading}
          className="h-auto py-0.5 px-1.5 text-sm"
          style={{ color: neutral.textSecondary }}
        >
          ↻
        </Button>
      </div>

      {loading && <div style={{ fontSize: 12, color: neutral.textSecondary }}>Loading…</div>}
      {error && <div style={{ fontSize: 12, color: status.error }}>Error: {error}</div>}

      {!loading && scanners.length === 0 && (
        <div style={{ fontSize: 12, color: neutral.textSecondary, textAlign: 'center', padding: '20px 0' }}>
          No scanners found
        </div>
      )}

      {scanners.map(scanner => {
        const findingsCount = findingsByScannerName[scanner.name] || 0
        const healthStatus = nenHealth.abilities.find(a => a.name === scanner.name)
        const isHealthy = healthStatus?.status === 'healthy'

        return (
          <div
            key={scanner.name}
            className="rounded-lg border p-3 flex items-center justify-between"
            style={{ borderColor: neutral.pillBorder, background: neutral.pillBg }}
          >
            <div className="flex-1 min-w-0">
              <div
                style={{
                  fontSize: 12,
                  fontWeight: 500,
                  color: neutral.textPrimary,
                  marginBottom: 2,
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace: 'nowrap',
                }}
              >
                {scanner.name}
              </div>
              <div style={{ fontSize: 10, color: neutral.textSecondary }}>
                {findingsCount} finding{findingsCount !== 1 ? 's' : ''}
              </div>
            </div>
            <div
              className="flex-shrink-0 w-2 h-2 rounded-full"
              style={{ background: isHealthy ? status.success : status.warning }}
              title={healthStatus?.message}
            />
          </div>
        )
      })}
    </div>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab content: Abilities
// ─────────────────────────────────────────────────────────────────────────────

function AbilitiesTab() {
  const nenHealth = useNenHealth()

  function getHealthColor(s: string): string {
    if (s === 'healthy') return 'oklch(0.71 0.1 142)'
    if (s === 'degraded') return status.warning
    return status.error
  }

  return (
    <div className="flex flex-col gap-3 p-4 flex-1 overflow-y-auto">
      <div style={{ fontSize: 12, color: neutral.textSecondary }}>
        {nenHealth.abilities.length} ability{nenHealth.abilities.length !== 1 ? 'ies' : ''}
      </div>

      {nenHealth.loading && <div style={{ fontSize: 12, color: neutral.textSecondary }}>Loading…</div>}
      {nenHealth.error && <div style={{ fontSize: 12, color: status.error }}>Error: {nenHealth.error}</div>}

      {!nenHealth.loading && nenHealth.abilities.length === 0 && (
        <div style={{ fontSize: 12, color: neutral.textSecondary, textAlign: 'center', padding: '20px 0' }}>
          No abilities data
        </div>
      )}

      {nenHealth.abilities.map(ability => {
        const healthColor = getHealthColor(ability.status)

        return (
          <div
            key={ability.name}
            className="rounded-lg border p-3"
            style={{ borderColor: neutral.pillBorder, background: neutral.pillBg }}
          >
            <div className="flex items-center justify-between mb-2">
              <div
                style={{
                  fontSize: 12,
                  fontWeight: 500,
                  color: neutral.textPrimary,
                }}
              >
                {ability.name}
              </div>
              <Badge
                variant="outline"
                className="capitalize"
                style={{ fontSize: 10, color: healthColor, borderColor: healthColor }}
              >
                {ability.status}
              </Badge>
            </div>
            <div
              style={{
                fontSize: 10,
                color: neutral.textSecondary,
                overflow: 'hidden',
                textOverflow: 'ellipsis',
                whiteSpace: 'nowrap',
              }}
              title={ability.message}
            >
              {ability.message}
            </div>
          </div>
        )
      })}
    </div>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab content: Cost
// ─────────────────────────────────────────────────────────────────────────────

function CostTab() {
  const { data: report, loading, error, refresh } = useRyuReport()

  function formatCurrency(cents: number): string {
    return `$${(cents / 100).toFixed(2)}`
  }

  return (
    <div className="flex flex-col gap-4 p-4 flex-1 overflow-y-auto">
      <div className="flex items-center justify-between">
        <span style={{ fontSize: 11, color: neutral.textSecondary }}>Cost Report</span>
        <Button
          variant="ghost"
          size="sm"
          onClick={refresh}
          disabled={loading}
          className="h-auto py-0.5 px-1.5 text-sm"
          style={{ color: neutral.textSecondary }}
        >
          ↻
        </Button>
      </div>

      {loading && <div style={{ fontSize: 12, color: neutral.textSecondary }}>Loading…</div>}
      {error && <div style={{ fontSize: 12, color: status.error }}>Error: {error}</div>}

      {!loading && !error && report && (
        <>
          {/* Spend summary */}
          <div className="grid grid-cols-2 gap-2">
            <div
              className="rounded-lg border p-3"
              style={{ borderColor: neutral.pillBorder, background: neutral.pillBg }}
            >
              <div style={{ fontSize: 11, color: neutral.textSecondary, marginBottom: 4 }}>
                Today
              </div>
              <div style={{ fontSize: 16, fontWeight: 500, color: neutral.textPrimary }}>
                {formatCurrency(Math.round(report.today_spend * 100))}
              </div>
            </div>

            <div
              className="rounded-lg border p-3"
              style={{ borderColor: neutral.pillBorder, background: neutral.pillBg }}
            >
              <div style={{ fontSize: 11, color: neutral.textSecondary, marginBottom: 4 }}>
                This week
              </div>
              <div style={{ fontSize: 16, fontWeight: 500, color: neutral.textPrimary }}>
                {formatCurrency(Math.round(report.week_spend * 100))}
              </div>
            </div>
          </div>

          {/* Top missions */}
          {report.top_missions.length > 0 && (
            <div>
              <div style={{ fontSize: 11, color: neutral.textSecondary, marginBottom: 2 }}>
                Top missions
              </div>
              <div className="flex flex-col gap-1.5">
                {report.top_missions.map(m => (
                  <div
                    key={m.id}
                    className="rounded-lg border p-2.5"
                    style={{ borderColor: neutral.pillBorder, background: neutral.pillBg }}
                  >
                    <div className="flex items-center justify-between gap-2 mb-1">
                      <span
                        style={{
                          fontSize: 11,
                          fontWeight: 500,
                          color: neutral.textPrimary,
                          flex: 1,
                          overflow: 'hidden',
                          textOverflow: 'ellipsis',
                          whiteSpace: 'nowrap',
                        }}
                        title={m.task}
                      >
                        {m.task}
                      </span>
                      <span
                        style={{
                          fontSize: 11,
                          fontFamily: 'monospace',
                          color: neutral.textPrimary,
                          fontWeight: 500,
                          flexShrink: 0,
                        }}
                      >
                        {formatCurrency(Math.round(m.cost * 100))}
                      </span>
                    </div>
                    <div className="flex items-center justify-between">
                      <span
                        style={{
                          fontSize: 9,
                          color: neutral.textSecondary,
                          fontFamily: 'monospace',
                        }}
                      >
                        {m.id.slice(0, 12)}…
                      </span>
                      <Badge variant="secondary" style={{ fontSize: 8 }} className="capitalize">
                        {m.status}
                      </Badge>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}
        </>
      )}

      {!loading && !error && (!report || report.top_missions.length === 0) && (
        <div style={{ fontSize: 12, color: neutral.textSecondary, textAlign: 'center', padding: '20px 0' }}>
          No cost data available
        </div>
      )}
    </div>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab content: Evals
// ─────────────────────────────────────────────────────────────────────────────

function EvalsTab() {
  const { data: results, loading, error, refresh } = useKoResults()

  return (
    <div className="flex flex-col gap-3 p-4 flex-1 overflow-y-auto">
      <div className="flex items-center justify-between">
        <span style={{ fontSize: 12, color: neutral.textSecondary }}>
          {results?.suites.length || 0} suite{results?.suites.length !== 1 ? 's' : ''}
        </span>
        <Button
          variant="ghost"
          size="sm"
          onClick={refresh}
          disabled={loading}
          className="h-auto py-0.5 px-1.5 text-sm"
          style={{ color: neutral.textSecondary }}
        >
          ↻
        </Button>
      </div>

      {loading && <div style={{ fontSize: 12, color: neutral.textSecondary }}>Loading…</div>}
      {error && <div style={{ fontSize: 12, color: status.error }}>Error: {error}</div>}

      {!loading && !error && (!results || results.suites.length === 0) && (
        <div style={{ fontSize: 12, color: neutral.textSecondary, textAlign: 'center', padding: '20px 0' }}>
          No eval suites found
        </div>
      )}

      {!loading && results && results.suites.map(suite => {
        const passRatePercent = Math.round(suite.pass_rate * 100)
        const passRateColor =
          passRatePercent >= 90 ? 'oklch(0.71 0.1 142)' :
          passRatePercent >= 70 ? status.warning :
          status.error

        return (
          <div
            key={suite.name}
            className="rounded-lg border p-3"
            style={{ borderColor: neutral.pillBorder, background: neutral.pillBg }}
          >
            <div className="flex items-center justify-between mb-2">
              <span
                style={{
                  fontSize: 12,
                  fontWeight: 500,
                  color: neutral.textPrimary,
                  flex: 1,
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace: 'nowrap',
                }}
                title={suite.name}
              >
                {suite.name}
              </span>
              <span
                style={{
                  fontSize: 12,
                  fontWeight: 500,
                  fontFamily: 'monospace',
                  color: passRateColor,
                  flexShrink: 0,
                  marginLeft: 8,
                }}
              >
                {passRatePercent}%
              </span>
            </div>

            {/* Progress bar */}
            <div
              className="h-1.5 rounded-full mb-2"
              style={{ background: neutral.pillBorder, overflow: 'hidden' }}
            >
              <div
                className="h-full transition-all"
                style={{ width: `${passRatePercent}%`, background: passRateColor }}
              />
            </div>

            <div className="flex items-center justify-between gap-2 text-[10px] text-[var(--text-secondary)]">
              <span>
                {suite.passed}/{suite.total} passed
              </span>
              <span>
                {formatRelativeTime(suite.last_run_at)}
              </span>
            </div>
          </div>
        )
      })}
    </div>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// Main NenPanel component with tabs
// ─────────────────────────────────────────────────────────────────────────────

type Tab = 'overview' | 'findings' | 'scanners' | 'abilities' | 'cost' | 'evals'

export function NenPanel() {
  const [tab, setTab] = useState<Tab>('overview')

  const tabLabels: Record<Tab, string> = {
    overview: 'Overview',
    findings: 'Findings',
    scanners: 'Scanners',
    abilities: 'Abilities',
    cost: 'Cost',
    evals: 'Evals',
  }

  return (
    <div className="nen-findings-panel flex flex-col h-full">
      {/* Tab bar */}
      <div className="findings-tab-bar flex-shrink-0">
        {(Object.keys(tabLabels) as Tab[]).map(t => (
          <button
            key={t}
            type="button"
            className={`findings-tab${tab === t ? ' findings-tab--active' : ''}`}
            onClick={() => setTab(t)}
          >
            {tabLabels[t]}
          </button>
        ))}
      </div>

      {/* Tab content */}
      <div className="flex-1 overflow-hidden">
        {tab === 'overview' && <OverviewTab />}
        {tab === 'findings' && <FindingsTab />}
        {tab === 'scanners' && <ScannersTab />}
        {tab === 'abilities' && <AbilitiesTab />}
        {tab === 'cost' && <CostTab />}
        {tab === 'evals' && <EvalsTab />}
      </div>
    </div>
  )
}
