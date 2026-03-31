import { useState, useMemo, useCallback, useEffect, useRef } from 'react'
import { useTracker } from '../hooks/useTracker'
import { useTrackerStats } from '../hooks/useTrackerStats'
import type { TrackerItem, TrackerPriority, TrackerStatus } from '../types'
import { neutral, status as statusColors } from '../colors'
import { Badge } from './ui/badge'

// ─────────────────────────────────────────────────────────────────────────────
// Constants & helpers
// ─────────────────────────────────────────────────────────────────────────────

const STATUSES: TrackerStatus[] = ['open', 'in-progress', 'done', 'cancelled']
const PRIORITIES: TrackerPriority[] = ['P0', 'P1', 'P2', 'P3']
export const PRIORITY_ORDER: Record<string, number> = { P0: 0, P1: 1, P2: 2, P3: 3 }

const LS_FILTERS_KEY = 'tracker-filters'
const LS_GROUP_KEY = 'tracker-group'

type GroupBy = 'none' | 'priority' | 'status' | 'label'

interface FilterState {
  statuses: string[]
  priorities: string[]
  labels: string[]
}

function priorityColor(p: string | undefined): string {
  if (p === 'P0') return statusColors.error
  if (p === 'P1') return 'oklch(0.68 0.18 40)'
  if (p === 'P2') return statusColors.warning
  return neutral.textSecondary
}

function statusColor(s: string): string {
  if (s === 'open') return statusColors.info
  if (s === 'in-progress') return statusColors.warning
  if (s === 'done') return statusColors.success
  return neutral.textSecondary
}

function formatRelativeTime(iso: string | undefined): string {
  if (!iso) return ''
  const diff = Date.now() - new Date(iso).getTime()
  if (diff < 60_000) return 'just now'
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`
  if (diff < 604_800_000) return `${Math.floor(diff / 86_400_000)}d ago`
  return new Date(iso).toLocaleDateString([], { month: 'short', day: 'numeric' })
}

function parseLabels(labels: string | undefined): string[] {
  if (!labels) return []
  return labels.split(',').map(l => l.trim()).filter(Boolean)
}

function sortItems(items: TrackerItem[]): TrackerItem[] {
  return [...items].sort((a, b) => {
    const pa = PRIORITY_ORDER[a.priority ?? 'P3'] ?? 4
    const pb = PRIORITY_ORDER[b.priority ?? 'P3'] ?? 4
    if (pa !== pb) return pa - pb
    const ta = new Date(a.updated_at ?? a.created_at ?? 0).getTime()
    const tb = new Date(b.updated_at ?? b.created_at ?? 0).getTime()
    return tb - ta
  })
}

function readStoredFilters(): FilterState {
  try {
    const v = localStorage.getItem(LS_FILTERS_KEY)
    if (v) return JSON.parse(v) as FilterState
  } catch { /* ignore */ }
  return { statuses: [], priorities: [], labels: [] }
}

function readStoredGroup(): GroupBy {
  try {
    const v = localStorage.getItem(LS_GROUP_KEY)
    if (v && ['none', 'priority', 'status', 'label'].includes(v)) return v as GroupBy
  } catch { /* ignore */ }
  return 'none'
}

// ─────────────────────────────────────────────────────────────────────────────
// Inline dropdown
// ─────────────────────────────────────────────────────────────────────────────

interface InlineDropdownProps {
  options: Array<{ value: string; label: string; color: string }>
  onSelect: (value: string) => void
  alignRight?: boolean
}

function InlineDropdown({ options, onSelect, alignRight }: InlineDropdownProps) {
  return (
    <div
      className={`absolute top-full mt-0.5 z-50 rounded shadow-lg py-1 min-w-[90px] ${alignRight ? 'right-0' : 'left-0'}`}
      style={{ backgroundColor: '#1a1a1f', border: `1px solid ${neutral.pillBorder}` }}
    >
      {options.map(opt => (
        <button
          key={opt.value}
          className="w-full text-left px-2.5 py-1 text-[11px] hover:bg-white/10 transition-colors"
          style={{ color: opt.color }}
          onMouseDown={e => { e.preventDefault(); e.stopPropagation() }}
          onClick={e => { e.stopPropagation(); onSelect(opt.value) }}
        >
          {opt.label}
        </button>
      ))}
    </div>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// Issue card
// ─────────────────────────────────────────────────────────────────────────────

type UpdateFn = (id: string, changes: { status?: string; priority?: string; labels?: string }) => Promise<void>

interface IssueCardProps {
  item: TrackerItem
  onUpdate: UpdateFn
}

function IssueCard({ item, onUpdate }: IssueCardProps) {
  const [open, setOpen] = useState(false)
  const [activeDropdown, setActiveDropdown] = useState<'status' | 'priority' | null>(null)
  const [addingLabel, setAddingLabel] = useState(false)
  const [labelInput, setLabelInput] = useState('')
  const [updating, setUpdating] = useState(false)

  const dropdownAreaRef = useRef<HTMLDivElement>(null)
  const labelInputRef = useRef<HTMLInputElement>(null)

  // Close dropdown on outside mousedown
  useEffect(() => {
    if (!activeDropdown) return
    const handler = (e: MouseEvent) => {
      if (dropdownAreaRef.current && !dropdownAreaRef.current.contains(e.target as Node)) {
        setActiveDropdown(null)
      }
    }
    document.addEventListener('mousedown', handler)
    return () => document.removeEventListener('mousedown', handler)
  }, [activeDropdown])

  // Focus the label input as soon as it mounts
  useEffect(() => {
    if (addingLabel) labelInputRef.current?.focus()
  }, [addingLabel])

  const mutate = useCallback(async (changes: { status?: string; priority?: string; labels?: string }) => {
    setUpdating(true)
    try {
      await onUpdate(item.id, changes)
    } finally {
      setUpdating(false)
    }
  }, [item.id, onUpdate])

  const handleStatusSelect = (s: string) => {
    setActiveDropdown(null)
    void mutate({ status: s })
  }

  const handlePrioritySelect = (p: string) => {
    setActiveDropdown(null)
    void mutate({ priority: p })
  }

  const handleAddLabel = () => {
    const trimmed = labelInput.trim()
    setAddingLabel(false)
    setLabelInput('')
    if (!trimmed) return
    const existing = parseLabels(item.labels)
    if (existing.includes(trimmed)) return
    void mutate({ labels: [...existing, trimmed].join(',') })
  }

  const handleRemoveLabel = (label: string) => {
    const newLabels = parseLabels(item.labels).filter(l => l !== label).join(',')
    void mutate({ labels: newLabels })
  }

  const labels = parseLabels(item.labels)
  const pColor = priorityColor(item.priority)
  const sColor = statusColor(item.status)
  const timeDisplay = formatRelativeTime(item.updated_at ?? item.created_at)

  const statusOptions = STATUSES.map(s => ({ value: s, label: s, color: statusColor(s) }))
  const priorityOptions = PRIORITIES.map(p => ({ value: p, label: p, color: priorityColor(p) }))

  return (
    <li
      className="rounded-lg border"
      style={{ borderColor: neutral.pillBorder, backgroundColor: neutral.pillBg, opacity: updating ? 0.7 : 1 }}
    >
      {/* ── Header row ── */}
      <div
        className="flex items-center gap-2 px-3 py-2 cursor-pointer select-none"
        role="button"
        tabIndex={0}
        onClick={() => { if (!activeDropdown && !addingLabel) setOpen(o => !o) }}
        onKeyDown={e => { if ((e.key === 'Enter' || e.key === ' ') && !activeDropdown && !addingLabel) setOpen(o => !o) }}
        aria-expanded={open}
      >
        {/* Badges area — priority + status share one outside-click ref */}
        <div ref={dropdownAreaRef} className="flex items-center gap-2 flex-shrink-0">
          {/* Priority badge */}
          <div className="relative">
            <Badge
              variant="outline"
              className="cursor-pointer text-[10px] font-mono uppercase hover:opacity-80 transition-opacity"
              style={{ color: pColor, borderColor: pColor }}
              onClick={e => { e.stopPropagation(); setActiveDropdown(d => d === 'priority' ? null : 'priority') }}
              aria-label="Change priority"
            >
              {item.priority ?? '—'}
            </Badge>
            {activeDropdown === 'priority' && (
              <InlineDropdown options={priorityOptions} onSelect={handlePrioritySelect} />
            )}
          </div>

          {/* Status badge */}
          <div className="relative">
            <Badge
              variant="outline"
              className="cursor-pointer text-[10px] capitalize hover:opacity-80 transition-opacity"
              style={{ color: sColor, borderColor: sColor }}
              onClick={e => { e.stopPropagation(); setActiveDropdown(d => d === 'status' ? null : 'status') }}
              aria-label="Change status"
            >
              {item.status}
            </Badge>
            {activeDropdown === 'status' && (
              <InlineDropdown options={statusOptions} onSelect={handleStatusSelect} alignRight />
            )}
          </div>
        </div>

        {/* Title */}
        <span
          className="flex-1 min-w-0 text-sm leading-snug truncate"
          style={{ color: neutral.textPrimary }}
        >
          {item.title}
        </span>

        {/* Labels + add-tag area */}
        <div
          className="flex gap-1 flex-shrink-0 flex-wrap items-center"
          onClick={e => e.stopPropagation()}
        >
          {labels.map(l => (
            <span
              key={l}
              className="group flex items-center gap-0.5 text-[10px] px-1.5 py-0.5 rounded"
              style={{ backgroundColor: 'rgba(255,255,255,0.06)', color: neutral.textSecondary }}
            >
              {l}
              <button
                className="opacity-0 group-hover:opacity-100 ml-0.5 transition-opacity hover:text-red-400 leading-none"
                onClick={e => { e.stopPropagation(); handleRemoveLabel(l) }}
                aria-label={`Remove label ${l}`}
              >
                ×
              </button>
            </span>
          ))}

          {addingLabel ? (
            <input
              ref={labelInputRef}
              className="text-[10px] px-1.5 py-0.5 rounded w-16 outline-none bg-transparent"
              style={{ border: `1px solid ${neutral.pillBorder}`, color: neutral.textPrimary }}
              value={labelInput}
              onChange={e => setLabelInput(e.target.value)}
              onKeyDown={e => {
                if (e.key === 'Enter') { e.preventDefault(); handleAddLabel() }
                if (e.key === 'Escape') { setAddingLabel(false); setLabelInput('') }
              }}
              onBlur={handleAddLabel}
              placeholder="label"
            />
          ) : (
            <button
              className="text-[10px] px-1 py-0.5 rounded opacity-40 hover:opacity-80 transition-opacity"
              style={{ color: neutral.textSecondary, border: `1px solid ${neutral.pillBorder}` }}
              onClick={e => { e.stopPropagation(); setAddingLabel(true) }}
              aria-label="Add label"
            >
              + tag
            </button>
          )}
        </div>

        {/* Time */}
        {timeDisplay && (
          <time
            className="text-[11px] flex-shrink-0"
            style={{ color: neutral.textSecondary }}
          >
            {timeDisplay}
          </time>
        )}

        <span className="text-[10px] flex-shrink-0" style={{ color: neutral.textSecondary }} aria-hidden>
          {open ? '▲' : '▼'}
        </span>
      </div>

      {/* ── Expanded detail ── */}
      {open && (
        <div
          className="px-3 pb-3 pt-1 border-t text-xs space-y-1.5"
          style={{ borderColor: neutral.pillBorder, color: neutral.textSecondary }}
        >
          <p className="font-mono text-[10px]" style={{ color: neutral.textSecondary }}>
            {item.id}
          </p>

          {item.description && (
            <p className="leading-relaxed" style={{ color: neutral.textPrimary }}>
              {item.description}
            </p>
          )}

          {item.parent_id && (
            <p>
              <span style={{ color: neutral.textSecondary }}>Parent: </span>
              <span className="font-mono" style={{ color: neutral.textPrimary }}>{item.parent_id}</span>
            </p>
          )}

          {item.assignee && (
            <p>
              <span style={{ color: neutral.textSecondary }}>Assignee: </span>
              <span style={{ color: neutral.textPrimary }}>{item.assignee}</span>
            </p>
          )}

          <div className="flex gap-4">
            {item.created_at && (
              <p>
                <span style={{ color: neutral.textSecondary }}>Created: </span>
                <span>{new Date(item.created_at).toLocaleString()}</span>
              </p>
            )}
            {item.updated_at && (
              <p>
                <span style={{ color: neutral.textSecondary }}>Updated: </span>
                <span>{new Date(item.updated_at).toLocaleString()}</span>
              </p>
            )}
          </div>
        </div>
      )}
    </li>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// Collapsible group section
// ─────────────────────────────────────────────────────────────────────────────

interface GroupSectionProps {
  title: string
  items: TrackerItem[]
  accentColor?: string
  onUpdate: UpdateFn
}

function GroupSection({ title, items, accentColor, onUpdate }: GroupSectionProps) {
  const [collapsed, setCollapsed] = useState(false)

  return (
    <section>
      <button
        className="flex items-center gap-2 w-full text-left py-1.5 mb-2"
        onClick={() => setCollapsed(c => !c)}
        aria-expanded={!collapsed}
      >
        {accentColor && (
          <span
            className="w-2 h-2 rounded-full flex-shrink-0"
            style={{ backgroundColor: accentColor }}
            aria-hidden
          />
        )}
        <span className="text-xs font-medium uppercase tracking-wide" style={{ color: neutral.textSecondary }}>
          {title}
        </span>
        <span
          className="text-[10px] px-1.5 py-0.5 rounded font-mono ml-0.5"
          style={{ backgroundColor: 'rgba(255,255,255,0.06)', color: neutral.textSecondary }}
        >
          {items.length}
        </span>
        <span className="text-[10px] ml-auto" style={{ color: neutral.textSecondary }} aria-hidden>
          {collapsed ? '▶' : '▼'}
        </span>
      </button>

      {!collapsed && (
        <ul className="space-y-2 mb-6">
          {items.map(item => <IssueCard key={item.id} item={item} onUpdate={onUpdate} />)}
        </ul>
      )}
    </section>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// Filter toggle helper
// ─────────────────────────────────────────────────────────────────────────────

function toggle(arr: string[], value: string): string[] {
  return arr.includes(value) ? arr.filter(v => v !== value) : [...arr, value]
}

// ─────────────────────────────────────────────────────────────────────────────
// Main panel
// ─────────────────────────────────────────────────────────────────────────────

export function TrackerPanel() {
  const { items, loading, error, refresh, updateItem } = useTracker()
  const { stats } = useTrackerStats()

  const [filters, setFilters] = useState<FilterState>(readStoredFilters)
  const [groupBy, setGroupBy] = useState<GroupBy>(readStoredGroup)
  const [searchRaw, setSearchRaw] = useState('')
  const [search, setSearch] = useState('')
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  // Persist filter state
  useEffect(() => {
    try { localStorage.setItem(LS_FILTERS_KEY, JSON.stringify(filters)) } catch { /* ignore */ }
  }, [filters])

  useEffect(() => {
    try { localStorage.setItem(LS_GROUP_KEY, groupBy) } catch { /* ignore */ }
  }, [groupBy])

  // Debounce search
  useEffect(() => {
    if (debounceRef.current != null) clearTimeout(debounceRef.current)
    debounceRef.current = setTimeout(() => setSearch(searchRaw), 300)
    return () => { if (debounceRef.current != null) clearTimeout(debounceRef.current) }
  }, [searchRaw])

  // Collect all unique labels from items for sidebar
  const allLabels = useMemo(() => {
    const counts: Record<string, number> = {}
    for (const item of items) {
      for (const l of parseLabels(item.labels)) {
        counts[l] = (counts[l] ?? 0) + 1
      }
    }
    return Object.entries(counts).sort((a, b) => b[1] - a[1])
  }, [items])

  // Apply filters + search
  const filtered = useMemo(() => {
    let result = items

    if (filters.statuses.length > 0) {
      result = result.filter(item => filters.statuses.includes(item.status))
    }
    if (filters.priorities.length > 0) {
      result = result.filter(item => item.priority != null && filters.priorities.includes(item.priority))
    }
    if (filters.labels.length > 0) {
      result = result.filter(item => {
        const itemLabels = parseLabels(item.labels)
        return filters.labels.every(l => itemLabels.includes(l))
      })
    }
    if (search.trim()) {
      const q = search.toLowerCase()
      result = result.filter(item =>
        item.title.toLowerCase().includes(q) ||
        (item.description ?? '').toLowerCase().includes(q) ||
        item.id.toLowerCase().includes(q),
      )
    }

    return result
  }, [items, filters, search])

  // Group filtered items
  const grouped = useMemo((): Array<{ key: string; items: TrackerItem[] }> => {
    if (groupBy === 'none') return [{ key: '__all__', items: sortItems(filtered) }]

    const groups: Record<string, TrackerItem[]> = {}

    for (const item of filtered) {
      let keys: string[]
      if (groupBy === 'priority') keys = [item.priority ?? 'none']
      else if (groupBy === 'status') keys = [item.status]
      else {
        const itemLabels = parseLabels(item.labels)
        keys = itemLabels.length > 0 ? itemLabels : ['unlabeled']
      }

      for (const k of keys) {
        if (!groups[k]) groups[k] = []
        groups[k].push(item)
      }
    }

    // Sort group keys deterministically
    const sortedKeys = Object.keys(groups).sort((a, b) => {
      if (groupBy === 'priority') {
        return (PRIORITY_ORDER[a] ?? 4) - (PRIORITY_ORDER[b] ?? 4)
      }
      if (groupBy === 'status') {
        return STATUSES.indexOf(a as TrackerStatus) - STATUSES.indexOf(b as TrackerStatus)
      }
      return a.localeCompare(b)
    })

    return sortedKeys.map(k => ({ key: k, items: sortItems(groups[k]) }))
  }, [filtered, groupBy])

  const groupAccentColor = useCallback((key: string): string | undefined => {
    if (groupBy === 'priority') return priorityColor(key)
    if (groupBy === 'status') return statusColor(key)
    return undefined
  }, [groupBy])

  return (
    <div className="flex h-full overflow-hidden" style={{ color: neutral.textPrimary }}>
      {/* ── Left sidebar ────────────────────────────────────── */}
      <aside
        className="w-44 flex-shrink-0 flex flex-col gap-4 overflow-y-auto py-4 pr-3 pl-1"
        style={{ borderRight: `1px solid ${neutral.pillBorder}` }}
      >
        {/* Status filters */}
        <section>
          <p className="text-[10px] uppercase tracking-widest mb-2 font-medium" style={{ color: neutral.textSecondary }}>
            Status
          </p>
          <ul className="space-y-1">
            {STATUSES.map(s => {
              const count = stats?.by_status[s] ?? 0
              const active = filters.statuses.includes(s)
              return (
                <li key={s}>
                  <button
                    className="flex items-center justify-between w-full px-2 py-1 rounded text-left text-xs transition-colors"
                    style={{
                      backgroundColor: active ? 'rgba(255,255,255,0.08)' : 'transparent',
                      color: active ? neutral.textPrimary : neutral.textSecondary,
                    }}
                    onClick={() => setFilters(f => ({ ...f, statuses: toggle(f.statuses, s) }))}
                    aria-pressed={active}
                  >
                    <span className="capitalize">{s}</span>
                    <span
                      className="text-[10px] font-mono px-1 rounded"
                      style={{ backgroundColor: 'rgba(255,255,255,0.06)', color: neutral.textSecondary }}
                    >
                      {count}
                    </span>
                  </button>
                </li>
              )
            })}
          </ul>
        </section>

        {/* Priority filters */}
        <section>
          <p className="text-[10px] uppercase tracking-widest mb-2 font-medium" style={{ color: neutral.textSecondary }}>
            Priority
          </p>
          <ul className="space-y-1">
            {PRIORITIES.map(p => {
              const count = stats?.by_priority[p] ?? 0
              const active = filters.priorities.includes(p)
              const pColor = priorityColor(p)
              return (
                <li key={p}>
                  <button
                    className="flex items-center gap-2 justify-between w-full px-2 py-1 rounded text-left text-xs transition-colors"
                    style={{
                      backgroundColor: active ? 'rgba(255,255,255,0.08)' : 'transparent',
                      color: active ? neutral.textPrimary : neutral.textSecondary,
                    }}
                    onClick={() => setFilters(f => ({ ...f, priorities: toggle(f.priorities, p) }))}
                    aria-pressed={active}
                  >
                    <span className="flex items-center gap-1.5">
                      <span
                        className="w-2 h-2 rounded-full flex-shrink-0"
                        style={{ backgroundColor: pColor }}
                        aria-hidden
                      />
                      {p}
                    </span>
                    <span
                      className="text-[10px] font-mono px-1 rounded"
                      style={{ backgroundColor: 'rgba(255,255,255,0.06)', color: neutral.textSecondary }}
                    >
                      {count}
                    </span>
                  </button>
                </li>
              )
            })}
          </ul>
        </section>

        {/* Labels */}
        {allLabels.length > 0 && (
          <section>
            <p className="text-[10px] uppercase tracking-widest mb-2 font-medium" style={{ color: neutral.textSecondary }}>
              Labels
            </p>
            <div className="flex flex-wrap gap-1">
              {allLabels.map(([label, count]) => {
                const active = filters.labels.includes(label)
                return (
                  <button
                    key={label}
                    className="flex items-center gap-1 text-[10px] px-1.5 py-0.5 rounded transition-colors"
                    style={{
                      backgroundColor: active ? 'rgba(217,119,87,0.2)' : 'rgba(255,255,255,0.06)',
                      color: active ? '#d97757' : neutral.textSecondary,
                      border: active ? '1px solid rgba(217,119,87,0.4)' : '1px solid transparent',
                    }}
                    onClick={() => setFilters(f => ({ ...f, labels: toggle(f.labels, label) }))}
                    aria-pressed={active}
                  >
                    {label}
                    <span className="font-mono">{count}</span>
                  </button>
                )
              })}
            </div>
          </section>
        )}

        {/* Clear filters */}
        {(filters.statuses.length > 0 || filters.priorities.length > 0 || filters.labels.length > 0) && (
          <button
            className="text-[10px] text-left px-2 py-1 rounded transition-colors"
            style={{ color: statusColors.error }}
            onClick={() => setFilters({ statuses: [], priorities: [], labels: [] })}
          >
            Clear filters
          </button>
        )}
      </aside>

      {/* ── Main content ─────────────────────────────────────── */}
      <div className="flex-1 flex flex-col min-w-0 overflow-hidden">
        {/* Top bar */}
        <div
          className="flex items-center gap-3 px-4 py-3 flex-shrink-0"
          style={{ borderBottom: `1px solid ${neutral.pillBorder}` }}
        >
          {/* Count */}
          <span className="text-xs flex-shrink-0" style={{ color: neutral.textSecondary }}>
            {filtered.length} issue{filtered.length !== 1 ? 's' : ''} found
          </span>

          {/* Search */}
          <input
            type="search"
            placeholder="Search issues…"
            value={searchRaw}
            onChange={e => setSearchRaw(e.target.value)}
            className="flex-1 min-w-0 text-xs px-2 py-1 rounded outline-none bg-transparent"
            style={{
              border: `1px solid ${neutral.pillBorder}`,
              color: neutral.textPrimary,
            }}
          />

          {/* Group by */}
          <label className="flex items-center gap-1.5 flex-shrink-0 text-xs" style={{ color: neutral.textSecondary }}>
            Group by
            <select
              value={groupBy}
              onChange={e => setGroupBy(e.target.value as GroupBy)}
              className="text-xs px-1.5 py-0.5 rounded outline-none cursor-pointer"
              style={{
                background: neutral.pillBg,
                border: `1px solid ${neutral.pillBorder}`,
                color: neutral.textPrimary,
              }}
            >
              <option value="none">None</option>
              <option value="priority">Priority</option>
              <option value="status">Status</option>
              <option value="label">Label</option>
            </select>
          </label>

          {/* Refresh */}
          <button
            className="text-xs flex-shrink-0 px-2 py-0.5 rounded transition-opacity"
            style={{
              color: neutral.textSecondary,
              border: `1px solid ${neutral.pillBorder}`,
              opacity: loading ? 0.5 : 1,
            }}
            onClick={() => { void refresh() }}
            disabled={loading}
            aria-label="Refresh"
          >
            {loading ? '…' : '↺'}
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto px-4 py-3">
          {error && (
            <p className="text-xs mb-3 px-2 py-1.5 rounded" style={{ color: statusColors.error, backgroundColor: statusColors.errorBg }}>
              {error}
            </p>
          )}

          {!loading && !error && filtered.length === 0 && (
            <p className="text-xs text-center py-8" style={{ color: neutral.textSecondary }}>
              No issues match the current filters.
            </p>
          )}

          {groupBy === 'none' ? (
            <ul className="space-y-2">
              {grouped[0]?.items.map(item => (
                <IssueCard key={item.id} item={item} onUpdate={updateItem} />
              ))}
            </ul>
          ) : (
            grouped.map(({ key, items: groupItems }) => (
              <GroupSection
                key={key}
                title={key}
                items={groupItems}
                accentColor={groupAccentColor(key)}
                onUpdate={updateItem}
              />
            ))
          )}
        </div>
      </div>
    </div>
  )
}
