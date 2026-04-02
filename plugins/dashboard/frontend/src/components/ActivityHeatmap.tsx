import { useMemo, useRef, useState } from 'react'
import type { RecentMission } from '../types'

const CELL_SIZE = 12
const CELL_GAP = 2
const CELL_STEP = CELL_SIZE + CELL_GAP

const MONTH_NAMES = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']
const DAY_LABELS = ['M', '', 'W', '', 'F', '', 'S']

interface DayData {
  date: string
  total: number
  completed: number
  failed: number
  warned: number
}

interface TooltipState {
  x: number
  y: number
  data: DayData
}

interface ActivityHeatmapProps {
  recent: RecentMission[]
}

function toDateStr(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
}

export function ActivityHeatmap({ recent }: ActivityHeatmapProps) {
  const [tooltip, setTooltip] = useState<TooltipState | null>(null)
  const wrapperRef = useRef<HTMLDivElement>(null)

  const byDate = useMemo<Map<string, DayData>>(() => {
    const map = new Map<string, DayData>()
    for (const m of recent) {
      const date = m.started_at.slice(0, 10)
      if (!map.has(date)) map.set(date, { date, total: 0, completed: 0, failed: 0, warned: 0 })
      const d = map.get(date)!
      d.total++
      if (m.status === 'completed') d.completed++
      else if (m.status === 'failed') d.failed++
      else if (m.status === 'cancelled' || m.status === 'stalled') d.warned++
    }
    return map
  }, [recent])

  const { days, numWeeks, monthLabels, todayStr } = useMemo(() => {
    const now = new Date()
    now.setHours(0, 0, 0, 0)
    const todayStr = toDateStr(now)

    const start = new Date(now)
    start.setMonth(start.getMonth() - 6)
    // Adjust back to Monday (getDay: 0=Sun, 1=Mon, ..., 6=Sat)
    const dow = start.getDay()
    const toMon = dow === 0 ? 6 : dow - 1
    start.setDate(start.getDate() - toMon)

    const days: string[] = []
    const cur = new Date(start)
    while (cur <= now) {
      days.push(toDateStr(cur))
      cur.setDate(cur.getDate() + 1)
    }
    const numWeeks = Math.ceil(days.length / 7)

    const monthLabels: { label: string; weekIdx: number }[] = []
    let lastMonth = -1
    days.forEach((dateStr, i) => {
      const month = parseInt(dateStr.slice(5, 7)) - 1
      if (month !== lastMonth) {
        monthLabels.push({ label: MONTH_NAMES[month], weekIdx: Math.floor(i / 7) })
        lastMonth = month
      }
    })

    return { days, numWeeks, monthLabels, todayStr }
  }, [])

  const { successRate, trendDelta } = useMemo(() => {
    let total = 0, completed = 0
    let priorT = 0, priorC = 0, curT = 0, curC = 0
    const midStr = days[Math.floor(days.length / 2)] ?? days[0]

    for (const d of byDate.values()) {
      total += d.total
      completed += d.completed
      if (d.date < midStr) {
        priorT += d.total
        priorC += d.completed
      } else {
        curT += d.total
        curC += d.completed
      }
    }

    const rate = total > 0 ? Math.round((completed / total) * 100) : 0
    const prior = priorT > 0 ? priorC / priorT : 0
    const curr = curT > 0 ? curC / curT : 0
    const delta = Math.round((curr - prior) * 100)
    return { successRate: rate, trendDelta: delta }
  }, [byDate, days])

  function handleCellEnter(e: React.MouseEvent<HTMLDivElement>, data: DayData) {
    const rect = e.currentTarget.getBoundingClientRect()
    const wrap = wrapperRef.current?.getBoundingClientRect()
    setTooltip({
      x: rect.left - (wrap?.left ?? 0) + CELL_SIZE / 2,
      y: rect.top - (wrap?.top ?? 0),
      data,
    })
  }

  return (
    <div className="heatmap-wrapper" ref={wrapperRef}>
      {/* Month labels */}
      <div className="heatmap-month-row">
        <div className="heatmap-gutter" />
        <div className="heatmap-months" style={{ width: numWeeks * CELL_STEP }}>
          {monthLabels.map((m) => (
            <span
              key={`${m.label}-${m.weekIdx}`}
              className="heatmap-month"
              style={{ left: m.weekIdx * CELL_STEP }}
            >
              {m.label}
            </span>
          ))}
        </div>
      </div>

      {/* Body: day labels + grid */}
      <div className="heatmap-body">
        <div className="heatmap-gutter">
          {DAY_LABELS.map((label, i) => (
            <div key={i} className="heatmap-day-label">
              {label}
            </div>
          ))}
        </div>

        <div
          className="heatmap-grid"
          style={{
            gridTemplateRows: `repeat(7, ${CELL_SIZE}px)`,
            gridTemplateColumns: `repeat(${numWeeks}, ${CELL_SIZE}px)`,
            gap: `${CELL_GAP}px`,
          }}
        >
          {days.map((dateStr) => {
            if (dateStr > todayStr) {
              return <div key={dateStr} className="heatmap-cell" style={{ visibility: 'hidden' }} />
            }
            const data = byDate.get(dateStr) ?? { date: dateStr, total: 0, completed: 0, failed: 0, warned: 0 }
            let cls = 'heatmap-cell'
            if (data.total === 0) cls += ' heatmap-cell--empty'
            else if (data.failed > 0) cls += ' heatmap-cell--fail'
            else if (data.warned > 0) cls += ' heatmap-cell--warn'
            else if (data.total >= 3) cls += ' heatmap-cell--green-hi'
            else cls += ' heatmap-cell--green-lo'

            return (
              <div
                key={dateStr}
                className={cls}
                onMouseEnter={(e) => handleCellEnter(e, data)}
                onMouseLeave={() => setTooltip(null)}
                aria-label={`${dateStr}: ${data.total} mission${data.total !== 1 ? 's' : ''}`}
              />
            )
          })}
        </div>
      </div>

      {/* Tooltip */}
      {tooltip && (
        <div
          className="heatmap-tooltip"
          style={{ left: tooltip.x, top: tooltip.y }}
          role="tooltip"
        >
          <div className="heatmap-tt-date">{tooltip.data.date}</div>
          <div className="heatmap-tt-counts">
            {tooltip.data.total === 0 ? (
              <span>No missions</span>
            ) : (
              <>
                <span className="heatmap-tt-ok">{tooltip.data.completed} ok</span>
                {tooltip.data.failed > 0 && (
                  <span className="heatmap-tt-fail"> · {tooltip.data.failed} failed</span>
                )}
                {tooltip.data.warned > 0 && (
                  <span className="heatmap-tt-warn"> · {tooltip.data.warned} warn</span>
                )}
              </>
            )}
          </div>
        </div>
      )}

      {/* Summary bar */}
      <div className="heatmap-summary">
        <div className="heatmap-rate-block">
          <span className="heatmap-rate-num">{successRate}%</span>
          <span className="heatmap-rate-lbl">success rate</span>
        </div>
        <div className={`heatmap-trend ${trendDelta >= 0 ? 'heatmap-trend--up' : 'heatmap-trend--down'}`}>
          <span className="heatmap-trend-arrow">{trendDelta >= 0 ? '↑' : '↓'}</span>
          <span className="heatmap-trend-pct">{Math.abs(trendDelta)}%</span>
          <span className="heatmap-trend-lbl">vs prior</span>
        </div>
      </div>
    </div>
  )
}
