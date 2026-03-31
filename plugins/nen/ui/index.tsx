import { useState, useEffect, useCallback, useRef } from 'react'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import { queryPluginItems, queryPluginStatus, nenScan, pluginAction } from '@/lib/wails'
import type { PluginViewProps } from '@/types'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface ComponentResult {
  name: string
  score: number
  trend: 'up' | 'down' | 'flat' | 'new'
  issues?: string[]
}

interface FindingSummary {
  id: string
  ability: string
  severity: 'critical' | 'high' | 'medium' | 'low' | string
  category: string
  title: string
  found_at: string
}

interface NenStatus {
  score: number
  critical_count: number
  evaluated_at: string
  daemon_running: boolean
  active_findings: number
}

interface NenItems {
  items: ComponentResult[]
  count: number
  findings: FindingSummary[]
}

// Ko score history point (accumulated across polls)
interface KoScorePoint {
  ts: number
  score: number
}

type SeverityFilter = 'all' | 'critical' | 'high' | 'medium' | 'low'

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const PLUGIN_NAME = 'nen'

const SEVERITY_ORDER: Record<string, number> = {
  critical: 0,
  high: 1,
  medium: 2,
  low: 3,
}

const SEVERITY_STYLES: Record<string, { bg: string; text: string; border: string }> = {
  critical: {
    bg: 'color-mix(in srgb, #ef4444 12%, transparent)',
    text: '#ef4444',
    border: 'color-mix(in srgb, #ef4444 30%, transparent)',
  },
  high: {
    bg: 'color-mix(in srgb, #f97316 12%, transparent)',
    text: '#f97316',
    border: 'color-mix(in srgb, #f97316 30%, transparent)',
  },
  medium: {
    bg: 'color-mix(in srgb, #eab308 12%, transparent)',
    text: '#eab308',
    border: 'color-mix(in srgb, #eab308 30%, transparent)',
  },
  low: {
    bg: 'color-mix(in srgb, #6b7280 12%, transparent)',
    text: '#6b7280',
    border: 'color-mix(in srgb, #6b7280 30%, transparent)',
  },
}

// All four scanners: gyo/en/ryu are external scanners, shu is the ko evaluator
const SCANNER_ABILITIES: Record<string, string> = {
  gyo: 'orchestrator-metrics',
  en: 'system-health',
  ryu: 'cost-analysis',
  shu: 'ko-eval',
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function scoreColor(score: number): string {
  if (score >= 80) return '#22c55e'
  if (score >= 60) return '#eab308'
  if (score >= 40) return '#f97316'
  return '#ef4444'
}

function scoreLabel(score: number): string {
  if (score >= 80) return 'Healthy'
  if (score >= 60) return 'Degraded'
  if (score >= 40) return 'Warning'
  return 'Critical'
}

function trendGlyph(trend: string): string {
  if (trend === 'up') return '↑'
  if (trend === 'down') return '↓'
  if (trend === 'new') return '★'
  return '—'
}

function trendColor(trend: string): string {
  if (trend === 'up') return '#22c55e'
  if (trend === 'down') return '#ef4444'
  return 'var(--text-secondary)'
}

function relativeTime(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime()
  const mins = Math.floor(diff / 60_000)
  if (mins < 1) return 'just now'
  if (mins < 60) return `${mins}m ago`
  const hrs = Math.floor(mins / 60)
  if (hrs < 24) return `${hrs}h ago`
  return `${Math.floor(hrs / 24)}d ago`
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function OverallScoreRing({ score }: { score: number }) {
  const radius = 28
  const stroke = 5
  const normalizedRadius = radius - stroke
  const circumference = 2 * Math.PI * normalizedRadius
  const offset = circumference - (score / 100) * circumference
  const color = scoreColor(score)

  return (
    <div className="flex flex-col items-center gap-1">
      <svg width={radius * 2} height={radius * 2}>
        <circle
          cx={radius}
          cy={radius}
          r={normalizedRadius}
          fill="none"
          stroke="var(--pill-border)"
          strokeWidth={stroke}
        />
        <circle
          cx={radius}
          cy={radius}
          r={normalizedRadius}
          fill="none"
          stroke={color}
          strokeWidth={stroke}
          strokeDasharray={circumference}
          strokeDashoffset={offset}
          strokeLinecap="round"
          transform={`rotate(-90 ${radius} ${radius})`}
          style={{ transition: 'stroke-dashoffset 0.5s ease' }}
        />
        <text
          x={radius}
          y={radius + 5}
          textAnchor="middle"
          fontSize="12"
          fontWeight="700"
          fill={color}
        >
          {score}
        </text>
      </svg>
      <span className="text-[10px] font-medium" style={{ color }}>
        {scoreLabel(score)}
      </span>
    </div>
  )
}

function ComponentCard({ result }: { result: ComponentResult }) {
  const color = scoreColor(result.score)

  return (
    <div
      className="flex flex-col gap-1.5 rounded-md p-2.5"
      style={{
        background: 'var(--mic-bg)',
        border: '1px solid var(--pill-border)',
      }}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs font-semibold capitalize" style={{ color: 'var(--text-primary)' }}>
          {result.name}
        </span>
        <div className="flex items-center gap-1.5">
          <span className="text-[10px] font-bold" style={{ color: trendColor(result.trend) }}>
            {trendGlyph(result.trend)}
          </span>
          <span
            className="rounded px-1.5 py-0.5 text-[10px] font-bold tabular-nums"
            style={{
              background: `color-mix(in srgb, ${color} 15%, transparent)`,
              color,
            }}
          >
            {result.score}
          </span>
        </div>
      </div>

      {/* Score bar */}
      <div
        className="h-1 w-full rounded-full"
        style={{ background: 'var(--pill-border)' }}
        role="progressbar"
        aria-valuenow={result.score}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-label={`${result.name} health: ${result.score}/100`}
      >
        <div
          className="h-full rounded-full"
          style={{
            width: `${result.score}%`,
            background: color,
            transition: 'width 0.4s ease',
          }}
        />
      </div>

      {/* Issues */}
      {result.issues && result.issues.length > 0 && (
        <ul className="mt-0.5 flex flex-col gap-0.5">
          {result.issues.map((issue, i) => (
            <li
              key={i}
              className="text-[10px] leading-snug"
              style={{ color: 'var(--text-secondary)' }}
            >
              · {issue}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

function FindingRow({
  finding,
  onDismiss,
  onResolve,
}: {
  finding: FindingSummary
  onDismiss: (id: string) => void
  onResolve: (id: string) => void
}) {
  const [acting, setActing] = useState<'dismiss' | 'resolve' | null>(null)
  const sev = finding.severity.toLowerCase()
  const style = SEVERITY_STYLES[sev] ?? SEVERITY_STYLES.low

  async function handleAction(verb: 'dismiss' | 'resolve') {
    setActing(verb)
    try {
      await pluginAction(PLUGIN_NAME, verb, finding.id)
    } catch {
      // Optimistically remove regardless — backend may be read-only
    }
    if (verb === 'dismiss') onDismiss(finding.id)
    else onResolve(finding.id)
  }

  return (
    <div
      className="flex items-start gap-2 rounded px-2 py-1.5 text-xs"
      style={{ background: style.bg, border: `1px solid ${style.border}` }}
    >
      <span
        className="mt-0.5 shrink-0 rounded px-1 py-0.5 text-[9px] font-bold uppercase"
        style={{ color: style.text }}
      >
        {finding.severity}
      </span>
      <div className="flex flex-1 flex-col gap-0.5 overflow-hidden">
        <span className="truncate font-medium" style={{ color: 'var(--text-primary)' }}>
          {finding.title}
        </span>
        <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
          {finding.ability} · {relativeTime(finding.found_at)}
        </span>
      </div>
      <div className="flex shrink-0 items-center gap-1">
        <Button
          size="sm"
          variant="ghost"
          className="h-5 px-1.5 text-[9px]"
          disabled={acting !== null}
          onClick={() => handleAction('resolve')}
          aria-label={`Resolve: ${finding.title}`}
        >
          {acting === 'resolve' ? '…' : '✓'}
        </Button>
        <Button
          size="sm"
          variant="ghost"
          className="h-5 px-1.5 text-[9px]"
          disabled={acting !== null}
          onClick={() => handleAction('dismiss')}
          aria-label={`Dismiss: ${finding.title}`}
        >
          {acting === 'dismiss' ? '…' : '✕'}
        </Button>
      </div>
    </div>
  )
}

function FindingsSection({
  findings,
  onDismiss,
  onResolve,
  filter,
}: {
  findings: FindingSummary[]
  onDismiss: (id: string) => void
  onResolve: (id: string) => void
  filter: SeverityFilter
}) {
  const visible = filter === 'all' ? findings : findings.filter(f => f.severity.toLowerCase() === filter)

  const sorted = [...visible].sort((a, b) => {
    const ao = SEVERITY_ORDER[a.severity.toLowerCase()] ?? 99
    const bo = SEVERITY_ORDER[b.severity.toLowerCase()] ?? 99
    return ao - bo
  })

  const grouped: Record<string, FindingSummary[]> = {}
  for (const f of sorted) {
    const sev = f.severity.toLowerCase()
    if (!grouped[sev]) grouped[sev] = []
    grouped[sev].push(f)
  }

  const severities = ['critical', 'high', 'medium', 'low'].filter(s => grouped[s]?.length)

  if (severities.length === 0) {
    return (
      <p className="py-3 text-center text-[11px]" style={{ color: 'var(--text-secondary)' }}>
        {filter === 'all' ? 'No active findings' : `No ${filter} findings`}
      </p>
    )
  }

  return (
    <div className="flex flex-col gap-2">
      {severities.map(sev => {
        const style = SEVERITY_STYLES[sev]
        // Critical and high start open; medium and low start collapsed
        const defaultOpen = sev === 'critical' || sev === 'high'
        return (
          <Collapsible key={sev} defaultOpen={defaultOpen}>
            <div
              className="overflow-hidden rounded-md"
              style={{ border: `1px solid ${style?.border ?? 'var(--pill-border)'}` }}
            >
              <CollapsibleTrigger asChild>
                <button
                  type="button"
                  className="flex w-full items-center gap-2 px-2.5 py-2 text-left"
                  style={{ background: style?.bg ?? 'var(--mic-bg)' }}
                  aria-label={`${sev} findings, ${grouped[sev].length} total`}
                >
                  <span
                    className="text-[10px] font-bold uppercase"
                    style={{ color: style?.text ?? 'var(--text-secondary)' }}
                  >
                    {sev}
                  </span>
                  <span
                    className="rounded-full px-1.5 py-0.5 text-[9px] font-medium"
                    style={{
                      background: `color-mix(in srgb, ${style?.text ?? 'currentColor'} 15%, transparent)`,
                      color: style?.text ?? 'var(--text-secondary)',
                    }}
                  >
                    {grouped[sev].length}
                  </span>
                  <span
                    className="ml-auto text-[10px]"
                    style={{ color: style?.text ?? 'var(--text-secondary)' }}
                    aria-hidden="true"
                  >
                    ▾
                  </span>
                </button>
              </CollapsibleTrigger>
              <CollapsibleContent>
                <div className="flex flex-col gap-1 p-2">
                  {grouped[sev].map(f => (
                    <FindingRow
                      key={f.id}
                      finding={f}
                      onDismiss={onDismiss}
                      onResolve={onResolve}
                    />
                  ))}
                </div>
              </CollapsibleContent>
            </div>
          </Collapsible>
        )
      })}
    </div>
  )
}

// Minimal sparkline chart for Ko score history
function KoScoreChart({ history }: { history: KoScorePoint[] }) {
  const width = 220
  const height = 48
  const padding = { x: 4, y: 4 }

  if (history.length < 2) {
    const score = history[0]?.score ?? 0
    const color = scoreColor(score)
    return (
      <div className="flex flex-col items-center justify-center gap-1 py-2">
        <span className="text-2xl font-bold tabular-nums" style={{ color }}>
          {score}
        </span>
        <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
          Waiting for history…
        </span>
      </div>
    )
  }

  const scores = history.map(p => p.score)
  const minScore = Math.max(0, Math.min(...scores) - 5)
  const maxScore = Math.min(100, Math.max(...scores) + 5)
  const range = maxScore - minScore || 1

  const innerW = width - padding.x * 2
  const innerH = height - padding.y * 2

  const points = history.map((p, i) => {
    const x = padding.x + (i / (history.length - 1)) * innerW
    const y = padding.y + (1 - (p.score - minScore) / range) * innerH
    return `${x},${y}`
  })

  const polyline = points.join(' ')
  const lastPoint = points[points.length - 1]
  const lastScore = history[history.length - 1].score
  const color = scoreColor(lastScore)

  return (
    <div className="flex flex-col gap-1">
      <svg width={width} height={height} aria-label={`Ko eval score history, latest: ${lastScore}`}>
        <defs>
          <linearGradient id="ko-area-gradient" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor={color} stopOpacity="0.25" />
            <stop offset="100%" stopColor={color} stopOpacity="0.02" />
          </linearGradient>
        </defs>
        <polygon
          points={[
            `${padding.x},${height - padding.y}`,
            ...points,
            `${width - padding.x},${height - padding.y}`,
          ].join(' ')}
          fill="url(#ko-area-gradient)"
        />
        <polyline
          points={polyline}
          fill="none"
          stroke={color}
          strokeWidth="1.5"
          strokeLinejoin="round"
          strokeLinecap="round"
        />
        {lastPoint && (
          <circle
            cx={parseFloat(lastPoint.split(',')[0])}
            cy={parseFloat(lastPoint.split(',')[1])}
            r="2.5"
            fill={color}
          />
        )}
      </svg>
      <div className="flex items-center justify-between px-1">
        <span className="text-[9px]" style={{ color: 'var(--text-secondary)' }}>
          {history.length} samples
        </span>
        <span className="text-[10px] font-bold tabular-nums" style={{ color }}>
          {lastScore}/100
        </span>
      </div>
    </div>
  )
}

function ScannerStatusCard({
  scanner,
  ability,
  hasFindings,
  findingCount,
}: {
  scanner: string
  ability: string
  hasFindings: boolean
  findingCount: number
}) {
  const dotColor = hasFindings ? '#f97316' : '#22c55e'
  const label = hasFindings ? `${findingCount} finding${findingCount !== 1 ? 's' : ''}` : 'Clean'

  return (
    <Card
      className="flex items-center gap-2 p-2.5"
      style={{
        background: 'var(--mic-bg)',
        borderColor: 'var(--pill-border)',
      }}
    >
      <span
        className="inline-block h-2 w-2 shrink-0 rounded-full"
        style={{ background: dotColor, boxShadow: `0 0 4px ${dotColor}` }}
        aria-hidden="true"
      />
      <div className="flex flex-1 flex-col gap-0.5 overflow-hidden">
        <span className="truncate text-xs font-semibold uppercase" style={{ color: 'var(--text-primary)' }}>
          {scanner}
        </span>
        <span className="truncate text-[10px]" style={{ color: 'var(--text-secondary)' }}>
          {ability.replace(/-/g, ' ')}
        </span>
        <span className="text-[10px]" style={{ color: hasFindings ? '#f97316' : 'var(--text-secondary)' }}>
          {label}
        </span>
      </div>
    </Card>
  )
}

// ---------------------------------------------------------------------------
// Main view
// ---------------------------------------------------------------------------

export default function NenView({ isConnected }: PluginViewProps) {
  const [components, setComponents] = useState<ComponentResult[]>([])
  const [findings, setFindings] = useState<FindingSummary[]>([])
  const [status, setStatus] = useState<NenStatus | null>(null)
  const [loading, setLoading] = useState(true)
  const [evaluating, setEvaluating] = useState(false)
  const [error, setError] = useState('')
  const [koHistory, setKoHistory] = useState<KoScorePoint[]>([])
  const [filterSeverity, setFilterSeverity] = useState<SeverityFilter>('all')

  // Track last evaluated_at to avoid duplicate history points
  const lastEvalAt = useRef<string>('')

  const load = useCallback(async () => {
    try {
      setError('')
      const [rawItems, rawStatus] = await Promise.allSettled([
        queryPluginItems(PLUGIN_NAME),
        queryPluginStatus(PLUGIN_NAME),
      ])

      if (rawStatus.status === 'fulfilled') {
        setStatus(rawStatus.value as unknown as NenStatus)
      }

      if (rawItems.status === 'fulfilled') {
        const val = rawItems.value as unknown
        let parsed: NenItems | null = null

        if (val && typeof val === 'object' && !Array.isArray(val) && 'items' in val) {
          parsed = val as NenItems
        } else if (Array.isArray(val)) {
          parsed = { items: val as ComponentResult[], count: (val as ComponentResult[]).length, findings: [] }
        }

        if (parsed) {
          setComponents(parsed.items ?? [])
          setFindings(parsed.findings ?? [])

          const evalAt = (rawStatus.status === 'fulfilled'
            ? (rawStatus.value as unknown as NenStatus).evaluated_at
            : '') ?? ''
          const koResult = (parsed.items ?? []).find(r => r.name === 'ko')
          if (koResult && evalAt && evalAt !== lastEvalAt.current) {
            lastEvalAt.current = evalAt
            setKoHistory(prev => {
              const next = [...prev, { ts: new Date(evalAt).getTime(), score: koResult.score }]
              return next.slice(-20)
            })
          }
        }
      } else {
        setError('Failed to load nen data')
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Unknown error')
    } finally {
      setLoading(false)
    }
  }, [])

  const runEvaluate = useCallback(async () => {
    setEvaluating(true)
    setError('')
    try {
      await nenScan()
      // Give the daemon a moment to write results before polling
      await new Promise(r => setTimeout(r, 1500))
      await load()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Evaluation failed')
    } finally {
      setEvaluating(false)
    }
  }, [load])

  const handleDismiss = useCallback((id: string) => {
    setFindings(prev => prev.filter(f => f.id !== id))
  }, [])

  const handleResolve = useCallback((id: string) => {
    setFindings(prev => prev.filter(f => f.id !== id))
  }, [])

  useEffect(() => {
    load()
    const interval = setInterval(load, 30_000)
    return () => clearInterval(interval)
  }, [load])

  // Derive scanner finding counts from the findings list
  const scannerFindings = Object.entries(SCANNER_ABILITIES).map(([scanner, ability]) => {
    const count = findings.filter(f => f.ability === ability).length
    return { scanner, ability, count, hasFindings: count > 0 }
  })

  const koComponent = components.find(c => c.name === 'ko')

  if (loading && components.length === 0) {
    return (
      <div className="flex flex-col gap-3 p-4">
        <div className="h-24 animate-pulse rounded-lg" style={{ background: 'var(--mic-bg)' }} />
        <div className="grid grid-cols-2 gap-2">
          {[0, 1, 2, 3].map(i => (
            <div key={i} className="h-16 animate-pulse rounded-md" style={{ background: 'var(--mic-bg)' }} />
          ))}
        </div>
        <div className="h-32 animate-pulse rounded-lg" style={{ background: 'var(--mic-bg)' }} />
      </div>
    )
  }

  const totalFindings = findings.length
  const visibleFindings =
    filterSeverity === 'all' ? totalFindings : findings.filter(f => f.severity.toLowerCase() === filterSeverity).length

  return (
    <div className="flex flex-col gap-4 p-4">
      {/* Connection warning */}
      {isConnected === false && (
        <p
          className="rounded px-2 py-1 text-xs"
          style={{
            background: 'color-mix(in srgb, var(--color-warning) 12%, transparent)',
            color: 'var(--color-warning)',
          }}
        >
          Dashboard disconnected — showing cached data
        </p>
      )}

      {/* Error */}
      {error && (
        <div
          className="flex items-center justify-between gap-2 rounded px-2 py-1.5"
          style={{
            background: 'color-mix(in srgb, var(--color-error) 12%, transparent)',
            color: 'var(--color-error)',
          }}
        >
          <span className="text-xs">{error}</span>
          <Button size="sm" variant="outline" onClick={() => load()}>
            Retry
          </Button>
        </div>
      )}

      {/* ── Header: overall score + meta ──────────────────────────────────── */}
      <Card style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}>
        <div className="flex items-center gap-4 p-3">
          {status && <OverallScoreRing score={status.score} />}

          <div className="flex flex-1 flex-col gap-1.5">
            <div className="flex items-center justify-between gap-1">
              <span className="text-sm font-bold" style={{ color: 'var(--text-primary)' }}>
                Nen Health
              </span>
              <div className="flex items-center gap-1">
                <Button
                  size="sm"
                  variant="outline"
                  className="h-6 px-2 text-[10px]"
                  onClick={runEvaluate}
                  disabled={evaluating || loading}
                >
                  {evaluating ? 'Running…' : 'Evaluate'}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-6 px-2 text-[10px]"
                  onClick={() => load()}
                  disabled={loading}
                >
                  {loading ? '…' : '↻'}
                </Button>
              </div>
            </div>

            <div className="flex flex-wrap gap-3">
              {status && (
                <>
                  <Stat label="Critical" value={String(status.critical_count)} alert={status.critical_count > 0} />
                  <Stat label="Findings" value={String(status.active_findings)} alert={status.active_findings > 0} />
                  <Stat
                    label="Daemon"
                    value={status.daemon_running ? 'Running' : 'Stopped'}
                    alert={!status.daemon_running}
                  />
                  <Stat label="Evaluated" value={relativeTime(status.evaluated_at)} />
                </>
              )}
            </div>
          </div>
        </div>
      </Card>

      {/* ── Scanner status Cards (gyo / en / ryu / shu) ───────────────────── */}
      <section aria-labelledby="scanners-heading">
        <h2
          id="scanners-heading"
          className="mb-2 text-[10px] font-semibold uppercase tracking-widest"
          style={{ color: 'var(--text-secondary)' }}
        >
          Scanners
        </h2>
        <div className="grid grid-cols-2 gap-2">
          {scannerFindings.map(s => (
            <ScannerStatusCard
              key={s.scanner}
              scanner={s.scanner}
              ability={s.ability}
              hasFindings={s.hasFindings}
              findingCount={s.count}
            />
          ))}
        </div>
      </section>

      {/* ── Component health cards ─────────────────────────────────────────── */}
      <section aria-labelledby="components-heading">
        <h2
          id="components-heading"
          className="mb-2 text-[10px] font-semibold uppercase tracking-widest"
          style={{ color: 'var(--text-secondary)' }}
        >
          Components ({components.length})
        </h2>
        {components.length === 0 ? (
          <p className="py-3 text-center text-xs" style={{ color: 'var(--text-secondary)' }}>
            No evaluation data — run <code className="font-mono">shu evaluate</code>
          </p>
        ) : (
          <div className="grid grid-cols-2 gap-2">
            {components.map(c => (
              <ComponentCard key={c.name} result={c} />
            ))}
          </div>
        )}
      </section>

      {/* ── Ko eval score history ──────────────────────────────────────────── */}
      <section aria-labelledby="ko-heading">
        <h2
          id="ko-heading"
          className="mb-2 text-[10px] font-semibold uppercase tracking-widest"
          style={{ color: 'var(--text-secondary)' }}
        >
          Ko Eval Score
        </h2>
        <Card style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}>
          <div className="flex flex-col gap-1 p-3">
            {koComponent ? (
              <>
                <div className="flex items-center justify-between">
                  <span className="text-xs font-medium" style={{ color: 'var(--text-secondary)' }}>
                    Promptfoo suite health
                  </span>
                  <span
                    className="text-[10px] font-medium"
                    style={{ color: trendColor(koComponent.trend) }}
                  >
                    {trendGlyph(koComponent.trend)} {koComponent.trend}
                  </span>
                </div>
                <KoScoreChart
                  history={
                    koHistory.length > 0
                      ? koHistory
                      : [{ ts: Date.now(), score: koComponent.score }]
                  }
                />
                {koComponent.issues && koComponent.issues.length > 0 && (
                  <ul className="mt-1 flex flex-col gap-0.5">
                    {koComponent.issues.map((issue, i) => (
                      <li key={i} className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                        · {issue}
                      </li>
                    ))}
                  </ul>
                )}
              </>
            ) : (
              <p className="py-2 text-center text-[11px]" style={{ color: 'var(--text-secondary)' }}>
                No Ko data yet
              </p>
            )}
          </div>
        </Card>
      </section>

      {/* ── Active findings ────────────────────────────────────────────────── */}
      <section aria-labelledby="findings-heading">
        <div className="mb-2 flex items-center justify-between gap-2">
          <h2
            id="findings-heading"
            className="text-[10px] font-semibold uppercase tracking-widest"
            style={{ color: 'var(--text-secondary)' }}
          >
            Findings
            {totalFindings > 0 && (
              <span className="ml-1 tabular-nums">
                ({filterSeverity === 'all' ? totalFindings : `${visibleFindings}/${totalFindings}`})
              </span>
            )}
          </h2>
          {totalFindings > 0 && (
            <Select
              value={filterSeverity}
              onValueChange={(v: string) => setFilterSeverity(v as SeverityFilter)}
            >
              <SelectTrigger
                className="h-6 w-[90px] border-0 bg-transparent px-1.5 text-[10px] focus:ring-0"
                style={{ color: 'var(--text-secondary)' }}
                aria-label="Filter findings by severity"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all" className="text-xs">All</SelectItem>
                <SelectItem value="critical" className="text-xs">Critical</SelectItem>
                <SelectItem value="high" className="text-xs">High</SelectItem>
                <SelectItem value="medium" className="text-xs">Medium</SelectItem>
                <SelectItem value="low" className="text-xs">Low</SelectItem>
              </SelectContent>
            </Select>
          )}
        </div>
        <FindingsSection
          findings={findings}
          onDismiss={handleDismiss}
          onResolve={handleResolve}
          filter={filterSeverity}
        />
      </section>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Inline helper component
// ---------------------------------------------------------------------------

function Stat({
  label,
  value,
  alert = false,
}: {
  label: string
  value: string
  alert?: boolean
}) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-[9px] uppercase tracking-wider" style={{ color: 'var(--text-secondary)' }}>
        {label}
      </span>
      <span
        className="text-xs font-semibold tabular-nums"
        style={{ color: alert ? '#ef4444' : 'var(--text-primary)' }}
      >
        {value}
      </span>
    </div>
  )
}
