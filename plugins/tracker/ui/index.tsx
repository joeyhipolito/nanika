import { useState, useEffect, useCallback, useMemo, useRef, type FormEvent, type KeyboardEvent } from 'react'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Card } from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { queryPluginItems, queryPluginStatus, pluginAction } from '@/lib/wails'
import type { PluginViewProps } from '@/types'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface Issue {
  id: string
  seq_id?: number | null
  title: string
  description?: string | null
  status: string
  priority?: string | null
  labels?: string | null
  assignee?: string | null
  parent_id?: string | null
  created_at: string
  updated_at: string
}

interface TrackerStatus {
  count: number
  ok: boolean
}

type ColumnKey = 'open' | 'in_progress' | 'done' | 'cancelled'
type ViewMode = 'kanban' | 'list'
type SortKey = 'priority' | 'title' | 'status' | 'created_at' | 'updated_at'
type SortDir = 'asc' | 'desc'

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const PLUGIN_NAME = 'tracker'

const COLUMNS: { key: ColumnKey; label: string }[] = [
  { key: 'open', label: 'Open' },
  { key: 'in_progress', label: 'In Progress' },
  { key: 'done', label: 'Done' },
  { key: 'cancelled', label: 'Cancelled' },
]

const PRIORITY_CONFIG: Record<string, { bg: string; text: string; border: string }> = {
  P0: { bg: 'color-mix(in srgb, #ef4444 12%, transparent)', text: '#ef4444', border: 'color-mix(in srgb, #ef4444 40%, transparent)' },
  P1: { bg: 'color-mix(in srgb, #f97316 12%, transparent)', text: '#f97316', border: 'color-mix(in srgb, #f97316 40%, transparent)' },
  P2: { bg: 'color-mix(in srgb, #eab308 12%, transparent)', text: '#eab308', border: 'color-mix(in srgb, #eab308 40%, transparent)' },
  P3: { bg: 'color-mix(in srgb, #8b5cf6 12%, transparent)', text: '#8b5cf6', border: 'color-mix(in srgb, #8b5cf6 40%, transparent)' },
}

const STATUS_CONFIG: Record<string, { dot: string; label: string }> = {
  open:        { dot: '#76766e', label: 'Open' },
  in_progress: { dot: '#d97757', label: 'In Progress' },
  done:        { dot: '#22c55e', label: 'Done' },
  cancelled:   { dot: '#ef4444', label: 'Cancelled' },
}

const PRIORITY_ORDER: Record<string, number> = { P0: 0, P1: 1, P2: 2, P3: 3 }
const ALL_PRIORITIES = ['P0', 'P1', 'P2', 'P3']
const ALL_STATUSES: ColumnKey[] = ['open', 'in_progress', 'done', 'cancelled']

const STATUS_OPTIONS = ALL_STATUSES.map((s) => ({ value: s, label: STATUS_CONFIG[s]?.label ?? s }))
const PRIORITY_OPTIONS = [
  { value: '', label: '—' },
  ...ALL_PRIORITIES.map((p) => ({ value: p, label: p })),
]

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function parseLabels(raw: string | null | undefined): string[] {
  if (!raw) return []
  return raw
    .split(',')
    .map((l: string) => l.trim())
    .filter((l: string) => l && !l.startsWith('linear:'))
}

function formatId(issue: Issue): string {
  return issue.seq_id != null ? `TRK-${issue.seq_id}` : issue.id
}

function prioritySort(p: string | null | undefined): number {
  return PRIORITY_ORDER[p || ''] ?? 99
}

function sortIssues(issues: Issue[], key: SortKey, dir: SortDir): Issue[] {
  return [...issues].sort((a, b) => {
    let cmp = 0
    if (key === 'priority') {
      cmp = prioritySort(a.priority) - prioritySort(b.priority)
    } else if (key === 'title') {
      cmp = a.title.localeCompare(b.title)
    } else if (key === 'status') {
      cmp = a.status.localeCompare(b.status)
    } else if (key === 'created_at') {
      cmp = a.created_at.localeCompare(b.created_at)
    } else if (key === 'updated_at') {
      cmp = a.updated_at.localeCompare(b.updated_at)
    }
    return dir === 'asc' ? cmp : -cmp
  })
}

// ---------------------------------------------------------------------------
// Atoms: IssueIdBadge, PriorityBadge, StatusDot
// ---------------------------------------------------------------------------

function IssueIdBadge({ issue }: { issue: Issue }) {
  return (
    <Badge
      variant="outline"
      className="font-mono text-[10px] px-1.5 py-0 h-5 rounded"
      style={{ color: 'var(--accent)', borderColor: 'color-mix(in srgb, var(--accent) 40%, transparent)' }}
    >
      {formatId(issue)}
    </Badge>
  )
}

function PriorityBadge({ priority }: { priority: string | null | undefined }) {
  if (!priority) return <span style={{ color: 'var(--text-secondary)' }} className="text-[10px]">—</span>
  const cfg = PRIORITY_CONFIG[priority]
  if (!cfg) return <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>{priority}</span>
  return (
    <Badge
      variant="outline"
      className="text-[10px] font-bold px-1.5 py-0 h-5 rounded"
      style={{ background: cfg.bg, color: cfg.text, borderColor: cfg.border }}
    >
      {priority}
    </Badge>
  )
}

function StatusDot({ status }: { status: string }) {
  const cfg = STATUS_CONFIG[status]
  const color = cfg?.dot ?? 'var(--text-secondary)'
  return (
    <span className="inline-flex items-center gap-1.5 text-[11px]" style={{ color: 'var(--text-primary)' }}>
      <span
        className="inline-block rounded-full"
        style={{ width: '6px', height: '6px', background: color, flexShrink: 0 }}
      />
      {cfg?.label ?? status.replace(/_/g, ' ')}
    </span>
  )
}

// ---------------------------------------------------------------------------
// Compact inline Select wrappers (shadcn)
// ---------------------------------------------------------------------------

interface InlineStatusSelectProps {
  value: string
  onValueChange: (val: string) => void
}

function InlineStatusSelect({ value, onValueChange }: InlineStatusSelectProps) {
  return (
    <Select value={value} onValueChange={onValueChange}>
      <SelectTrigger
        className="h-6 min-w-[100px] w-auto px-2 py-0 text-[11px] border-transparent focus:ring-0 focus:ring-offset-0"
        style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)', color: 'var(--text-primary)' }}
        onClick={(e) => e.stopPropagation()}
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}>
        {STATUS_OPTIONS.map((opt) => (
          <SelectItem key={opt.value} value={opt.value} className="text-xs" style={{ color: 'var(--text-primary)' }}>
            <StatusDot status={opt.value} />
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

interface InlinePrioritySelectProps {
  value: string
  onValueChange: (val: string) => void
}

function InlinePrioritySelect({ value, onValueChange }: InlinePrioritySelectProps) {
  const cfg = value ? PRIORITY_CONFIG[value] : undefined
  return (
    <Select value={value} onValueChange={onValueChange}>
      <SelectTrigger
        className="h-6 w-[60px] px-1.5 py-0 text-[10px] font-bold focus:ring-0 focus:ring-offset-0"
        style={{
          background: cfg ? cfg.bg : 'var(--mic-bg)',
          borderColor: cfg ? cfg.border : 'var(--pill-border)',
          color: cfg ? cfg.text : 'var(--text-secondary)',
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <SelectValue placeholder="—" />
      </SelectTrigger>
      <SelectContent style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}>
        {PRIORITY_OPTIONS.map((opt) => (
          <SelectItem key={opt.value} value={opt.value} className="text-xs" style={{ color: 'var(--text-primary)' }}>
            {opt.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

// ---------------------------------------------------------------------------
// InlineCreateForm
// ---------------------------------------------------------------------------

interface InlineCreateFormProps {
  onCreate: (title: string, priority: string) => Promise<void>
}

function InlineCreateForm({ onCreate }: InlineCreateFormProps) {
  const [open, setOpen] = useState(false)
  const [title, setTitle] = useState('')
  const [priority, setPriority] = useState('')
  const [saving, setSaving] = useState(false)
  const inputRef = useRef<HTMLInputElement>(null)

  function handleOpen() {
    setOpen(true)
    setTimeout(() => inputRef.current?.focus(), 0)
  }

  async function handleSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault()
    const trimmed = title.trim()
    if (!trimmed) return
    setSaving(true)
    try {
      await onCreate(trimmed, priority)
      setTitle('')
      setPriority('')
      setOpen(false)
    } finally {
      setSaving(false)
    }
  }

  function handleKeyDown(e: KeyboardEvent<HTMLFormElement>) {
    if (e.key === 'Escape') {
      setOpen(false)
      setTitle('')
    }
  }

  if (!open) {
    return (
      <button
        type="button"
        onClick={handleOpen}
        className="flex items-center gap-1.5 text-[11px] transition-colors hover:opacity-80"
        style={{ color: 'var(--text-secondary)' }}
      >
        <span className="text-base leading-none" style={{ color: 'var(--accent)' }}>+</span>
        New Issue
      </button>
    )
  }

  return (
    <form
      onSubmit={(e) => { void handleSubmit(e) }}
      onKeyDown={handleKeyDown}
      className="flex items-center gap-2 rounded-md px-3 py-2"
      style={{ background: 'color-mix(in srgb, var(--accent) 5%, var(--mic-bg))', border: '1px solid color-mix(in srgb, var(--accent) 25%, transparent)' }}
    >
      <input
        ref={inputRef}
        type="text"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        placeholder="Issue title…"
        className="flex-1 bg-transparent text-xs outline-none min-w-0"
        style={{ color: 'var(--text-primary)' }}
        aria-label="New issue title"
      />
      <Select value={priority} onValueChange={setPriority}>
        <SelectTrigger
          className="h-6 w-[60px] px-1.5 py-0 text-[10px] font-bold focus:ring-0 focus:ring-offset-0"
          style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)', color: 'var(--text-secondary)' }}
        >
          <SelectValue placeholder="Pri" />
        </SelectTrigger>
        <SelectContent style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}>
          {PRIORITY_OPTIONS.map((opt) => (
            <SelectItem key={opt.value} value={opt.value} className="text-xs" style={{ color: 'var(--text-primary)' }}>
              {opt.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <Button type="submit" size="sm" className="h-6 px-2.5 text-[11px]" disabled={!title.trim() || saving}>
        {saving ? '…' : 'Create'}
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="sm"
        className="h-6 px-2 text-[11px]"
        style={{ color: 'var(--text-secondary)' }}
        onClick={() => { setOpen(false); setTitle('') }}
      >
        Cancel
      </Button>
    </form>
  )
}

// ---------------------------------------------------------------------------
// IssueCard (Kanban) — draggable
// ---------------------------------------------------------------------------

interface IssueCardProps {
  issue: Issue
  selected: boolean
  onSelect: (issue: Issue) => void
  onDragStart: (issueId: string) => void
}

function IssueCard({ issue, selected, onSelect, onDragStart }: IssueCardProps) {
  const labels = parseLabels(issue.labels)

  return (
    <button
      type="button"
      draggable
      onDragStart={(e) => {
        e.dataTransfer.setData('issueId', issue.id)
        e.dataTransfer.effectAllowed = 'move'
        onDragStart(issue.id)
      }}
      onClick={() => onSelect(issue)}
      className="w-full text-left flex flex-col gap-1.5 rounded-md px-2.5 py-2.5 transition-all cursor-grab active:cursor-grabbing"
      style={{
        background: selected
          ? 'color-mix(in srgb, var(--accent) 10%, var(--mic-bg))'
          : 'color-mix(in srgb, var(--text-secondary) 4%, var(--mic-bg))',
        border: `1px solid ${selected ? 'color-mix(in srgb, var(--accent) 60%, transparent)' : 'var(--pill-border)'}`,
      }}
      aria-pressed={selected}
    >
      {/* Header row: ID + priority */}
      <div className="flex items-center justify-between gap-1">
        <IssueIdBadge issue={issue} />
        <PriorityBadge priority={issue.priority} />
      </div>

      {/* Title */}
      <p className="text-xs font-medium leading-snug text-left" style={{ color: 'var(--text-primary)' }}>
        {issue.title}
      </p>

      {/* Labels + assignee */}
      {(labels.length > 0 || issue.assignee) && (
        <div className="flex flex-wrap items-center gap-1 pt-0.5">
          {labels.map((label: string) => (
            <span
              key={label}
              className="rounded px-1.5 py-0 text-[9px]"
              style={{
                background: 'color-mix(in srgb, var(--accent) 8%, transparent)',
                color: 'var(--text-secondary)',
              }}
            >
              {label}
            </span>
          ))}
          {issue.assignee && (
            <span className="text-[9px] ml-auto" style={{ color: 'var(--text-secondary)' }}>
              {issue.assignee}
            </span>
          )}
        </div>
      )}
    </button>
  )
}

// ---------------------------------------------------------------------------
// KanbanColumn — drop zone
// ---------------------------------------------------------------------------

interface KanbanColumnProps {
  col: { key: ColumnKey; label: string }
  issues: Issue[]
  selectedId: string | null
  onSelect: (issue: Issue) => void
  onCardDragStart: (issueId: string) => void
  onDrop: (targetStatus: ColumnKey, issueId: string) => void
}

function KanbanColumn({ col, issues, selectedId, onSelect, onCardDragStart, onDrop }: KanbanColumnProps) {
  const [isDragOver, setIsDragOver] = useState(false)

  const sorted = [...issues].sort((a: Issue, b: Issue) => {
    return (PRIORITY_ORDER[a.priority || ''] ?? 99) - (PRIORITY_ORDER[b.priority || ''] ?? 99)
  })

  const dotColor = STATUS_CONFIG[col.key]?.dot ?? 'var(--text-secondary)'

  return (
    <div
      className="flex flex-col gap-2 min-w-0 transition-colors rounded-lg"
      onDragOver={(e) => { e.preventDefault(); setIsDragOver(true) }}
      onDragLeave={() => setIsDragOver(false)}
      onDrop={(e) => {
        e.preventDefault()
        setIsDragOver(false)
        const issueId = e.dataTransfer.getData('issueId')
        if (issueId) onDrop(col.key, issueId)
      }}
      style={{
        background: isDragOver ? 'color-mix(in srgb, var(--accent) 5%, transparent)' : undefined,
        outline: isDragOver ? '2px dashed color-mix(in srgb, var(--accent) 40%, transparent)' : undefined,
        outlineOffset: '-2px',
        borderRadius: '8px',
        padding: isDragOver ? '4px' : undefined,
      }}
    >
      {/* Column header */}
      <div className="flex items-center gap-2 px-1">
        <span
          className="inline-block rounded-full"
          style={{ width: '7px', height: '7px', background: dotColor, flexShrink: 0 }}
        />
        <span className="text-xs font-semibold" style={{ color: 'var(--text-primary)' }}>
          {col.label}
        </span>
        <span
          className="rounded-full px-1.5 py-0 text-[10px] font-medium"
          style={{
            background: 'color-mix(in srgb, var(--text-secondary) 12%, transparent)',
            color: 'var(--text-secondary)',
          }}
        >
          {issues.length}
        </span>
      </div>

      {/* Cards */}
      <div
        className="flex flex-col gap-1.5"
        style={{ minHeight: '80px', maxHeight: '60vh', overflowY: 'auto' }}
      >
        {sorted.length === 0 ? (
          <p className="py-6 text-center text-[10px]" style={{ color: 'var(--text-secondary)', opacity: 0.5 }}>
            Drop here
          </p>
        ) : (
          sorted.map((issue: Issue) => (
            <IssueCard
              key={issue.id}
              issue={issue}
              selected={selectedId === issue.id}
              onSelect={onSelect}
              onDragStart={onCardDragStart}
            />
          ))
        )}
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// DetailPanel
// ---------------------------------------------------------------------------

function DetailPanel({ issue, onClose }: { issue: Issue; onClose: () => void }) {
  const labels = parseLabels(issue.labels)

  return (
    <div
      className="flex flex-col gap-3 rounded-lg p-4"
      style={{ background: 'var(--mic-bg)', border: '1px solid var(--pill-border)' }}
    >
      <div className="flex items-center justify-between gap-2">
        <div className="flex flex-col gap-1">
          <IssueIdBadge issue={issue} />
          <h3 className="text-sm font-semibold leading-snug" style={{ color: 'var(--text-primary)' }}>
            {issue.title}
          </h3>
        </div>
        <button
          type="button"
          onClick={onClose}
          className="shrink-0 rounded p-1 text-xs leading-none transition-opacity hover:opacity-80"
          style={{ color: 'var(--text-secondary)' }}
          aria-label="Close detail panel"
        >
          ✕
        </button>
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <StatusDot status={issue.status} />
        <PriorityBadge priority={issue.priority} />
        {issue.assignee && (
          <span
            className="rounded px-1.5 py-0.5 text-[10px]"
            style={{
              background: 'color-mix(in srgb, var(--accent) 8%, transparent)',
              color: 'var(--text-secondary)',
            }}
          >
            {issue.assignee}
          </span>
        )}
      </div>

      {labels.length > 0 && (
        <div className="flex flex-wrap gap-1">
          {labels.map((label: string) => (
            <span
              key={label}
              className="rounded px-1.5 py-0.5 text-[9px]"
              style={{
                background: 'color-mix(in srgb, var(--accent) 8%, transparent)',
                color: 'var(--text-secondary)',
              }}
            >
              {label}
            </span>
          ))}
        </div>
      )}

      {issue.description && (
        <p
          className="text-xs leading-relaxed whitespace-pre-wrap rounded-md p-2"
          style={{ background: 'color-mix(in srgb, var(--text-secondary) 5%, transparent)', color: 'var(--text-secondary)' }}
        >
          {issue.description}
        </p>
      )}

      <div
        className="flex flex-col gap-0.5 pt-2 border-t"
        style={{ borderColor: 'var(--pill-border)' }}
      >
        <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
          Created {new Date(issue.created_at).toLocaleString()}
        </span>
        <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
          Updated {new Date(issue.updated_at).toLocaleString()}
        </span>
        {issue.parent_id && (
          <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
            Parent: {issue.parent_id}
          </span>
        )}
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// FilterBar
// ---------------------------------------------------------------------------

interface FilterBarProps {
  search: string
  onSearch: (q: string) => void
  activePriorities: Set<string>
  onTogglePriority: (p: string) => void
  activeStatuses: Set<string>
  onToggleStatus: (s: string) => void
  allLabels: string[]
  activeLabels: Set<string>
  onToggleLabel: (l: string) => void
  onClear: () => void
}

function FilterBar({
  search,
  onSearch,
  activePriorities,
  onTogglePriority,
  activeStatuses,
  onToggleStatus,
  allLabels,
  activeLabels,
  onToggleLabel,
  onClear,
}: FilterBarProps) {
  const hasFilter =
    activePriorities.size > 0 ||
    activeLabels.size > 0 ||
    activeStatuses.size > 0 ||
    search.trim().length > 0

  return (
    <div className="flex flex-col gap-2">
      {/* Search */}
      <div className="relative">
        <span
          className="absolute left-2.5 top-1/2 -translate-y-1/2 text-[12px] pointer-events-none"
          style={{ color: 'var(--text-secondary)' }}
          aria-hidden
        >
          ⌕
        </span>
        <input
          type="search"
          value={search}
          onChange={(e) => onSearch(e.target.value)}
          placeholder="Search issues…"
          className="rounded-md pl-7 pr-2.5 py-1.5 text-xs w-full outline-none transition-colors"
          style={{
            background: 'var(--mic-bg)',
            border: '1px solid var(--pill-border)',
            color: 'var(--text-primary)',
          }}
          aria-label="Search issues"
        />
      </div>

      {/* Filter chips */}
      <div className="flex flex-wrap items-center gap-1.5">
        {/* Priority chips */}
        {ALL_PRIORITIES.map((p) => {
          const cfg = PRIORITY_CONFIG[p]
          const active = activePriorities.has(p)
          return (
            <button
              key={p}
              type="button"
              onClick={() => onTogglePriority(p)}
              className="rounded px-1.5 py-0.5 text-[10px] font-bold transition-all"
              style={{
                background: active ? cfg.bg : 'color-mix(in srgb, var(--text-secondary) 8%, transparent)',
                color: active ? cfg.text : 'var(--text-secondary)',
                border: `1px solid ${active ? cfg.border : 'transparent'}`,
                opacity: active ? 1 : 0.55,
              }}
              aria-pressed={active}
            >
              {p}
            </button>
          )
        })}

        <span
          className="w-px h-3 mx-0.5"
          style={{ background: 'var(--pill-border)' }}
          aria-hidden
        />

        {/* Status chips */}
        {ALL_STATUSES.map((s) => {
          const cfg = STATUS_CONFIG[s]
          const active = activeStatuses.has(s)
          return (
            <button
              key={s}
              type="button"
              onClick={() => onToggleStatus(s)}
              className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] transition-all"
              style={{
                background: active
                  ? 'color-mix(in srgb, var(--text-secondary) 15%, transparent)'
                  : 'color-mix(in srgb, var(--text-secondary) 6%, transparent)',
                color: active ? 'var(--text-primary)' : 'var(--text-secondary)',
                border: `1px solid ${active ? 'var(--text-secondary)' : 'transparent'}`,
                opacity: active ? 1 : 0.55,
              }}
              aria-pressed={active}
            >
              <span
                className="inline-block rounded-full"
                style={{ width: '5px', height: '5px', background: cfg?.dot, flexShrink: 0 }}
              />
              {cfg?.label ?? s}
            </button>
          )
        })}

        {/* Label chips */}
        {allLabels.length > 0 && (
          <>
            <span
              className="w-px h-3 mx-0.5"
              style={{ background: 'var(--pill-border)' }}
              aria-hidden
            />
            {allLabels.map((label) => {
              const active = activeLabels.has(label)
              return (
                <button
                  key={label}
                  type="button"
                  onClick={() => onToggleLabel(label)}
                  className="rounded px-1.5 py-0.5 text-[9px] transition-all"
                  style={{
                    background: active
                      ? 'color-mix(in srgb, var(--accent) 15%, transparent)'
                      : 'color-mix(in srgb, var(--text-secondary) 6%, transparent)',
                    color: active ? 'var(--accent)' : 'var(--text-secondary)',
                    border: `1px solid ${active ? 'color-mix(in srgb, var(--accent) 40%, transparent)' : 'transparent'}`,
                    opacity: active ? 1 : 0.55,
                  }}
                  aria-pressed={active}
                >
                  {label}
                </button>
              )
            })}
          </>
        )}

        {hasFilter && (
          <button
            type="button"
            onClick={onClear}
            className="text-[10px] underline ml-1 transition-opacity hover:opacity-80"
            style={{ color: 'var(--text-secondary)' }}
          >
            Clear
          </button>
        )}
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// BulkActionBar
// ---------------------------------------------------------------------------

interface BulkActionBarProps {
  count: number
  onMarkStatus: (status: string) => void
  onClear: () => void
}

function BulkActionBar({ count, onMarkStatus, onClear }: BulkActionBarProps) {
  return (
    <div
      className="flex items-center gap-2 rounded-md px-3 py-2"
      style={{
        background: 'color-mix(in srgb, var(--accent) 8%, var(--mic-bg))',
        border: '1px solid color-mix(in srgb, var(--accent) 25%, transparent)',
      }}
    >
      <span className="text-[11px] font-semibold" style={{ color: 'var(--accent)' }}>
        {count} selected
      </span>
      <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>·</span>
      <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>Mark as:</span>
      {ALL_STATUSES.map((s) => (
        <button
          key={s}
          type="button"
          onClick={() => onMarkStatus(s)}
          className="flex items-center gap-1 rounded px-2 py-0.5 text-[10px] transition-colors"
          style={{
            background: 'color-mix(in srgb, var(--text-secondary) 8%, transparent)',
            color: 'var(--text-secondary)',
            border: '1px solid var(--pill-border)',
          }}
        >
          <span
            className="inline-block rounded-full"
            style={{ width: '5px', height: '5px', background: STATUS_CONFIG[s]?.dot, flexShrink: 0 }}
          />
          {STATUS_CONFIG[s]?.label ?? s}
        </button>
      ))}
      <button
        type="button"
        onClick={onClear}
        className="ml-auto text-[10px] underline transition-opacity hover:opacity-70"
        style={{ color: 'var(--text-secondary)' }}
      >
        Deselect all
      </button>
    </div>
  )
}

// ---------------------------------------------------------------------------
// SortIndicator
// ---------------------------------------------------------------------------

function SortIndicator({ active, dir }: { active: boolean; dir: SortDir }) {
  if (!active) return <span style={{ color: 'var(--text-secondary)', opacity: 0.3 }}>↕</span>
  return <span style={{ color: 'var(--accent)' }}>{dir === 'asc' ? '↑' : '↓'}</span>
}

// ---------------------------------------------------------------------------
// IssueTable
// ---------------------------------------------------------------------------

interface IssueTableProps {
  issues: Issue[]
  selectedId: string | null
  onRowClick: (issue: Issue) => void
  onUpdateIssue: (id: string, patch: Partial<Issue>) => void
  selectedRows: Set<string>
  onToggleRow: (id: string) => void
  onToggleAll: (ids: string[]) => void
  sortKey: SortKey
  sortDir: SortDir
  onSort: (key: SortKey) => void
  onCreate: (title: string, priority: string) => Promise<void>
}

function IssueTable({
  issues,
  selectedId,
  onRowClick,
  onUpdateIssue,
  selectedRows,
  onToggleRow,
  onToggleAll,
  sortKey,
  sortDir,
  onSort,
  onCreate,
}: IssueTableProps) {
  const allSelected = issues.length > 0 && issues.every((i) => selectedRows.has(i.id))
  const someSelected = issues.some((i) => selectedRows.has(i.id))

  function SortableHeader({ col, label }: { col: SortKey; label: string }) {
    return (
      <th
        className="px-3 py-2 text-left"
        style={{ borderBottom: '1px solid var(--pill-border)', userSelect: 'none' }}
      >
        <button
          type="button"
          onClick={() => onSort(col)}
          className="inline-flex items-center gap-1 text-[10px] font-semibold uppercase tracking-wide transition-opacity hover:opacity-80"
          style={{ color: 'var(--text-secondary)' }}
        >
          {label} <SortIndicator active={sortKey === col} dir={sortDir} />
        </button>
      </th>
    )
  }

  return (
    <div className="flex flex-col">
      {/* Inline create form footer */}
      <div
        className="px-3 py-2"
        style={{ borderTop: '1px solid color-mix(in srgb, var(--pill-border) 60%, transparent)' }}
      >
        <InlineCreateForm onCreate={onCreate} />
      </div>

      <div style={{ overflowX: 'auto' }}>
        <table className="w-full border-collapse text-xs" style={{ tableLayout: 'fixed' }}>
          <colgroup>
            <col style={{ width: '32px' }} />
            <col style={{ width: '72px' }} />
            <col style={{ width: '72px' }} />
            <col />
            <col style={{ width: '120px' }} />
            <col style={{ width: '100px' }} />
            <col style={{ width: '86px' }} />
          </colgroup>
          <thead>
            <tr style={{ background: 'color-mix(in srgb, var(--text-secondary) 4%, transparent)' }}>
              {/* Checkbox */}
              <th className="px-3 py-2" style={{ borderBottom: '1px solid var(--pill-border)' }}>
                <input
                  type="checkbox"
                  checked={allSelected}
                  ref={(el) => { if (el) el.indeterminate = someSelected && !allSelected }}
                  onChange={() => onToggleAll(issues.map((i) => i.id))}
                  aria-label="Select all"
                  className="cursor-pointer"
                  style={{ accentColor: 'var(--accent)' }}
                />
              </th>
              {/* ID */}
              <th
                className="px-3 py-2 text-left"
                style={{ borderBottom: '1px solid var(--pill-border)' }}
              >
                <span className="text-[10px] font-semibold uppercase tracking-wide" style={{ color: 'var(--text-secondary)' }}>
                  ID
                </span>
              </th>
              <SortableHeader col="priority" label="Priority" />
              <SortableHeader col="title" label="Title" />
              <SortableHeader col="status" label="Status" />
              {/* Labels */}
              <th
                className="px-3 py-2 text-left"
                style={{ borderBottom: '1px solid var(--pill-border)' }}
              >
                <span className="text-[10px] font-semibold uppercase tracking-wide" style={{ color: 'var(--text-secondary)' }}>
                  Labels
                </span>
              </th>
              <SortableHeader col="updated_at" label="Updated" />
            </tr>
          </thead>
          <tbody>
            {issues.length === 0 ? (
              <tr>
                <td
                  colSpan={7}
                  className="py-8 text-center text-xs"
                  style={{ color: 'var(--text-secondary)' }}
                >
                  No issues match your filters
                </td>
              </tr>
            ) : (
              issues.map((issue) => {
                const labels = parseLabels(issue.labels)
                const isRowSelected = selectedId === issue.id
                const isBulkSelected = selectedRows.has(issue.id)

                return (
                  <tr
                    key={issue.id}
                    onClick={() => onRowClick(issue)}
                    className="cursor-pointer transition-colors"
                    style={{
                      background: isRowSelected
                        ? 'color-mix(in srgb, var(--accent) 7%, var(--mic-bg))'
                        : isBulkSelected
                          ? 'color-mix(in srgb, var(--text-secondary) 5%, var(--mic-bg))'
                          : undefined,
                      borderBottom: '1px solid color-mix(in srgb, var(--pill-border) 45%, transparent)',
                    }}
                    aria-selected={isRowSelected}
                  >
                    {/* Checkbox */}
                    <td
                      className="px-3 py-2"
                      onClick={(e) => { e.stopPropagation(); onToggleRow(issue.id) }}
                    >
                      <input
                        type="checkbox"
                        checked={isBulkSelected}
                        onChange={() => onToggleRow(issue.id)}
                        aria-label={`Select ${formatId(issue)}`}
                        className="cursor-pointer"
                        style={{ accentColor: 'var(--accent)' }}
                      />
                    </td>

                    {/* ID */}
                    <td className="px-3 py-2" onClick={(e) => e.stopPropagation()}>
                      <IssueIdBadge issue={issue} />
                    </td>

                    {/* Priority */}
                    <td className="px-2 py-1.5" onClick={(e) => e.stopPropagation()}>
                      <InlinePrioritySelect
                        value={issue.priority ?? ''}
                        onValueChange={(val) => onUpdateIssue(issue.id, { priority: val || null })}
                      />
                    </td>

                    {/* Title */}
                    <td className="px-3 py-2">
                      <span
                        className="text-xs font-medium leading-snug truncate block"
                        style={{ color: 'var(--text-primary)' }}
                        title={issue.title}
                      >
                        {issue.title}
                      </span>
                    </td>

                    {/* Status */}
                    <td className="px-2 py-1.5" onClick={(e) => e.stopPropagation()}>
                      <InlineStatusSelect
                        value={issue.status}
                        onValueChange={(val) => onUpdateIssue(issue.id, { status: val })}
                      />
                    </td>

                    {/* Labels */}
                    <td className="px-3 py-2">
                      <div className="flex flex-wrap gap-0.5">
                        {labels.slice(0, 2).map((label) => (
                          <span
                            key={label}
                            className="rounded px-1 py-0 text-[9px]"
                            style={{
                              background: 'color-mix(in srgb, var(--accent) 8%, transparent)',
                              color: 'var(--text-secondary)',
                            }}
                          >
                            {label}
                          </span>
                        ))}
                        {labels.length > 2 && (
                          <span className="text-[9px]" style={{ color: 'var(--text-secondary)' }}>
                            +{labels.length - 2}
                          </span>
                        )}
                      </div>
                    </td>

                    {/* Updated */}
                    <td className="px-3 py-2">
                      <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                        {new Date(issue.updated_at).toLocaleDateString()}
                      </span>
                    </td>
                  </tr>
                )
              })
            )}
          </tbody>
        </table>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Main view
// ---------------------------------------------------------------------------

export default function TrackerView(_props: PluginViewProps) {
  const [issues, setIssues] = useState<Issue[]>([])
  const [status, setStatus] = useState<TrackerStatus | null>(null)
  const [loading, setLoading] = useState(true)
  const [feedback, setFeedback] = useState('')
  const [selectedIssue, setSelectedIssue] = useState<Issue | null>(null)
  const [viewMode, setViewMode] = useState<ViewMode>('list')

  // Filters
  const [search, setSearch] = useState('')
  const [activePriorities, setActivePriorities] = useState<Set<string>>(new Set())
  const [activeStatuses, setActiveStatuses] = useState<Set<string>>(new Set())
  const [activeLabels, setActiveLabels] = useState<Set<string>>(new Set())

  // Sort (list view)
  const [sortKey, setSortKey] = useState<SortKey>('priority')
  const [sortDir, setSortDir] = useState<SortDir>('asc')

  // Bulk select
  const [selectedRows, setSelectedRows] = useState<Set<string>>(new Set())

  const feedbackTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  function showFeedback(msg: string) {
    setFeedback(msg)
    if (feedbackTimerRef.current) clearTimeout(feedbackTimerRef.current)
    feedbackTimerRef.current = setTimeout(() => setFeedback(''), 3000)
  }

  const load = useCallback(async () => {
    try {
      setLoading(true)
      const [rawItems, rawStatus] = await Promise.allSettled([
        queryPluginItems(PLUGIN_NAME),
        queryPluginStatus(PLUGIN_NAME),
      ])

      if (rawItems.status === 'fulfilled') {
        const val = rawItems.value as unknown
        if (Array.isArray(val)) {
          setIssues(val as Issue[])
        } else if (val && typeof val === 'object' && 'items' in val) {
          setIssues((val as { items: Issue[] }).items || [])
        }
      }

      if (rawStatus.status === 'fulfilled') {
        setStatus(rawStatus.value as unknown as TrackerStatus)
      }
    } catch {
      showFeedback('Failed to load tracker data')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void load()
    const interval = setInterval(() => { void load() }, 30_000)
    return () => clearInterval(interval)
  }, [load])

  const allLabels = useMemo(
    () => Array.from(new Set(issues.flatMap((i) => parseLabels(i.labels)))).sort(),
    [issues],
  )

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase()
    return issues.filter((i) => {
      if (activePriorities.size > 0 && !activePriorities.has(i.priority || '')) return false
      if (activeStatuses.size > 0 && !activeStatuses.has(i.status as ColumnKey)) return false
      if (activeLabels.size > 0) {
        const issueLabelSet = new Set(parseLabels(i.labels))
        for (const l of activeLabels) {
          if (!issueLabelSet.has(l)) return false
        }
      }
      if (q) {
        const haystack = `${formatId(i)} ${i.id} ${i.title} ${i.labels ?? ''} ${i.assignee ?? ''}`.toLowerCase()
        if (!haystack.includes(q)) return false
      }
      return true
    })
  }, [issues, activePriorities, activeStatuses, activeLabels, search])

  const grouped = useMemo<Record<ColumnKey, Issue[]>>(
    () => ({
      open:        filtered.filter((i) => i.status === 'open' || i.status === 'backlog' || i.status === 'todo'),
      in_progress: filtered.filter((i) => i.status === 'in_progress' || i.status === 'in-progress'),
      done:        filtered.filter((i) => i.status === 'done'),
      cancelled:   filtered.filter((i) => i.status === 'cancelled'),
    }),
    [filtered],
  )

  const sortedFiltered = useMemo(
    () => sortIssues(filtered, sortKey, sortDir),
    [filtered, sortKey, sortDir],
  )

  function handleSort(key: SortKey) {
    if (sortKey === key) {
      setSortDir((d) => (d === 'asc' ? 'desc' : 'asc'))
    } else {
      setSortKey(key)
      setSortDir('asc')
    }
  }

  function togglePriority(p: string) {
    setActivePriorities((prev) => { const n = new Set(prev); n.has(p) ? n.delete(p) : n.add(p); return n })
  }

  function toggleStatus(s: string) {
    setActiveStatuses((prev) => { const n = new Set(prev); n.has(s) ? n.delete(s) : n.add(s); return n })
  }

  function toggleLabel(l: string) {
    setActiveLabels((prev) => { const n = new Set(prev); n.has(l) ? n.delete(l) : n.add(l); return n })
  }

  function clearFilters() {
    setActivePriorities(new Set())
    setActiveStatuses(new Set())
    setActiveLabels(new Set())
    setSearch('')
  }

  function handleSelect(issue: Issue) {
    setSelectedIssue((prev) => (prev?.id === issue.id ? null : issue))
  }

  // PATTERN: Optimistic-first — update local state immediately, fire backend call,
  //          revert on failure. Verb format: "update-<field>:<value>"
  async function handleUpdateIssue(id: string, patch: Partial<Issue>) {
    setIssues((prev) => prev.map((i) => (i.id === id ? { ...i, ...patch } : i)))
    setSelectedIssue((prev) => (prev?.id === id ? { ...prev, ...patch } : prev))

    try {
      for (const [field, value] of Object.entries(patch)) {
        await pluginAction(PLUGIN_NAME, `update-${field}:${value ?? ''}`, id)
      }
    } catch {
      showFeedback('Failed to save — reverting')
      await load()
    }
  }

  // PATTERN: Inline create via pluginAction verb "create:<title>[:priority=<p>]"
  //          Backend extension needed: tracker query action create:<title> --json
  async function handleCreateIssue(title: string, priority: string) {
    const verb = priority ? `create:${title}:priority=${priority}` : `create:${title}`
    try {
      await pluginAction(PLUGIN_NAME, verb, '')
      showFeedback('Issue created')
      await load()
    } catch {
      // Backend may not yet support this action verb — reload to reflect actual state
      showFeedback('Create may not be supported via UI yet — use CLI: tracker create "title"')
      await load()
    }
  }

  // Kanban drag: state tracked via dataTransfer (HTML5 DnD)
  async function handleKanbanDrop(targetStatus: ColumnKey, issueId: string) {
    const issue = issues.find((i) => i.id === issueId)
    if (!issue || issue.status === targetStatus) return
    await handleUpdateIssue(issueId, { status: targetStatus })
  }

  function toggleRow(id: string) {
    setSelectedRows((prev) => { const n = new Set(prev); n.has(id) ? n.delete(id) : n.add(id); return n })
  }

  function toggleAll(ids: string[]) {
    const allIn = ids.every((id) => selectedRows.has(id))
    setSelectedRows((prev) => {
      const n = new Set(prev)
      allIn ? ids.forEach((id) => n.delete(id)) : ids.forEach((id) => n.add(id))
      return n
    })
  }

  async function bulkMarkStatus(newStatus: string) {
    const ids = Array.from(selectedRows)
    setIssues((prev) => prev.map((i) => (selectedRows.has(i.id) ? { ...i, status: newStatus } : i)))
    setSelectedRows(new Set())

    try {
      await Promise.all(ids.map((id) => pluginAction(PLUGIN_NAME, `update-status:${newStatus}`, id)))
      showFeedback(`${ids.length} issue${ids.length > 1 ? 's' : ''} marked ${STATUS_CONFIG[newStatus]?.label ?? newStatus}`)
    } catch {
      showFeedback('Bulk update failed — reverting')
      await load()
    }
  }

  if (loading && issues.length === 0) {
    return (
      <div className="flex items-center justify-center py-12">
        <p className="text-sm" style={{ color: 'var(--text-secondary)' }}>Loading tracker…</p>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-3 p-4">
      {/* Feedback toast */}
      {feedback && (
        <p
          className="rounded-md px-3 py-1.5 text-xs"
          style={{
            background: feedback.includes('Failed') || feedback.includes('not supported')
              ? 'color-mix(in srgb, var(--color-error) 10%, transparent)'
              : 'color-mix(in srgb, var(--color-success) 10%, transparent)',
            color: feedback.includes('Failed') || feedback.includes('not supported')
              ? 'var(--color-error)'
              : 'var(--color-success)',
            border: '1px solid currentColor',
            borderOpacity: '0.3',
          }}
          role="status"
          aria-live="polite"
        >
          {feedback}
        </p>
      )}

      {/* Toolbar */}
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-3">
          <span className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
            {status?.count ?? issues.length} issues
          </span>
          <span className="text-[11px]" style={{ color: 'var(--text-secondary)' }}>
            {grouped.open.length} open · {grouped.in_progress.length} active · {grouped.done.length} done
          </span>
        </div>

        <div className="flex items-center gap-1.5">
          {/* View toggle */}
          <div
            className="flex rounded-md overflow-hidden"
            style={{ border: '1px solid var(--pill-border)' }}
            role="group"
            aria-label="View mode"
          >
            {(['list', 'kanban'] as const).map((mode) => (
              <button
                key={mode}
                type="button"
                onClick={() => setViewMode(mode)}
                className="px-2.5 py-1 text-[11px] font-medium capitalize transition-colors"
                style={{
                  background: viewMode === mode
                    ? 'color-mix(in srgb, var(--accent) 12%, var(--mic-bg))'
                    : 'var(--mic-bg)',
                  color: viewMode === mode ? 'var(--accent)' : 'var(--text-secondary)',
                  borderRight: mode === 'list' ? '1px solid var(--pill-border)' : undefined,
                }}
                aria-pressed={viewMode === mode}
              >
                {mode}
              </button>
            ))}
          </div>

          <Button
            size="sm"
            variant="outline"
            onClick={() => { void load() }}
            disabled={loading}
            className="h-7 px-2.5 text-[11px]"
          >
            {loading ? '…' : 'Refresh'}
          </Button>
        </div>
      </div>

      {/* Filter bar */}
      <FilterBar
        search={search}
        onSearch={setSearch}
        activePriorities={activePriorities}
        onTogglePriority={togglePriority}
        activeStatuses={activeStatuses}
        onToggleStatus={toggleStatus}
        allLabels={allLabels}
        activeLabels={activeLabels}
        onToggleLabel={toggleLabel}
        onClear={clearFilters}
      />

      {/* Bulk action bar */}
      {selectedRows.size > 0 && (
        <BulkActionBar
          count={selectedRows.size}
          onMarkStatus={(s) => { void bulkMarkStatus(s) }}
          onClear={() => setSelectedRows(new Set())}
        />
      )}

      {/* Main content */}
      {viewMode === 'list' ? (
        <div className="flex flex-col gap-3">
          <Card style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)', overflow: 'hidden' }}>
            <IssueTable
              issues={sortedFiltered}
              selectedId={selectedIssue?.id ?? null}
              onRowClick={handleSelect}
              onUpdateIssue={(id, patch) => { void handleUpdateIssue(id, patch) }}
              selectedRows={selectedRows}
              onToggleRow={toggleRow}
              onToggleAll={toggleAll}
              sortKey={sortKey}
              sortDir={sortDir}
              onSort={handleSort}
              onCreate={handleCreateIssue}
            />
          </Card>

          {selectedIssue && (
            <DetailPanel issue={selectedIssue} onClose={() => setSelectedIssue(null)} />
          )}
        </div>
      ) : (
        <div className="flex flex-col gap-3">
          <Card style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)', overflowX: 'auto' }}>
            <div className="flex gap-4 p-4" style={{ minWidth: 'max-content' }}>
              {COLUMNS.map((col) => (
                <div key={col.key} style={{ width: '230px', flexShrink: 0 }}>
                  <KanbanColumn
                    col={col}
                    issues={grouped[col.key]}
                    selectedId={selectedIssue?.id ?? null}
                    onSelect={handleSelect}
                    onCardDragStart={() => { /* dataTransfer handles state */ }}
                    onDrop={(targetStatus, issueId) => { void handleKanbanDrop(targetStatus, issueId) }}
                  />
                </div>
              ))}
            </div>
          </Card>

          {/* Inline create for kanban */}
          <div className="px-1">
            <InlineCreateForm onCreate={handleCreateIssue} />
          </div>

          {selectedIssue && (
            <DetailPanel issue={selectedIssue} onClose={() => setSelectedIssue(null)} />
          )}
        </div>
      )}
    </div>
  )
}
