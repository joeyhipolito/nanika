import { useState, useEffect, useCallback } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog'
import { Tabs, TabsList, TabsTrigger, TabsContent } from '@/components/ui/tabs'
import { queryPluginStatus, queryPluginItems, pluginAction } from '@/lib/wails'
import type { PluginViewProps } from '@/types'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type DraftState = 'pending' | 'approved' | 'rejected' | 'posted'

interface Draft {
  id: string
  state: DraftState
  platform: string
  opportunity: {
    title: string
    url: string
    author: string
  }
  comment: string
  persona: string
  created_at: string
  reviewed_at?: string
  posted_at?: string
}

interface StatusCounts {
  pending: number
  approved: number
  rejected: number
  posted: number
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const STATE_ORDER: DraftState[] = ['pending', 'approved', 'rejected', 'posted']

const PLATFORMS = ['youtube', 'linkedin', 'reddit', 'substack', 'x'] as const
type Platform = (typeof PLATFORMS)[number]

const PLATFORM_LABELS: Record<string, string> = {
  youtube: 'YT',
  linkedin: 'LI',
  reddit: 'RE',
  substack: 'SS',
  x: 'X',
}

const PLATFORM_NAMES: Record<Platform, string> = {
  youtube: 'YouTube',
  linkedin: 'LinkedIn',
  reddit: 'Reddit',
  substack: 'Substack',
  x: 'X (Twitter)',
}

const STATE_BADGE_VARIANT: Record<DraftState, 'default' | 'secondary' | 'destructive' | 'outline'> = {
  pending: 'default',
  approved: 'secondary',
  rejected: 'destructive',
  posted: 'outline',
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatRelativeTime(dateStr: string): string {
  const diffMs = Date.now() - new Date(dateStr).getTime()
  const min = Math.floor(diffMs / 60_000)
  if (min < 1) return 'just now'
  if (min < 60) return `${min}m ago`
  const h = Math.floor(min / 60)
  if (h < 24) return `${h}h ago`
  return `${Math.floor(h / 24)}d ago`
}

function formatAbsoluteTime(dateStr: string): string {
  return new Date(dateStr).toLocaleString(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  })
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function PlatformIcon({ platform }: { platform: string }) {
  const label = PLATFORM_LABELS[platform.toLowerCase()] ?? platform.slice(0, 2).toUpperCase()
  return (
    <span
      className="inline-flex items-center justify-center w-7 h-7 rounded text-[10px] font-bold font-mono flex-shrink-0"
      style={{
        background: 'var(--mic-bg)',
        color: 'var(--text-secondary)',
        border: '1px solid var(--pill-border)',
      }}
      aria-label={platform}
    >
      {label}
    </span>
  )
}

// ---------------------------------------------------------------------------
// Draft detail Dialog
// ---------------------------------------------------------------------------

interface DraftDialogProps {
  draft: Draft | null
  open: boolean
  onClose: () => void
  onAction: (verb: string, id: string) => Promise<void>
  busy: string | null
}

function DraftDialog({ draft, open, onClose, onAction, busy }: DraftDialogProps) {
  if (!draft) return null

  const title = draft.opportunity?.title ?? 'Untitled'
  const isBusyApprove = busy === `approve:${draft.id}`
  const isBusyReject = busy === `reject:${draft.id}`

  return (
    <Dialog open={open} onOpenChange={v => { if (!v) onClose() }}>
      <DialogContent
        className="max-w-lg"
        style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
      >
        <DialogHeader>
          <div className="flex items-center gap-2 pr-6">
            <PlatformIcon platform={draft.platform} />
            <DialogTitle className="text-sm font-medium leading-snug" style={{ color: 'var(--text-primary)' }}>
              {title}
            </DialogTitle>
          </div>
          <DialogDescription asChild>
            <div className="flex flex-wrap gap-2 mt-1">
              <Badge variant={STATE_BADGE_VARIANT[draft.state]} className="text-[10px] capitalize">
                {draft.state}
              </Badge>
              <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                {draft.platform}
              </span>
              {draft.persona && (
                <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                  persona: {draft.persona}
                </span>
              )}
            </div>
          </DialogDescription>
        </DialogHeader>

        {/* Full comment */}
        <div className="flex flex-col gap-3">
          <div>
            <p
              className="text-[10px] uppercase tracking-wider mb-1.5"
              style={{ color: 'var(--text-secondary)' }}
            >
              Comment
            </p>
            <p
              className="text-sm leading-relaxed whitespace-pre-wrap"
              style={{ color: 'var(--text-primary)' }}
            >
              {draft.comment || <em>No comment generated</em>}
            </p>
          </div>

          {/* Opportunity URL */}
          {draft.opportunity?.url && (
            <div>
              <p
                className="text-[10px] uppercase tracking-wider mb-1"
                style={{ color: 'var(--text-secondary)' }}
              >
                Source
              </p>
              <p
                className="text-xs font-mono truncate"
                style={{ color: 'var(--text-secondary)' }}
                title={draft.opportunity.url}
              >
                {draft.opportunity.url}
              </p>
            </div>
          )}

          {/* Timestamps */}
          <dl
            className="grid grid-cols-2 gap-x-4 gap-y-1 text-[10px]"
            style={{ color: 'var(--text-secondary)' }}
          >
            <div>
              <dt className="uppercase tracking-wider">Created</dt>
              <dd>{formatAbsoluteTime(draft.created_at)}</dd>
            </div>
            {draft.reviewed_at && (
              <div>
                <dt className="uppercase tracking-wider">Reviewed</dt>
                <dd>{formatAbsoluteTime(draft.reviewed_at)}</dd>
              </div>
            )}
            {draft.posted_at && (
              <div>
                <dt className="uppercase tracking-wider">Posted</dt>
                <dd>{formatAbsoluteTime(draft.posted_at)}</dd>
              </div>
            )}
          </dl>

          {/* Actions — pending only */}
          {draft.state === 'pending' && (
            <div className="flex gap-2 pt-1" role="group" aria-label={`Actions for ${title}`}>
              <Button
                size="sm"
                variant="outline"
                className="flex-1 h-8 text-xs"
                disabled={isBusyApprove || isBusyReject}
                onClick={async () => { await onAction('approve', draft.id); onClose() }}
              >
                {isBusyApprove ? 'Approving…' : 'Approve'}
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="flex-1 h-8 text-xs"
                style={{ color: 'var(--color-error)' }}
                disabled={isBusyApprove || isBusyReject}
                onClick={async () => { await onAction('reject', draft.id); onClose() }}
              >
                {isBusyReject ? 'Rejecting…' : 'Reject'}
              </Button>
            </div>
          )}
        </div>
      </DialogContent>
    </Dialog>
  )
}

// ---------------------------------------------------------------------------
// DraftCard
// ---------------------------------------------------------------------------

interface DraftCardProps {
  draft: Draft
  onAction: (verb: string, id: string) => Promise<void>
  onOpen: (draft: Draft) => void
  busy: string | null
}

function DraftCard({ draft, onAction, onOpen, busy }: DraftCardProps) {
  const title = draft.opportunity?.title ?? 'Untitled'
  const preview = draft.comment ?? ''
  const truncated = preview.length > 140 ? preview.slice(0, 140) + '…' : preview

  return (
    <article>
      <Card
        className="p-3 flex flex-col gap-2 cursor-pointer hover:opacity-80 transition-opacity"
        style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
        onClick={() => onOpen(draft)}
        role="button"
        tabIndex={0}
        aria-label={`Open draft: ${title}`}
        onKeyDown={e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); onOpen(draft) } }}
      >
        {/* Header: icon + title + badge */}
        <div className="flex items-start gap-2">
          <PlatformIcon platform={draft.platform} />
          <div className="flex-1 min-w-0">
            <p
              className="text-xs font-medium truncate leading-snug"
              style={{ color: 'var(--text-primary)' }}
              title={title}
            >
              {title}
            </p>
            <p className="text-[10px] mt-0.5" style={{ color: 'var(--text-secondary)' }}>
              {draft.platform} · {formatRelativeTime(draft.created_at)}
            </p>
          </div>
          <Badge variant={STATE_BADGE_VARIANT[draft.state]} className="text-[10px] capitalize flex-shrink-0">
            {draft.state}
          </Badge>
        </div>

        {/* Comment preview */}
        <p
          className="text-xs leading-relaxed"
          style={{ color: 'var(--text-secondary)' }}
        >
          {truncated || <em>No comment generated</em>}
        </p>

        {/* Approve / Reject inline — pending only */}
        {draft.state === 'pending' && (
          <div
            className="flex gap-1.5 pt-0.5"
            role="group"
            aria-label={`Actions for ${title}`}
            onClick={e => e.stopPropagation()}
          >
            <Button
              size="sm"
              variant="outline"
              className="h-6 px-2 text-[10px]"
              disabled={busy === `approve:${draft.id}`}
              onClick={() => onAction('approve', draft.id)}
            >
              {busy === `approve:${draft.id}` ? '…' : 'Approve'}
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="h-6 px-2 text-[10px]"
              style={{ color: 'var(--color-error)' }}
              disabled={busy === `reject:${draft.id}`}
              onClick={() => onAction('reject', draft.id)}
            >
              {busy === `reject:${draft.id}` ? '…' : 'Reject'}
            </Button>
          </div>
        )}
      </Card>
    </article>
  )
}

// ---------------------------------------------------------------------------
// StateGroup
// ---------------------------------------------------------------------------

interface StateGroupProps {
  state: DraftState
  drafts: Draft[]
  onAction: (verb: string, id: string) => Promise<void>
  onOpen: (draft: Draft) => void
  busy: string | null
}

function StateGroup({ state, drafts, onAction, onOpen, busy }: StateGroupProps) {
  return (
    <section aria-label={`${state} drafts`}>
      <h2
        className="text-[10px] uppercase tracking-wider font-medium mb-2 flex items-center gap-1.5"
        style={{ color: 'var(--text-secondary)' }}
      >
        {state}
        <span
          className="inline-flex items-center justify-center w-4 h-4 rounded-full text-[9px] font-bold"
          style={{ background: 'var(--pill-border)', color: 'var(--text-primary)' }}
          aria-label={`${drafts.length} ${state}`}
        >
          {drafts.length}
        </span>
      </h2>
      <div className="flex flex-col gap-2">
        {drafts.map(draft => (
          <DraftCard key={draft.id} draft={draft} onAction={onAction} onOpen={onOpen} busy={busy} />
        ))}
      </div>
    </section>
  )
}

// ---------------------------------------------------------------------------
// HistoryTimeline
// ---------------------------------------------------------------------------

interface HistoryTimelineProps {
  drafts: Draft[]
}

function HistoryTimeline({ drafts }: HistoryTimelineProps) {
  const posted = drafts
    .filter(d => d.state === 'posted')
    .sort((a, b) => {
      const ta = a.posted_at ?? a.created_at
      const tb = b.posted_at ?? b.created_at
      return tb.localeCompare(ta)
    })

  if (posted.length === 0) {
    return (
      <p className="text-sm py-4" style={{ color: 'var(--text-secondary)' }}>
        No posted comments yet.
      </p>
    )
  }

  return (
    <ol className="flex flex-col gap-0" aria-label="Posted comments timeline">
      {posted.map((draft, i) => {
        const title = draft.opportunity?.title ?? 'Untitled'
        const preview = draft.comment ?? ''
        const truncated = preview.length > 120 ? preview.slice(0, 120) + '…' : preview
        const postedAt = draft.posted_at ?? draft.reviewed_at ?? draft.created_at

        return (
          <li key={draft.id} className="flex gap-3 pb-4">
            {/* Timeline spine */}
            <div className="flex flex-col items-center flex-shrink-0 w-7">
              <PlatformIcon platform={draft.platform} />
              {i < posted.length - 1 && (
                <div
                  className="w-px flex-1 mt-1"
                  style={{ background: 'var(--pill-border)' }}
                  aria-hidden
                />
              )}
            </div>

            {/* Content */}
            <div className="flex flex-col gap-0.5 pt-0.5 min-w-0">
              <p
                className="text-xs font-medium truncate leading-snug"
                style={{ color: 'var(--text-primary)' }}
                title={title}
              >
                {title}
              </p>
              <p className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                {draft.platform} · {formatRelativeTime(postedAt)}
              </p>
              <p
                className="text-xs leading-relaxed mt-1"
                style={{ color: 'var(--text-secondary)' }}
                title={preview}
              >
                {truncated}
              </p>
            </div>
          </li>
        )
      })}
    </ol>
  )
}

// ---------------------------------------------------------------------------
// AdaptTab
// ---------------------------------------------------------------------------

function AdaptTab() {
  const [source, setSource] = useState('')
  const [selected, setSelected] = useState<Set<Platform>>(new Set(PLATFORMS))
  const [copied, setCopied] = useState(false)

  const platformArg = Array.from(selected).join(',')
  const cmd = source.trim()
    ? `engage adapt "${source.trim().replace(/"/g, '\\"')}" --platforms ${platformArg}`
    : ''

  function togglePlatform(p: Platform) {
    setSelected(prev => {
      const next = new Set(prev)
      if (next.has(p)) {
        if (next.size > 1) next.delete(p)
      } else {
        next.add(p)
      }
      return next
    })
  }

  async function handleCopy() {
    if (!cmd) return
    await navigator.clipboard.writeText(cmd)
    setCopied(true)
    setTimeout(() => setCopied(false), 2000)
  }

  return (
    <div className="flex flex-col gap-4 py-2">
      {/* Source input */}
      <div>
        <label
          htmlFor="adapt-source"
          className="block text-[10px] uppercase tracking-wider mb-1.5"
          style={{ color: 'var(--text-secondary)' }}
        >
          Source (URL or file path)
        </label>
        <input
          id="adapt-source"
          type="text"
          value={source}
          onChange={e => setSource(e.target.value)}
          placeholder="https://example.com/article or ~/article.md"
          className="w-full rounded border px-3 py-1.5 text-xs font-mono focus:outline-none focus:ring-1"
          style={{
            background: 'var(--mic-bg)',
            borderColor: 'var(--pill-border)',
            color: 'var(--text-primary)',
          }}
          aria-describedby="adapt-source-hint"
        />
        <p id="adapt-source-hint" className="mt-1 text-[10px]" style={{ color: 'var(--text-secondary)' }}>
          URL or path to the content you want to adapt for social platforms.
        </p>
      </div>

      {/* Platform checkboxes */}
      <fieldset>
        <legend
          className="text-[10px] uppercase tracking-wider mb-2"
          style={{ color: 'var(--text-secondary)' }}
        >
          Target platforms
        </legend>
        <div className="flex flex-wrap gap-2">
          {PLATFORMS.map(p => {
            const checked = selected.has(p)
            return (
              <label
                key={p}
                className="flex items-center gap-1.5 cursor-pointer select-none"
              >
                <input
                  type="checkbox"
                  checked={checked}
                  onChange={() => togglePlatform(p)}
                  className="w-3 h-3 rounded accent-current"
                  aria-label={PLATFORM_NAMES[p]}
                />
                <span className="text-xs" style={{ color: 'var(--text-primary)' }}>
                  {PLATFORM_NAMES[p]}
                </span>
              </label>
            )
          })}
        </div>
      </fieldset>

      {/* Generated command */}
      <div>
        <p
          className="text-[10px] uppercase tracking-wider mb-1.5"
          style={{ color: 'var(--text-secondary)' }}
        >
          CLI command
        </p>
        <div
          className="flex items-start gap-2 rounded border p-2"
          style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
        >
          <code
            className="flex-1 text-xs font-mono break-all leading-relaxed"
            style={{ color: cmd ? 'var(--text-primary)' : 'var(--text-secondary)' }}
          >
            {cmd || 'Enter a source above to generate the command.'}
          </code>
          <Button
            size="sm"
            variant="outline"
            className="h-6 px-2 text-[10px] flex-shrink-0"
            disabled={!cmd}
            onClick={handleCopy}
            aria-label="Copy command to clipboard"
          >
            {copied ? 'Copied!' : 'Copy'}
          </Button>
        </div>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// StatsBar
// ---------------------------------------------------------------------------

interface StatsBarProps {
  status: StatusCounts
}

function StatsBar({ status }: StatsBarProps) {
  return (
    <dl className="grid grid-cols-4 gap-2" aria-label="Queue counts by state">
      {STATE_ORDER.map(state => (
        <Card
          key={state}
          className="flex flex-col gap-0.5 p-2.5"
          style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
        >
          <dt className="text-[10px] uppercase tracking-wider" style={{ color: 'var(--text-secondary)' }}>
            {state}
          </dt>
          <dd
            className="text-lg font-mono font-bold leading-none"
            style={{
              color: state === 'pending' && (status[state] ?? 0) > 0
                ? 'var(--color-warning)'
                : 'var(--text-primary)',
            }}
          >
            {status[state] ?? 0}
          </dd>
        </Card>
      ))}
    </dl>
  )
}

// ---------------------------------------------------------------------------
// PlatformFilter
// ---------------------------------------------------------------------------

interface PlatformFilterProps {
  activePlatform: string
  onChange: (p: string) => void
  drafts: Draft[]
}

function PlatformFilter({ activePlatform, onChange, drafts }: PlatformFilterProps) {
  const platformsPresent = Array.from(new Set(drafts.map(d => d.platform.toLowerCase())))

  if (platformsPresent.length < 2) return null

  return (
    <div className="flex flex-wrap gap-1.5" role="group" aria-label="Filter by platform">
      <button
        className="text-[10px] px-2 py-0.5 rounded font-medium transition-colors focus:outline-none focus-visible:ring-1"
        style={{
          background: activePlatform === 'all' ? 'var(--pill-border)' : 'transparent',
          color: 'var(--text-secondary)',
          border: '1px solid var(--pill-border)',
        }}
        onClick={() => onChange('all')}
        aria-pressed={activePlatform === 'all'}
      >
        All
      </button>
      {platformsPresent.map(p => {
        const label = PLATFORM_LABELS[p] ?? p.slice(0, 2).toUpperCase()
        const isActive = activePlatform === p
        return (
          <button
            key={p}
            className="text-[10px] px-2 py-0.5 rounded font-medium transition-colors focus:outline-none focus-visible:ring-1"
            style={{
              background: isActive ? 'var(--pill-border)' : 'transparent',
              color: 'var(--text-secondary)',
              border: '1px solid var(--pill-border)',
            }}
            onClick={() => onChange(p)}
            aria-pressed={isActive}
          >
            {label}
          </button>
        )
      })}
    </div>
  )
}

// ---------------------------------------------------------------------------
// EngageView
// ---------------------------------------------------------------------------

export default function EngageView(_props: PluginViewProps) {
  const [status, setStatus] = useState<StatusCounts | null>(null)
  const [drafts, setDrafts] = useState<Draft[] | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [feedback, setFeedback] = useState<string | null>(null)
  const [activePlatform, setActivePlatform] = useState('all')
  const [selectedDraft, setSelectedDraft] = useState<Draft | null>(null)
  const [dialogOpen, setDialogOpen] = useState(false)

  const loadData = useCallback(async () => {
    try {
      const [statusRes, itemsRes] = await Promise.allSettled([
        queryPluginStatus('engage'),
        queryPluginItems('engage'),
      ])
      if (statusRes.status === 'fulfilled') {
        setStatus(statusRes.value as unknown as StatusCounts)
      }
      if (itemsRes.status === 'fulfilled') {
        setDrafts(itemsRes.value as unknown as Draft[])
      }
      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    loadData()
    const id = setInterval(loadData, 30_000)
    return () => clearInterval(id)
  }, [loadData])

  const handleAction = useCallback(async (verb: string, itemId: string) => {
    setBusy(`${verb}:${itemId}`)
    setFeedback(null)
    try {
      const data = await pluginAction('engage', verb, itemId)
      const ok = data.ok !== false
      setFeedback(ok ? `${verb} succeeded` : `${verb} failed: ${String(data['error'] ?? 'unknown error')}`)
      if (ok) await loadData()
    } catch (err) {
      setFeedback(`${verb} error: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      setBusy(null)
      setTimeout(() => setFeedback(null), 4_000)
    }
  }, [loadData])

  function openDraft(draft: Draft) {
    setSelectedDraft(draft)
    setDialogOpen(true)
  }

  function closeDraft() {
    setDialogOpen(false)
    setTimeout(() => setSelectedDraft(null), 150)
  }

  if (loading) {
    return (
      <div className="flex flex-col gap-4 p-4 animate-pulse">
        <div className="grid grid-cols-4 gap-2">
          {[1, 2, 3, 4].map(i => (
            <div key={i} className="h-10 rounded" style={{ background: 'var(--mic-bg)' }} />
          ))}
        </div>
        {[1, 2, 3].map(i => (
          <div key={i} className="h-20 rounded" style={{ background: 'var(--mic-bg)' }} />
        ))}
      </div>
    )
  }

  if (error) {
    return (
      <div className="p-4">
        <Badge variant="destructive" className="mb-2">error</Badge>
        <p className="text-sm font-mono" style={{ color: 'var(--color-error)' }}>{error}</p>
        <Button variant="outline" size="sm" className="mt-3" onClick={loadData}>Retry</Button>
      </div>
    )
  }

  const allDrafts = drafts ?? []

  // Apply platform filter
  const filteredDrafts = activePlatform === 'all'
    ? allDrafts
    : allDrafts.filter(d => d.platform.toLowerCase() === activePlatform)

  // Group by state
  const grouped: Record<DraftState, Draft[]> = { pending: [], approved: [], rejected: [], posted: [] }
  for (const d of filteredDrafts) {
    if (grouped[d.state]) grouped[d.state].push(d)
  }

  const visibleStates = STATE_ORDER.filter(s => grouped[s].length > 0)

  return (
    <>
      <DraftDialog
        draft={selectedDraft}
        open={dialogOpen}
        onClose={closeDraft}
        onAction={handleAction}
        busy={busy}
      />

      <div className="flex flex-col gap-3 p-4">
        {/* Action feedback */}
        {feedback && (
          <p
            className="text-xs px-2 py-1 rounded"
            role="status"
            aria-live="polite"
            style={{
              background: feedback.includes('failed') || feedback.includes('error')
                ? 'color-mix(in srgb, var(--color-error) 12%, transparent)'
                : 'color-mix(in srgb, var(--color-success) 12%, transparent)',
              color: feedback.includes('failed') || feedback.includes('error')
                ? 'var(--color-error)'
                : 'var(--color-success)',
            }}
          >
            {feedback}
          </p>
        )}

        {/* Stats bar */}
        {status && <StatsBar status={status} />}

        {/* Tabs */}
        <Tabs defaultValue="queue">
          <div className="flex items-center justify-between gap-2">
            <TabsList
              className="h-7"
              style={{ background: 'var(--mic-bg)', border: '1px solid var(--pill-border)' }}
            >
              <TabsTrigger value="queue" className="text-[11px] h-6 px-2.5">
                Queue
                {(status?.pending ?? 0) > 0 && (
                  <span
                    className="ml-1 inline-flex items-center justify-center w-4 h-4 rounded-full text-[9px] font-bold"
                    style={{ background: 'var(--color-warning)', color: '#000' }}
                    aria-label={`${status?.pending} pending`}
                  >
                    {status?.pending}
                  </span>
                )}
              </TabsTrigger>
              <TabsTrigger value="adapt" className="text-[11px] h-6 px-2.5">Adapt</TabsTrigger>
              <TabsTrigger value="history" className="text-[11px] h-6 px-2.5">
                History
                {(status?.posted ?? 0) > 0 && (
                  <span
                    className="ml-1 text-[9px]"
                    style={{ color: 'var(--text-secondary)' }}
                    aria-label={`${status?.posted} posted`}
                  >
                    {status?.posted}
                  </span>
                )}
              </TabsTrigger>
            </TabsList>

            <Button
              size="sm"
              variant="outline"
              className="h-7 px-2.5 text-[11px]"
              onClick={loadData}
              aria-label="Refresh engage queue"
            >
              Refresh
            </Button>
          </div>

          {/* Queue tab */}
          <TabsContent value="queue" className="mt-3">
            <div className="flex flex-col gap-3">
              <PlatformFilter
                activePlatform={activePlatform}
                onChange={setActivePlatform}
                drafts={allDrafts}
              />

              {visibleStates.length === 0 ? (
                <p className="text-sm py-2" style={{ color: 'var(--text-secondary)' }}>
                  {activePlatform === 'all' ? 'No drafts in queue.' : `No ${activePlatform} drafts.`}
                </p>
              ) : (
                <div className="flex flex-col gap-5">
                  {visibleStates.map(state => (
                    <StateGroup
                      key={state}
                      state={state}
                      drafts={grouped[state]}
                      onAction={handleAction}
                      onOpen={openDraft}
                      busy={busy}
                    />
                  ))}
                </div>
              )}
            </div>
          </TabsContent>

          {/* Adapt tab */}
          <TabsContent value="adapt" className="mt-3">
            <AdaptTab />
          </TabsContent>

          {/* History tab */}
          <TabsContent value="history" className="mt-3">
            <HistoryTimeline drafts={allDrafts} />
          </TabsContent>
        </Tabs>
      </div>
    </>
  )
}
