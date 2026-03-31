import { useState } from 'react'
import {
  BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer,
  PieChart, Pie, Cell,
  LineChart, Line, CartesianGrid,
} from 'recharts'
import { useMetrics } from '../hooks/useMetrics'
import { claude, neutral } from '../colors'
import { Badge } from './ui/badge'
import { Button } from './ui/button'

function formatDuration(s: number): string {
  if (s < 60) return `${s.toFixed(0)}s`
  if (s < 3_600) return `${(s / 60).toFixed(1)}m`
  return `${(s / 3_600).toFixed(1)}h`
}

const TOOLTIP_STYLE = {
  contentStyle: { background: neutral.pillBg, border: `1px solid ${neutral.pillBorder}`, borderRadius: 6, fontSize: 12 },
  labelStyle: { color: neutral.textPrimary },
  itemStyle: { color: neutral.textPrimary },
}

const LAST_OPTIONS = [10, 25, 50, 100] as const

type RecentStatus = 'completed' | 'failed' | 'cancelled' | 'in_progress' | string

function recentStatusBadgeStyle(s: RecentStatus): React.CSSProperties {
  switch (s) {
    case 'completed': return { color: 'var(--color-success)', borderColor: 'var(--color-success)' }
    case 'failed': return { color: 'var(--color-error)', borderColor: 'var(--color-error)' }
    case 'in_progress': return { color: 'var(--accent)', borderColor: 'var(--accent)' }
    case 'cancelled': return { color: 'var(--text-secondary)', borderColor: 'var(--pill-border)' }
    default: return { color: 'var(--text-secondary)', borderColor: 'var(--pill-border)' }
  }
}

export function MetricsPanel() {
  const [last, setLast] = useState(25)
  const { metrics, loading, error, refresh } = useMetrics(last)

  const domainData = metrics
    ? Object.entries(metrics.by_domain).map(([domain, stats]) => ({
        domain: domain.length > 12 ? domain.slice(0, 12) + '…' : domain,
        completed: stats.completed,
        failed: stats.failed,
        cancelled: stats.cancelled,
      }))
    : []

  const trendData = metrics
    ? metrics.recent
        .slice()
        .reverse()
        .map((m, i) => ({
          i,
          label: m.task.length > 16 ? m.task.slice(0, 16) + '…' : m.task,
          duration: parseFloat((m.duration_s / 60).toFixed(2)),
        }))
    : []

  const personaData = metrics
    ? Object.entries(metrics.by_persona)
        .map(([name, stats]) => ({
          name: name.length > 14 ? name.slice(0, 14) + '…' : name,
          phases: stats.phases,
          completed: stats.completed,
          failed: stats.failed,
        }))
        .sort((a, b) => b.phases - a.phases)
        .slice(0, 8)
    : []

  const pieData = metrics
    ? [
        { name: 'Completed', value: metrics.completed, color: claude.chart1 },
        { name: 'Failed', value: metrics.failed, color: claude.chart2 },
        { name: 'Cancelled', value: metrics.cancelled, color: claude.chart3 },
        {
          name: 'Other',
          value: Math.max(0, metrics.total - metrics.completed - metrics.failed - metrics.cancelled),
          color: claude.chart4,
        },
      ].filter(d => d.value > 0)
    : []

  return (
    <div className="metrics-panel">
      <div className="metrics-toolbar">
        <span className="metrics-toolbar-label">Last</span>
        <div className="metrics-range-btns">
          {LAST_OPTIONS.map(n => (
            <Button
              key={n}
              variant="outline"
              size="sm"
              onClick={() => setLast(n)}
              className="h-auto py-0.5 px-2 rounded-full text-[11px]"
              style={
                last === n
                  ? { color: 'var(--accent)', borderColor: 'var(--accent)', background: 'var(--accent-soft)' }
                  : { color: 'var(--text-secondary)', borderColor: 'var(--pill-border)', background: 'transparent' }
              }
            >
              {n}
            </Button>
          ))}
        </div>
        <Button
          variant="ghost"
          size="sm"
          onClick={refresh}
          aria-label="Refresh metrics"
          className="h-auto py-0.5 px-1.5 text-sm"
          style={{ color: 'var(--text-secondary)' }}
        >
          ↻
        </Button>
      </div>

      {loading && <div className="panel-empty">Loading metrics…</div>}
      {error && <div className="panel-empty panel-empty--error">{error}</div>}

      {metrics && (
        <>
          <div className="metrics-stats">
            <div className="metrics-stat">
              <span className="metrics-stat-value">{metrics.total}</span>
              <span className="metrics-stat-label">total</span>
            </div>
            <div className="metrics-stat">
              <span className="metrics-stat-value metrics-stat-value--green">{metrics.completed}</span>
              <span className="metrics-stat-label">completed</span>
            </div>
            <div className="metrics-stat">
              <span className="metrics-stat-value metrics-stat-value--red">{metrics.failed}</span>
              <span className="metrics-stat-label">failed</span>
            </div>
            <div className="metrics-stat">
              <span className="metrics-stat-value">{formatDuration(metrics.avg_duration_s)}</span>
              <span className="metrics-stat-label">avg duration</span>
            </div>
          </div>

          <div className="metrics-charts">
            {/* Domain breakdown */}
            {domainData.length > 0 && (
              <div className="metrics-chart-section">
                <h4 className="metrics-chart-title">By Domain</h4>
                <ResponsiveContainer width="100%" height={130}>
                  <BarChart data={domainData} margin={{ top: 4, right: 8, left: -22, bottom: 0 }}>
                    <XAxis
                      dataKey="domain"
                      tick={{ fill: neutral.textSecondary, fontSize: 10 }}
                      axisLine={false}
                      tickLine={false}
                    />
                    <YAxis
                      tick={{ fill: neutral.textSecondary, fontSize: 10 }}
                      axisLine={false}
                      tickLine={false}
                    />
                    <Tooltip {...TOOLTIP_STYLE} />
                    <Bar dataKey="completed" stackId="s" fill={claude.chart1} />
                    <Bar dataKey="failed" stackId="s" fill={claude.chart2} />
                    <Bar dataKey="cancelled" stackId="s" fill={claude.chart3} radius={[2, 2, 0, 0]} />
                  </BarChart>
                </ResponsiveContainer>
              </div>
            )}

            {/* Duration trend */}
            {trendData.length > 1 && (
              <div className="metrics-chart-section">
                <h4 className="metrics-chart-title">Duration Trend (min)</h4>
                <ResponsiveContainer width="100%" height={100}>
                  <LineChart data={trendData} margin={{ top: 4, right: 8, left: -22, bottom: 0 }}>
                    <CartesianGrid strokeDasharray="3 3" stroke={neutral.pillBorder} vertical={false} />
                    <XAxis dataKey="i" hide />
                    <YAxis tick={{ fill: neutral.textSecondary, fontSize: 10 }} axisLine={false} tickLine={false} />
                    <Tooltip
                      {...TOOLTIP_STYLE}
                      formatter={(v: unknown) => [`${v}m`, 'duration']}
                      labelFormatter={(_: unknown, payload: unknown[]) => {
                        const p = payload as Array<{ payload?: { label?: string } }>
                        return p[0]?.payload?.label ?? ''
                      }}
                    />
                    <Line
                      type="monotone"
                      dataKey="duration"
                      stroke={claude.chart1}
                      strokeWidth={2}
                      dot={false}
                      activeDot={{ r: 4, fill: claude.chart1 }}
                    />
                  </LineChart>
                </ResponsiveContainer>
              </div>
            )}

            {/* Status pie + legend */}
            {pieData.length > 0 && (
              <div className="metrics-chart-section">
                <h4 className="metrics-chart-title">Status Distribution</h4>
                <div className="metrics-pie-row">
                  <PieChart width={110} height={110}>
                    <Pie
                      data={pieData}
                      cx={50}
                      cy={50}
                      innerRadius={28}
                      outerRadius={50}
                      paddingAngle={2}
                      dataKey="value"
                    >
                      {pieData.map((entry, i) => (
                        <Cell key={i} fill={entry.color} />
                      ))}
                    </Pie>
                  </PieChart>
                  <ul className="metrics-legend">
                    {pieData.map(d => (
                      <li key={d.name} className="metrics-legend-item">
                        <span className="metrics-legend-dot" style={{ background: d.color }} />
                        <span className="metrics-legend-name">{d.name}</span>
                        <span className="metrics-legend-count">{d.value}</span>
                      </li>
                    ))}
                  </ul>
                </div>
              </div>
            )}

            {/* Top personas */}
            {personaData.length > 0 && (
              <div className="metrics-chart-section">
                <h4 className="metrics-chart-title">Top Personas (by phases)</h4>
                <ResponsiveContainer width="100%" height={130}>
                  <BarChart
                    data={personaData}
                    layout="vertical"
                    margin={{ top: 4, right: 8, left: 4, bottom: 0 }}
                  >
                    <XAxis
                      type="number"
                      tick={{ fill: neutral.textSecondary, fontSize: 10 }}
                      axisLine={false}
                      tickLine={false}
                    />
                    <YAxis
                      type="category"
                      dataKey="name"
                      width={100}
                      tick={{ fill: neutral.textSecondary, fontSize: 10 }}
                      axisLine={false}
                      tickLine={false}
                    />
                    <Tooltip {...TOOLTIP_STYLE} />
                    <Bar dataKey="completed" stackId="s" fill={claude.chart1} />
                    <Bar dataKey="failed" stackId="s" fill={claude.chart2} radius={[0, 2, 2, 0]} />
                  </BarChart>
                </ResponsiveContainer>
              </div>
            )}
          </div>

          {/* Recent missions */}
          {metrics.recent.length > 0 && (
            <div className="metrics-recent">
              <h4 className="metrics-chart-title">Recent</h4>
              <ul className="metrics-recent-list">
                {metrics.recent.map((m, i) => (
                  <li key={i} className="metrics-recent-item">
                    <Badge
                      variant="outline"
                      className="flex-shrink-0"
                      style={{ fontSize: 10, ...recentStatusBadgeStyle(m.status) }}
                    >
                      {m.status}
                    </Badge>
                    <span className="metrics-recent-task">{m.task}</span>
                    <span className="metrics-recent-duration">{formatDuration(m.duration_s)}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </>
      )}
    </div>
  )
}
