import { useState, useEffect, useCallback, useMemo, useRef } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import type { PluginViewProps } from '@/types'
import { queryPluginStatus, queryPluginItems, pluginAction } from '@/lib/wails'

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function cx(...classes: (string | undefined | null | false)[]): string {
  return classes.filter(Boolean).join(' ')
}

// ---------------------------------------------------------------------------
// Domain types — mirrors scheduler query output (Go JSON field names)
// ---------------------------------------------------------------------------

interface SchedulerStatus {
  daemon_running: boolean
  job_count: number
  enabled_count: number
  next_run_at?: string | null
}

interface SchedulerJob {
  id: number
  name: string
  schedule: string
  enabled: boolean
  last_run?: string | null
  next_run?: string | null
  last_exit_code?: number | null
}

interface RunRecord {
  ts: string
  exit_code: number
  stdout: string
  stderr: string
}

type FilterMode = 'all' | 'enabled' | 'disabled'
type SortKey = 'name' | 'schedule' | 'next_run' | 'last_status'
type SortDir = 'asc' | 'desc'

// ---------------------------------------------------------------------------
// Formatters
// ---------------------------------------------------------------------------

function cronToHuman(expr: string): string {
  const parts = expr.trim().split(/\s+/)
  if (parts.length !== 5) return expr
  const [min, hour, , , dow] = parts
  const dayNames = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat']
  let time = ''
  if (min !== '*' && hour !== '*') {
    const h = parseInt(hour, 10)
    const m = parseInt(min, 10)
    const ampm = h >= 12 ? 'PM' : 'AM'
    const hh = h % 12 || 12
    time = `${hh}:${m.toString().padStart(2, '0')} ${ampm}`
  }
  if (dow !== '*') {
    const dayIdx = parseInt(dow, 10)
    const dayLabel = isNaN(dayIdx) ? dow : (dayNames[dayIdx] ?? dow)
    return time ? `${dayLabel}s at ${time}` : `Every ${dayLabel}`
  }
  if (time) return `Daily at ${time}`
  if (min === '*' && hour === '*') return 'Every minute'
  if (min !== '*' && hour === '*') return `Every hour at :${min.padStart(2, '0')}`
  return expr
}

function formatCountdown(isoDate: string | null | undefined): string {
  if (!isoDate) return '—'
  const diff = new Date(isoDate).getTime() - Date.now()
  if (diff <= 0) return 'now'
  const s = Math.floor(diff / 1000)
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m`
  const h = Math.floor(m / 60)
  const rm = m % 60
  if (h < 24) return rm > 0 ? `${h}h ${rm}m` : `${h}h`
  const d = Math.floor(h / 24)
  const rh = h % 24
  return rh > 0 ? `${d}d ${rh}h` : `${d}d`
}

function formatRelative(isoDate: string | null | undefined): string {
  if (!isoDate) return 'never'
  const diff = Date.now() - new Date(isoDate).getTime()
  const s = Math.floor(diff / 1000)
  if (s < 60) return `${s}s ago`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m ago`
  const h = Math.floor(m / 60)
  if (h < 24) return `${h}h ago`
  return `${Math.floor(h / 24)}d ago`
}

function sortJobs(jobs: SchedulerJob[], key: SortKey, dir: SortDir): SchedulerJob[] {
  const sorted = [...jobs].sort((a, b) => {
    let cmp = 0
    if (key === 'name') {
      cmp = a.name.localeCompare(b.name)
    } else if (key === 'schedule') {
      cmp = a.schedule.localeCompare(b.schedule)
    } else if (key === 'next_run') {
      const at = a.next_run ?? ''
      const bt = b.next_run ?? ''
      cmp = at.localeCompare(bt)
    } else if (key === 'last_status') {
      const ae = a.last_exit_code ?? -1
      const be = b.last_exit_code ?? -1
      cmp = ae - be
    }
    return dir === 'asc' ? cmp : -cmp
  })
  return sorted
}

// ---------------------------------------------------------------------------
// DaemonBanner
// ---------------------------------------------------------------------------

function DaemonBanner({ status, onRefresh }: { status: SchedulerStatus; onRefresh: () => void }) {
  const isRunning = status.daemon_running
  return (
    <div
      className="flex items-center gap-3 rounded-lg px-4 py-3"
      style={{
        background: isRunning
          ? 'color-mix(in srgb, var(--color-success) 10%, transparent)'
          : 'color-mix(in srgb, var(--color-error) 10%, transparent)',
        border: `1px solid ${isRunning
          ? 'color-mix(in srgb, var(--color-success) 25%, transparent)'
          : 'color-mix(in srgb, var(--color-error) 25%, transparent)'}`,
      }}
    >
      {/* Pulsing status dot */}
      <span className="relative flex-shrink-0 h-2 w-2">
        {isRunning && (
          <span
            className="absolute inline-flex h-full w-full rounded-full animate-ping opacity-75"
            style={{ background: 'var(--color-success)' }}
            aria-hidden="true"
          />
        )}
        <span
          className="relative inline-flex h-2 w-2 rounded-full"
          style={{ background: isRunning ? 'var(--color-success)' : 'var(--color-error)' }}
          aria-hidden="true"
        />
      </span>

      <div className="flex flex-col gap-0.5 min-w-0">
        <span
          className="text-sm font-semibold"
          style={{ color: isRunning ? 'var(--color-success)' : 'var(--color-error)' }}
        >
          Daemon {isRunning ? 'running' : 'stopped'}
        </span>
        {isRunning && status.next_run_at && (
          <span className="text-xs" style={{ color: 'var(--text-secondary)' }}>
            Next run in {formatCountdown(status.next_run_at)}
          </span>
        )}
      </div>

      <div className="ml-auto flex items-center gap-3">
        <span className="text-xs" style={{ color: 'var(--text-secondary)' }}>
          {status.enabled_count}/{status.job_count} enabled
        </span>
        <Button
          size="sm"
          variant="outline"
          className="h-6 px-2 text-[10px]"
          onClick={onRefresh}
          aria-label="Refresh scheduler data"
        >
          Refresh
        </Button>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// FilterBar
// ---------------------------------------------------------------------------

interface FilterBarProps {
  search: string
  filter: FilterMode
  sortKey: SortKey
  sortDir: SortDir
  onSearch: (v: string) => void
  onFilter: (v: FilterMode) => void
  onSortKey: (v: SortKey) => void
  onSortDir: (v: SortDir) => void
  onAdd: () => void
}

function FilterBar({ search, filter, sortKey, sortDir, onSearch, onFilter, onSortKey, onSortDir, onAdd }: FilterBarProps) {
  const selectClass = cx(
    'h-7 rounded-md border px-2 text-xs focus:outline-none focus:ring-1',
    'bg-transparent',
  )
  return (
    <div className="flex items-center gap-2 flex-wrap">
      <input
        type="search"
        value={search}
        onChange={e => onSearch(e.target.value)}
        placeholder="Filter jobs…"
        aria-label="Filter jobs by name"
        className="h-7 flex-1 min-w-[120px] rounded-md border px-2 text-xs focus:outline-none focus:ring-1 bg-transparent"
        style={{ borderColor: 'var(--pill-border)', color: 'var(--text-primary)' }}
      />
      <select
        value={filter}
        onChange={e => onFilter(e.target.value as FilterMode)}
        aria-label="Filter by status"
        className={selectClass}
        style={{ borderColor: 'var(--pill-border)', color: 'var(--text-primary)' }}
      >
        <option value="all">All</option>
        <option value="enabled">Enabled</option>
        <option value="disabled">Disabled</option>
      </select>
      <select
        value={sortKey}
        onChange={e => onSortKey(e.target.value as SortKey)}
        aria-label="Sort by"
        className={selectClass}
        style={{ borderColor: 'var(--pill-border)', color: 'var(--text-primary)' }}
      >
        <option value="name">Name</option>
        <option value="schedule">Schedule</option>
        <option value="next_run">Next Run</option>
        <option value="last_status">Last Status</option>
      </select>
      <button
        onClick={() => onSortDir(sortDir === 'asc' ? 'desc' : 'asc')}
        aria-label={`Sort direction: ${sortDir === 'asc' ? 'ascending' : 'descending'}`}
        className="h-7 w-7 rounded-md border flex items-center justify-center text-xs hover:bg-opacity-10"
        style={{ borderColor: 'var(--pill-border)', color: 'var(--text-secondary)' }}
        title={sortDir === 'asc' ? 'Sort ascending' : 'Sort descending'}
      >
        {sortDir === 'asc' ? '↑' : '↓'}
      </button>
      <Button
        size="sm"
        variant="outline"
        className="h-7 px-2 text-xs ml-auto"
        onClick={onAdd}
        aria-label="Add new job"
      >
        + Add Job
      </Button>
    </div>
  )
}

// ---------------------------------------------------------------------------
// JobRow — table row with expandable history
// ---------------------------------------------------------------------------

interface JobRowProps {
  job: SchedulerJob
  history: RunRecord[]
  busy: string | null
  onToggle: (job: SchedulerJob) => void
  onRun: (job: SchedulerJob) => void
  onDelete: (job: SchedulerJob) => void
}

function JobRow({ job, history, busy, onToggle, onRun, onDelete }: JobRowProps) {
  const [expanded, setExpanded] = useState(false)
  const busyToggle = busy === `toggle:${job.id}`
  const busyRun = busy === `run:${job.id}`
  const hasError = job.last_exit_code != null && job.last_exit_code !== 0
  const hasRun = job.last_run != null

  return (
    <>
      <div
        className="grid items-center gap-2 px-3 py-2.5"
        style={{
          borderBottom: '1px solid var(--pill-border)',
          gridTemplateColumns: '1fr 110px 90px 80px auto',
        }}
      >
        {/* Name + schedule */}
        <div className="min-w-0">
          <div className="flex items-center gap-1.5 mb-0.5">
            <span
              className="text-sm font-medium truncate"
              style={{ color: job.enabled ? 'var(--text-primary)' : 'var(--text-secondary)' }}
            >
              {job.name}
            </span>
            {!job.enabled && (
              <Badge variant="outline" className="text-[9px] px-1 py-0 flex-shrink-0">
                off
              </Badge>
            )}
          </div>
          <div className="flex items-center gap-1.5">
            <span className="text-[10px] font-mono" style={{ color: 'var(--text-secondary)' }}>
              {job.schedule}
            </span>
            <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
              · {cronToHuman(job.schedule)}
            </span>
          </div>
        </div>

        {/* Next run */}
        <div className="text-right">
          {job.enabled && job.next_run ? (
            <span className="text-[11px] font-mono tabular-nums" style={{ color: 'var(--text-primary)' }}>
              in {formatCountdown(job.next_run)}
            </span>
          ) : (
            <span className="text-[11px]" style={{ color: 'var(--text-secondary)' }}>—</span>
          )}
        </div>

        {/* Last status */}
        <div className="flex items-center justify-end gap-1">
          {hasRun ? (
            <>
              <Badge
                variant={hasError ? 'destructive' : 'outline'}
                className="text-[9px] px-1.5 py-0"
                style={
                  !hasError
                    ? {
                        color: 'var(--color-success)',
                        borderColor: 'color-mix(in srgb, var(--color-success) 30%, transparent)',
                      }
                    : undefined
                }
              >
                {hasError ? `exit ${job.last_exit_code}` : 'ok'}
              </Badge>
              <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                {formatRelative(job.last_run)}
              </span>
            </>
          ) : (
            <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>never</span>
          )}
        </div>

        {/* History toggle */}
        <div className="flex justify-center">
          {history.length > 0 && (
            <button
              onClick={() => setExpanded(x => !x)}
              aria-expanded={expanded}
              aria-label={`${expanded ? 'Collapse' : 'Expand'} run history for ${job.name}`}
              className="text-[10px] px-1.5 py-0.5 rounded hover:opacity-80"
              style={{ color: 'var(--text-secondary)', background: 'var(--mic-bg)' }}
            >
              {expanded ? '▲' : '▼'} {history.length}
            </button>
          )}
        </div>

        {/* Actions */}
        <div className="flex items-center gap-1 flex-shrink-0">
          <Button
            size="sm"
            variant="outline"
            className="h-6 px-2 text-[10px]"
            disabled={busyToggle}
            onClick={() => onToggle(job)}
            aria-label={job.enabled ? `Disable ${job.name}` : `Enable ${job.name}`}
          >
            {busyToggle ? '…' : job.enabled ? 'Disable' : 'Enable'}
          </Button>
          <Button
            size="sm"
            variant="outline"
            className="h-6 px-2 text-[10px]"
            disabled={busyRun}
            onClick={() => onRun(job)}
            aria-label={`Run ${job.name} now`}
          >
            {busyRun ? '…' : 'Run'}
          </Button>
          <button
            onClick={() => onDelete(job)}
            aria-label={`Delete ${job.name}`}
            className="h-6 w-6 rounded text-[10px] hover:opacity-80 flex items-center justify-center"
            style={{ color: 'var(--color-error)' }}
            title={`Delete ${job.name}`}
          >
            ✕
          </button>
        </div>
      </div>

      {/* Expandable history */}
      {expanded && history.length > 0 && (
        <div
          className="px-3 py-2"
          style={{
            background: 'color-mix(in srgb, var(--mic-bg) 60%, transparent)',
            borderBottom: '1px solid var(--pill-border)',
          }}
        >
          <p className="text-[9px] uppercase tracking-wider font-medium mb-1.5" style={{ color: 'var(--text-secondary)' }}>
            Run History (this session)
          </p>
          <div className="flex flex-col gap-1.5">
            {history.map((r, i) => (
              <div key={i} className="flex items-start gap-2">
                <Badge
                  variant={r.exit_code !== 0 ? 'destructive' : 'outline'}
                  className="text-[9px] px-1 py-0 flex-shrink-0 mt-0.5"
                  style={
                    r.exit_code === 0
                      ? {
                          color: 'var(--color-success)',
                          borderColor: 'color-mix(in srgb, var(--color-success) 30%, transparent)',
                        }
                      : undefined
                  }
                >
                  {r.exit_code !== 0 ? `exit ${r.exit_code}` : 'ok'}
                </Badge>
                <span className="text-[10px] font-mono flex-shrink-0" style={{ color: 'var(--text-secondary)' }}>
                  {formatRelative(r.ts)}
                </span>
                {(r.stdout || r.stderr) && (
                  <pre
                    className="text-[10px] font-mono flex-1 min-w-0 overflow-x-auto whitespace-pre-wrap break-all"
                    style={{ color: r.exit_code !== 0 ? 'var(--color-error)' : 'var(--text-secondary)' }}
                  >
                    {(r.stdout + r.stderr).trim()}
                  </pre>
                )}
              </div>
            ))}
          </div>
        </div>
      )}
    </>
  )
}

// ---------------------------------------------------------------------------
// Table header
// ---------------------------------------------------------------------------

function JobTableHeader() {
  return (
    <div
      className="grid items-center gap-2 px-3 py-1.5"
      style={{
        gridTemplateColumns: '1fr 110px 90px 80px auto',
        borderBottom: '1px solid var(--pill-border)',
      }}
    >
      <span className="text-[9px] uppercase tracking-wider font-medium" style={{ color: 'var(--text-secondary)' }}>Job</span>
      <span className="text-[9px] uppercase tracking-wider font-medium text-right" style={{ color: 'var(--text-secondary)' }}>Next Run</span>
      <span className="text-[9px] uppercase tracking-wider font-medium text-right" style={{ color: 'var(--text-secondary)' }}>Last Status</span>
      <span className="text-[9px] uppercase tracking-wider font-medium text-center" style={{ color: 'var(--text-secondary)' }}>History</span>
      <span className="text-[9px] uppercase tracking-wider font-medium" style={{ color: 'var(--text-secondary)' }}>Actions</span>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Modal (base overlay)
// ---------------------------------------------------------------------------

interface ModalProps {
  open: boolean
  onClose: () => void
  title: string
  children: React.ReactNode
  maxWidth?: string
}

function Modal({ open, onClose, title, children, maxWidth = '420px' }: ModalProps) {
  const overlayRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    document.addEventListener('keydown', handler)
    return () => document.removeEventListener('keydown', handler)
  }, [open, onClose])

  if (!open) return null

  return (
    <div
      ref={overlayRef}
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
      style={{ background: 'rgba(0,0,0,0.6)' }}
      onClick={e => { if (e.target === overlayRef.current) onClose() }}
      role="dialog"
      aria-modal="true"
      aria-labelledby="modal-title"
    >
      <div
        className="rounded-lg border p-5 flex flex-col gap-4 w-full"
        style={{
          maxWidth,
          background: 'var(--mic-bg)',
          borderColor: 'var(--pill-border)',
          boxShadow: '0 20px 60px rgba(0,0,0,0.4)',
        }}
      >
        <div className="flex items-center justify-between">
          <h2
            id="modal-title"
            className="text-sm font-semibold"
            style={{ color: 'var(--text-primary)' }}
          >
            {title}
          </h2>
          <button
            onClick={onClose}
            aria-label="Close dialog"
            className="h-6 w-6 rounded flex items-center justify-center text-sm hover:opacity-70"
            style={{ color: 'var(--text-secondary)' }}
          >
            ✕
          </button>
        </div>
        {children}
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// AddJobDialog
// ---------------------------------------------------------------------------

interface AddJobForm {
  name: string
  schedule: string
  command: string
}

interface AddJobDialogProps {
  open: boolean
  onClose: () => void
  onSubmit: (form: AddJobForm) => Promise<void>
}

function AddJobDialog({ open, onClose, onSubmit }: AddJobDialogProps) {
  const [form, setForm] = useState<AddJobForm>({ name: '', schedule: '', command: '' })
  const [submitting, setSubmitting] = useState(false)
  const [formError, setFormError] = useState<string | null>(null)
  const nameRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (open) {
      setForm({ name: '', schedule: '', command: '' })
      setFormError(null)
      setTimeout(() => nameRef.current?.focus(), 50)
    }
  }, [open])

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!form.name.trim() || !form.schedule.trim() || !form.command.trim()) {
      setFormError('All fields are required.')
      return
    }
    setSubmitting(true)
    setFormError(null)
    try {
      await onSubmit(form)
      onClose()
    } catch (err) {
      setFormError(err instanceof Error ? err.message : String(err))
    } finally {
      setSubmitting(false)
    }
  }

  const inputClass = cx(
    'w-full h-8 rounded-md border px-2.5 text-xs focus:outline-none focus:ring-1',
    'bg-transparent',
  )
  const inputStyle = { borderColor: 'var(--pill-border)', color: 'var(--text-primary)' }
  const labelClass = 'text-[10px] uppercase tracking-wider font-medium'

  return (
    <Modal open={open} onClose={onClose} title="Add Scheduled Job">
      <form onSubmit={handleSubmit} className="flex flex-col gap-3">
        <div className="flex flex-col gap-1">
          <label htmlFor="job-name" className={labelClass} style={{ color: 'var(--text-secondary)' }}>
            Name
          </label>
          <input
            ref={nameRef}
            id="job-name"
            type="text"
            value={form.name}
            onChange={e => setForm(f => ({ ...f, name: e.target.value }))}
            placeholder="e.g. daily-backup"
            className={inputClass}
            style={inputStyle}
          />
        </div>
        <div className="flex flex-col gap-1">
          <label htmlFor="job-schedule" className={labelClass} style={{ color: 'var(--text-secondary)' }}>
            Cron Schedule
          </label>
          <input
            id="job-schedule"
            type="text"
            value={form.schedule}
            onChange={e => setForm(f => ({ ...f, schedule: e.target.value }))}
            placeholder="e.g. 0 2 * * * (daily at 2am)"
            className={inputClass}
            style={inputStyle}
          />
          {form.schedule.trim().split(/\s+/).length === 5 && (
            <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
              → {cronToHuman(form.schedule)}
            </span>
          )}
        </div>
        <div className="flex flex-col gap-1">
          <label htmlFor="job-command" className={labelClass} style={{ color: 'var(--text-secondary)' }}>
            Command
          </label>
          <input
            id="job-command"
            type="text"
            value={form.command}
            onChange={e => setForm(f => ({ ...f, command: e.target.value }))}
            placeholder="e.g. /usr/local/bin/backup.sh"
            className={inputClass}
            style={inputStyle}
          />
        </div>

        {formError && (
          <p className="text-xs px-2 py-1.5 rounded" style={{ color: 'var(--color-error)', background: 'color-mix(in srgb, var(--color-error) 10%, transparent)' }}>
            {formError}
          </p>
        )}

        <div className="flex items-center justify-end gap-2 pt-1">
          <Button type="button" variant="outline" size="sm" onClick={onClose} disabled={submitting}>
            Cancel
          </Button>
          <Button type="submit" size="sm" disabled={submitting}>
            {submitting ? 'Adding…' : 'Add Job'}
          </Button>
        </div>
      </form>
    </Modal>
  )
}

// ---------------------------------------------------------------------------
// DeleteConfirmDialog
// ---------------------------------------------------------------------------

interface DeleteConfirmDialogProps {
  job: SchedulerJob | null
  onClose: () => void
  onConfirm: (job: SchedulerJob) => Promise<void>
}

function DeleteConfirmDialog({ job, onClose, onConfirm }: DeleteConfirmDialogProps) {
  const [deleting, setDeleting] = useState(false)
  const [deleteError, setDeleteError] = useState<string | null>(null)

  useEffect(() => {
    if (!job) {
      setDeleteError(null)
      setDeleting(false)
    }
  }, [job])

  const handleConfirm = async () => {
    if (!job) return
    setDeleting(true)
    setDeleteError(null)
    try {
      await onConfirm(job)
      onClose()
    } catch (err) {
      setDeleteError(err instanceof Error ? err.message : String(err))
    } finally {
      setDeleting(false)
    }
  }

  return (
    <Modal open={job !== null} onClose={onClose} title="Delete Job" maxWidth="360px">
      <p className="text-sm" style={{ color: 'var(--text-secondary)' }}>
        Delete job{' '}
        <span className="font-semibold font-mono" style={{ color: 'var(--text-primary)' }}>
          {job?.name}
        </span>
        ? This cannot be undone.
      </p>
      {deleteError && (
        <p className="text-xs px-2 py-1.5 rounded" style={{ color: 'var(--color-error)', background: 'color-mix(in srgb, var(--color-error) 10%, transparent)' }}>
          {deleteError}
        </p>
      )}
      <div className="flex items-center justify-end gap-2">
        <Button variant="outline" size="sm" onClick={onClose} disabled={deleting}>
          Cancel
        </Button>
        <Button variant="destructive" size="sm" onClick={handleConfirm} disabled={deleting}>
          {deleting ? 'Deleting…' : 'Delete'}
        </Button>
      </div>
    </Modal>
  )
}

// ---------------------------------------------------------------------------
// SchedulerView
// ---------------------------------------------------------------------------

export default function SchedulerView({ isConnected: _isConnected }: PluginViewProps) {
  const [status, setStatus] = useState<SchedulerStatus | null>(null)
  const [jobs, setJobs] = useState<SchedulerJob[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [feedback, setFeedback] = useState<{ msg: string; ok: boolean } | null>(null)
  const [runHistory, setRunHistory] = useState<Record<number, RunRecord[]>>({})

  // Sort / filter
  const [search, setSearch] = useState('')
  const [filterMode, setFilterMode] = useState<FilterMode>('all')
  const [sortKey, setSortKey] = useState<SortKey>('name')
  const [sortDir, setSortDir] = useState<SortDir>('asc')

  // Dialogs
  const [addOpen, setAddOpen] = useState(false)
  const [deleteTarget, setDeleteTarget] = useState<SchedulerJob | null>(null)

  // Countdown tick
  useEffect(() => {
    const id = setInterval(() => {
      // Re-render for live countdowns — no state update needed, but force via status clone
      setStatus(s => s ? { ...s } : s)
    }, 1_000)
    return () => clearInterval(id)
  }, [])

  const showFeedback = useCallback((msg: string, ok: boolean) => {
    setFeedback({ msg, ok })
    setTimeout(() => setFeedback(null), 4_000)
  }, [])

  const loadData = useCallback(async () => {
    try {
      const [statusResult, itemsResult] = await Promise.allSettled([
        queryPluginStatus('scheduler'),
        queryPluginItems('scheduler'),
      ])

      if (statusResult.status === 'fulfilled') {
        setStatus(statusResult.value as unknown as SchedulerStatus)
      }
      if (itemsResult.status === 'fulfilled') {
        const raw = itemsResult.value
        // Handle both array and {items, count} envelope shapes
        let items: unknown[]
        if (Array.isArray(raw)) {
          items = raw
        } else if (raw && typeof raw === 'object' && 'items' in raw && Array.isArray((raw as Record<string, unknown>)['items'])) {
          items = (raw as Record<string, unknown>)['items'] as unknown[]
        } else {
          items = []
        }
        setJobs(items as SchedulerJob[])
      }
      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setLoading(false)
    }
  }, [])

  // Initial load + 30s refresh
  useEffect(() => {
    void loadData()
    const id = setInterval(() => void loadData(), 30_000)
    return () => clearInterval(id)
  }, [loadData])

  const handleToggle = useCallback(async (job: SchedulerJob) => {
    setBusy(`toggle:${job.id}`)
    try {
      const verb = job.enabled ? 'disable' : 'enable'
      const data = await pluginAction('scheduler', verb, String(job.id))
      const ok = data['ok'] !== false
      showFeedback(ok ? `${job.name} ${verb}d` : `Failed to ${verb} ${job.name}`, ok)
      if (ok) await loadData()
    } catch (err) {
      showFeedback(`Error: ${err instanceof Error ? err.message : String(err)}`, false)
    } finally {
      setBusy(null)
    }
  }, [loadData, showFeedback])

  const handleRun = useCallback(async (job: SchedulerJob) => {
    setBusy(`run:${job.id}`)
    const startTs = new Date().toISOString()
    try {
      const data = await pluginAction('scheduler', 'run', String(job.id))
      const exitCode = typeof data['exit_code'] === 'number' ? data['exit_code'] : (data['ok'] !== false ? 0 : 1)
      const record: RunRecord = {
        ts: startTs,
        exit_code: exitCode,
        stdout: typeof data['stdout'] === 'string' ? data['stdout'] : '',
        stderr: typeof data['stderr'] === 'string' ? data['stderr'] : '',
      }
      setRunHistory(h => ({ ...h, [job.id]: [record, ...(h[job.id] ?? [])].slice(0, 10) }))
      showFeedback(
        exitCode === 0 ? `${job.name} ran (exit 0)` : `${job.name} failed (exit ${exitCode})`,
        exitCode === 0,
      )
      if (exitCode === 0) await loadData()
    } catch (err) {
      showFeedback(`Error: ${err instanceof Error ? err.message : String(err)}`, false)
    } finally {
      setBusy(null)
    }
  }, [loadData, showFeedback])

  const handleAdd = useCallback(async (form: AddJobForm) => {
    const payload = JSON.stringify({ name: form.name, schedule: form.schedule, command: form.command })
    const data = await pluginAction('scheduler', 'add', payload)
    if (data['ok'] === false) {
      throw new Error(typeof data['message'] === 'string' ? data['message'] : 'Add job failed — use CLI: scheduler jobs add')
    }
    showFeedback(`Job "${form.name}" added`, true)
    await loadData()
  }, [loadData, showFeedback])

  const handleDelete = useCallback(async (job: SchedulerJob) => {
    const data = await pluginAction('scheduler', 'delete', String(job.id))
    if (data['ok'] === false) {
      throw new Error(typeof data['message'] === 'string' ? data['message'] : 'Delete failed — use CLI: scheduler jobs remove')
    }
    showFeedback(`Job "${job.name}" deleted`, true)
    await loadData()
  }, [loadData, showFeedback])

  // Filtered + sorted jobs
  const visibleJobs = useMemo(() => {
    let filtered = jobs
    if (search.trim()) {
      const q = search.toLowerCase()
      filtered = filtered.filter(j => j.name.toLowerCase().includes(q) || j.schedule.toLowerCase().includes(q))
    }
    if (filterMode === 'enabled') filtered = filtered.filter(j => j.enabled)
    if (filterMode === 'disabled') filtered = filtered.filter(j => !j.enabled)
    return sortJobs(filtered, sortKey, sortDir)
  }, [jobs, search, filterMode, sortKey, sortDir])

  // ── Loading skeleton ─────────────────────────────────────────────────────

  if (loading) {
    return (
      <div className="flex flex-col gap-3 p-4 animate-pulse">
        <div className="h-12 rounded-lg" style={{ background: 'var(--mic-bg)' }} />
        <div className="h-7 rounded-md" style={{ background: 'var(--mic-bg)' }} />
        {[1, 2, 3].map(i => (
          <div key={i} className="h-12 rounded-lg" style={{ background: 'var(--mic-bg)' }} />
        ))}
      </div>
    )
  }

  // ── Error state ──────────────────────────────────────────────────────────

  if (error) {
    return (
      <div className="p-4 flex flex-col gap-2">
        <Badge variant="destructive">error</Badge>
        <p className="text-xs font-mono" style={{ color: 'var(--color-error)' }}>{error}</p>
        <Button variant="outline" size="sm" className="self-start" onClick={() => void loadData()}>
          Retry
        </Button>
      </div>
    )
  }

  // ── Main view ────────────────────────────────────────────────────────────

  return (
    <div className="flex flex-col gap-3 p-4">
      {/* Daemon status */}
      {status && <DaemonBanner status={status} onRefresh={() => void loadData()} />}

      {/* Action feedback toast */}
      {feedback && (
        <p
          className="text-xs px-2.5 py-1.5 rounded"
          style={{
            background: feedback.ok
              ? 'color-mix(in srgb, var(--color-success) 12%, transparent)'
              : 'color-mix(in srgb, var(--color-error) 12%, transparent)',
            color: feedback.ok ? 'var(--color-success)' : 'var(--color-error)',
          }}
          role="status"
          aria-live="polite"
        >
          {feedback.msg}
        </p>
      )}

      {/* Filter + sort + add */}
      <FilterBar
        search={search}
        filter={filterMode}
        sortKey={sortKey}
        sortDir={sortDir}
        onSearch={setSearch}
        onFilter={setFilterMode}
        onSortKey={setSortKey}
        onSortDir={setSortDir}
        onAdd={() => setAddOpen(true)}
      />

      {/* Job table */}
      <section aria-label="Scheduled jobs">
        {jobs.length === 0 ? (
          <p className="text-sm py-4 text-center" style={{ color: 'var(--text-secondary)' }}>
            No jobs. Run <code className="font-mono">scheduler init</code> to create the default pipeline.
          </p>
        ) : visibleJobs.length === 0 ? (
          <p className="text-sm py-4 text-center" style={{ color: 'var(--text-secondary)' }}>
            No jobs match the current filter.
          </p>
        ) : (
          <Card
            className="overflow-hidden"
            style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
          >
            <JobTableHeader />
            {visibleJobs.map(job => (
              <JobRow
                key={job.id}
                job={job}
                history={runHistory[job.id] ?? []}
                busy={busy}
                onToggle={handleToggle}
                onRun={handleRun}
                onDelete={setDeleteTarget}
              />
            ))}
          </Card>
        )}
      </section>

      {/* Dialogs */}
      <AddJobDialog
        open={addOpen}
        onClose={() => setAddOpen(false)}
        onSubmit={handleAdd}
      />
      <DeleteConfirmDialog
        job={deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={handleDelete}
      />
    </div>
  )
}
