import { useState, useEffect, useCallback, useMemo } from 'react'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { queryPluginItems, queryPluginStatus, pluginAction } from '@/lib/wails'
import type { PluginViewProps } from '@/types'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface ThreadSummary {
  id: string
  account: string
  snippet: string
  from: string
  subject: string
  date: string
  unread: boolean
  has_attachment: boolean
  labels: string[]
  message_count: number
}

interface AccountStatus {
  alias: string
  unread: number
}

interface GmailStatus {
  accounts: AccountStatus[]
  total_unread: number
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const PLUGIN_NAME = 'gmail'

const AVATAR_COLORS = [
  '#e57373', '#f06292', '#ba68c8', '#9575cd',
  '#7986cb', '#64b5f6', '#4db6ac', '#81c784',
  '#aed581', '#ffb74d', '#ff8a65', '#a1887f',
]

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatDate(dateStr: string): string {
  if (!dateStr) return ''
  try {
    const d = new Date(dateStr)
    const now = new Date()
    const isToday =
      d.getDate() === now.getDate() &&
      d.getMonth() === now.getMonth() &&
      d.getFullYear() === now.getFullYear()
    if (isToday) {
      return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    }
    const isThisYear = d.getFullYear() === now.getFullYear()
    return d.toLocaleDateString([], {
      month: 'short',
      day: 'numeric',
      ...(!isThisYear ? { year: 'numeric' } : {}),
    })
  } catch {
    return dateStr
  }
}

function getSenderName(from: string): string {
  const match = from.match(/^"?([^"<]+)"?\s*</u)
  return match ? match[1].trim() : from.split('@')[0] ?? from
}

function getSenderInitials(from: string): string {
  const name = getSenderName(from)
  const parts = name.trim().split(/\s+/)
  if (parts.length >= 2) {
    return `${parts[0][0]}${parts[1][0]}`.toUpperCase()
  }
  return name.slice(0, 2).toUpperCase()
}

function getSenderColor(from: string): string {
  let hash = 0
  for (let i = 0; i < from.length; i++) {
    hash = from.charCodeAt(i) + ((hash << 5) - hash)
  }
  return AVATAR_COLORS[Math.abs(hash) % AVATAR_COLORS.length]!
}

// ---------------------------------------------------------------------------
// Avatar
// ---------------------------------------------------------------------------

function Avatar({ from }: { from: string }) {
  const initials = getSenderInitials(from)
  const color = getSenderColor(from)
  return (
    <div
      aria-hidden="true"
      className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-[10px] font-bold text-white"
      style={{ background: color }}
    >
      {initials}
    </div>
  )
}

// ---------------------------------------------------------------------------
// AccountTab
// ---------------------------------------------------------------------------

function AccountTab({
  label,
  badge,
  active,
  onClick,
}: {
  label: string
  badge: number
  active: boolean
  onClick: () => void
}) {
  return (
    <button
      onClick={onClick}
      className="flex shrink-0 items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs font-medium transition-colors"
      style={{
        background: active
          ? 'color-mix(in srgb, var(--accent) 15%, transparent)'
          : 'var(--mic-bg)',
        color: active ? 'var(--accent)' : 'var(--text-secondary)',
        border: `1px solid ${active ? 'var(--accent)' : 'var(--pill-border)'}`,
      }}
    >
      {label}
      {badge > 0 && (
        <span
          className="rounded-full px-1.5 py-0.5 text-[10px] font-bold"
          style={{
            background: active
              ? 'var(--accent)'
              : 'color-mix(in srgb, var(--accent) 20%, transparent)',
            color: active ? 'var(--bg)' : 'var(--accent)',
          }}
        >
          {badge}
        </span>
      )}
    </button>
  )
}

// ---------------------------------------------------------------------------
// LabelFilter (pill buttons, same visual language as AccountTab)
// ---------------------------------------------------------------------------

function LabelFilter({
  labels,
  active,
  onChange,
}: {
  labels: string[]
  active: string
  onChange: (label: string) => void
}) {
  if (labels.length === 0) return null
  return (
    <nav className="flex gap-1.5 overflow-x-auto pb-0.5" aria-label="Label filter">
      <AccountTab label="All labels" badge={0} active={active === ''} onClick={() => onChange('')} />
      {labels.map(label => (
        <AccountTab
          key={label}
          label={label}
          badge={0}
          active={active === label}
          onClick={() => onChange(label)}
        />
      ))}
    </nav>
  )
}

// ---------------------------------------------------------------------------
// ComposeOverlay — fixed modal, no lucide dependency
// ---------------------------------------------------------------------------

function ComposeOverlay({
  open,
  sending,
  to,
  subject,
  body,
  onToChange,
  onSubjectChange,
  onBodyChange,
  onSend,
  onClose,
}: {
  open: boolean
  sending: boolean
  to: string
  subject: string
  body: string
  onToChange: (v: string) => void
  onSubjectChange: (v: string) => void
  onBodyChange: (v: string) => void
  onSend: () => void
  onClose: () => void
}) {
  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose() }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [open, onClose])

  if (!open) return null

  return (
    <div
      className="fixed inset-0 z-50 flex items-end justify-end p-4"
      role="dialog"
      aria-modal="true"
      aria-label="Compose email"
    >
      {/* Backdrop */}
      <div
        className="absolute inset-0"
        style={{ background: 'rgba(0,0,0,0.4)' }}
        onClick={onClose}
        aria-hidden="true"
      />

      {/* Compose panel */}
      <div
        className="relative z-10 flex w-full max-w-md flex-col gap-3 rounded-xl p-4 shadow-2xl"
        style={{
          background: 'var(--bg)',
          border: '1px solid var(--pill-border)',
        }}
      >
        {/* Header */}
        <div className="flex items-center justify-between">
          <span className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
            New message
          </span>
          <button
            onClick={onClose}
            className="flex h-6 w-6 items-center justify-center rounded text-xs leading-none transition-opacity hover:opacity-70"
            style={{ color: 'var(--text-secondary)' }}
            aria-label="Close compose"
          >
            ✕
          </button>
        </div>

        {/* To field */}
        <input
          type="email"
          placeholder="To"
          value={to}
          onChange={e => onToChange(e.target.value)}
          className="w-full rounded-md px-3 py-2 text-xs outline-none focus:ring-1"
          style={{
            background: 'var(--mic-bg)',
            border: '1px solid var(--pill-border)',
            color: 'var(--text-primary)',
          }}
          aria-label="To"
          disabled={sending}
        />

        {/* Subject field */}
        <input
          type="text"
          placeholder="Subject"
          value={subject}
          onChange={e => onSubjectChange(e.target.value)}
          className="w-full rounded-md px-3 py-2 text-xs outline-none focus:ring-1"
          style={{
            background: 'var(--mic-bg)',
            border: '1px solid var(--pill-border)',
            color: 'var(--text-primary)',
          }}
          aria-label="Subject"
          disabled={sending}
        />

        {/* Body */}
        <textarea
          placeholder="Write your message..."
          value={body}
          onChange={e => onBodyChange(e.target.value)}
          rows={6}
          className="w-full resize-none rounded-md px-3 py-2 text-xs outline-none focus:ring-1"
          style={{
            background: 'var(--mic-bg)',
            border: '1px solid var(--pill-border)',
            color: 'var(--text-primary)',
          }}
          aria-label="Message body"
          disabled={sending}
        />

        {/* Actions */}
        <div className="flex justify-end gap-2">
          <Button size="sm" variant="outline" onClick={onClose} disabled={sending}>
            Discard
          </Button>
          <Button
            size="sm"
            disabled={sending || !to.trim() || !subject.trim() || !body.trim()}
            onClick={onSend}
          >
            {sending ? 'Sending…' : 'Send'}
          </Button>
        </div>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// ThreadRow
// ---------------------------------------------------------------------------

function ThreadRow({
  thread,
  expanded,
  actioning,
  replyDraft,
  replySending,
  onToggle,
  onMarkRead,
  onArchive,
  onLabel,
  onReplyChange,
  onReplySend,
}: {
  thread: ThreadSummary
  expanded: boolean
  actioning: boolean
  replyDraft: string
  replySending: boolean
  onToggle: () => void
  onMarkRead: () => void
  onArchive: () => void
  onLabel: (label: string) => void
  onReplyChange: (v: string) => void
  onReplySend: () => void
}) {
  const date = formatDate(thread.date)
  const visibleLabels = (thread.labels ?? []).filter(
    l => l !== 'UNREAD' && l !== 'INBOX',
  )

  return (
    <div>
      {/* Thread row */}
      <button
        className="flex w-full items-center gap-2.5 px-3 py-2.5 text-left transition-colors hover:opacity-80"
        onClick={onToggle}
        aria-expanded={expanded}
      >
        <Avatar from={thread.from} />

        {/* Unread dot */}
        <span
          className="h-1.5 w-1.5 shrink-0 rounded-full"
          style={{ background: thread.unread ? 'var(--accent)' : 'transparent' }}
          aria-hidden="true"
        />

        {/* Main content */}
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline justify-between gap-2">
            <span
              className={`truncate text-xs ${thread.unread ? 'font-semibold' : 'font-medium'}`}
              style={{ color: 'var(--text-primary)' }}
            >
              {getSenderName(thread.from)}
            </span>
            <span className="shrink-0 text-[10px]" style={{ color: 'var(--text-secondary)' }}>
              {date}
            </span>
          </div>
          <p
            className={`truncate text-xs ${thread.unread ? 'font-medium' : ''}`}
            style={{
              color: thread.unread ? 'var(--text-primary)' : 'var(--text-secondary)',
            }}
          >
            {thread.subject || '(no subject)'}
          </p>
          {!expanded && (
            <p className="truncate text-[10px]" style={{ color: 'var(--text-secondary)' }}>
              {thread.snippet}
            </p>
          )}
        </div>

        {/* Attachment indicator */}
        {thread.has_attachment && (
          <span
            className="shrink-0 text-[10px]"
            style={{ color: 'var(--text-secondary)' }}
            aria-label="Has attachment"
          >
            📎
          </span>
        )}

        {/* Message count */}
        {thread.message_count > 1 && (
          <span
            className="shrink-0 rounded px-1 py-0.5 text-[10px]"
            style={{
              background: 'color-mix(in srgb, var(--text-secondary) 15%, transparent)',
              color: 'var(--text-secondary)',
            }}
          >
            {thread.message_count}
          </span>
        )}
      </button>

      {/* Expanded view */}
      {expanded && (
        <div
          className="mx-3 mb-2.5 flex flex-col gap-2.5 rounded-md p-3"
          style={{
            background: 'color-mix(in srgb, var(--bg) 50%, transparent)',
            border: '1px solid var(--pill-border)',
          }}
        >
          {/* Snippet */}
          <p className="text-xs leading-relaxed" style={{ color: 'var(--text-secondary)' }}>
            {thread.snippet || 'No preview available.'}
          </p>

          {/* Labels */}
          {visibleLabels.length > 0 && (
            <div className="flex flex-wrap gap-1">
              {visibleLabels.map(label => (
                <span
                  key={label}
                  className="rounded px-1.5 py-0.5 text-[9px]"
                  style={{
                    background: 'color-mix(in srgb, var(--accent) 10%, transparent)',
                    color: 'var(--text-secondary)',
                  }}
                >
                  {label}
                </span>
              ))}
            </div>
          )}

          {/* Actions row */}
          <div className="flex flex-wrap gap-2">
            {thread.unread && (
              <Button
                size="sm"
                variant="outline"
                disabled={actioning}
                onClick={onMarkRead}
              >
                Mark read
              </Button>
            )}
            <Button
              size="sm"
              variant="outline"
              disabled={actioning}
              onClick={onArchive}
            >
              Archive
            </Button>
            <LabelActionMenu
              onLabel={onLabel}
              disabled={actioning}
            />
          </div>

          {/* Inline reply */}
          <div className="flex flex-col gap-2 pt-1">
            <textarea
              placeholder="Reply…"
              value={replyDraft}
              onChange={e => onReplyChange(e.target.value)}
              rows={3}
              className="w-full resize-none rounded-md px-3 py-2 text-xs outline-none focus:ring-1"
              style={{
                background: 'var(--mic-bg)',
                border: '1px solid var(--pill-border)',
                color: 'var(--text-primary)',
              }}
              aria-label={`Reply to ${thread.subject || 'thread'}`}
              disabled={replySending}
            />
            <div className="flex justify-end">
              <Button
                size="sm"
                disabled={replySending || !replyDraft.trim()}
                onClick={onReplySend}
              >
                {replySending ? 'Sending…' : 'Reply'}
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// LabelActionMenu — simple inline label input
// ---------------------------------------------------------------------------

function LabelActionMenu({
  onLabel,
  disabled,
}: {
  onLabel: (label: string) => void
  disabled: boolean
}) {
  const [open, setOpen] = useState(false)
  const [value, setValue] = useState('')

  const submit = () => {
    const trimmed = value.trim()
    if (trimmed) {
      onLabel(trimmed)
      setValue('')
      setOpen(false)
    }
  }

  if (!open) {
    return (
      <Button
        size="sm"
        variant="outline"
        disabled={disabled}
        onClick={() => setOpen(true)}
      >
        Label
      </Button>
    )
  }

  return (
    <div className="flex items-center gap-1">
      <input
        autoFocus
        type="text"
        placeholder="Label name"
        value={value}
        onChange={e => setValue(e.target.value)}
        onKeyDown={e => {
          if (e.key === 'Enter') submit()
          if (e.key === 'Escape') { setValue(''); setOpen(false) }
        }}
        className="rounded-md px-2 py-1 text-xs outline-none focus:ring-1"
        style={{
          background: 'var(--mic-bg)',
          border: '1px solid var(--pill-border)',
          color: 'var(--text-primary)',
          width: '100px',
        }}
      />
      <Button size="sm" onClick={submit} disabled={!value.trim()}>
        Apply
      </Button>
      <Button
        size="sm"
        variant="outline"
        onClick={() => { setValue(''); setOpen(false) }}
      >
        ✕
      </Button>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Main view
// ---------------------------------------------------------------------------

export default function GmailView(_props: PluginViewProps) {
  const [threads, setThreads] = useState<ThreadSummary[]>([])
  const [status, setStatus] = useState<GmailStatus | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')
  const [activeAccount, setActiveAccount] = useState<string>('all')
  const [expandedId, setExpandedId] = useState<string | null>(null)
  const [actioning, setActioning] = useState<string | null>(null)
  const [searchQuery, setSearchQuery] = useState('')
  const [labelFilter, setLabelFilter] = useState('')

  // Compose state
  const [composeOpen, setComposeOpen] = useState(false)
  const [composeTo, setComposeTo] = useState('')
  const [composeSubject, setComposeSubject] = useState('')
  const [composeBody, setComposeBody] = useState('')
  const [composeSending, setComposeSending] = useState(false)

  // Inline reply state
  const [replyDrafts, setReplyDrafts] = useState<Record<string, string>>({})
  const [replySending, setReplySending] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      setLoading(true)
      setError('')
      const [rawItems, rawStatus] = await Promise.allSettled([
        queryPluginItems(PLUGIN_NAME),
        queryPluginStatus(PLUGIN_NAME),
      ])

      if (rawItems.status === 'fulfilled') {
        const val = rawItems.value as unknown
        if (Array.isArray(val)) {
          setThreads(val as ThreadSummary[])
        } else if (val && typeof val === 'object' && 'items' in val) {
          setThreads((val as { items: ThreadSummary[] }).items ?? [])
        }
      }

      if (rawStatus.status === 'fulfilled') {
        setStatus(rawStatus.value as unknown as GmailStatus)
      }
    } catch {
      setError('Failed to load inbox')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void load()
    const interval = setInterval(() => void load(), 30_000)
    return () => clearInterval(interval)
  }, [load])

  const accounts = status?.accounts ?? []
  const totalUnread = status?.total_unread ?? threads.filter(t => t.unread).length

  // Derive available labels from all threads (excluding system labels)
  const availableLabels = useMemo(() => {
    const seen = new Set<string>()
    for (const t of threads) {
      for (const l of t.labels ?? []) {
        if (l !== 'UNREAD' && l !== 'INBOX' && l !== 'SENT' && l !== 'DRAFT') {
          seen.add(l)
        }
      }
    }
    return [...seen].sort()
  }, [threads])

  // Apply filters
  const visibleThreads = useMemo(() => {
    let result = threads

    if (activeAccount !== 'all') {
      result = result.filter(t => t.account === activeAccount)
    }

    if (labelFilter) {
      result = result.filter(t => (t.labels ?? []).includes(labelFilter))
    }

    if (searchQuery.trim()) {
      const q = searchQuery.trim().toLowerCase()
      result = result.filter(
        t =>
          t.from.toLowerCase().includes(q) ||
          t.subject.toLowerCase().includes(q) ||
          t.snippet.toLowerCase().includes(q),
      )
    }

    return result
  }, [threads, activeAccount, labelFilter, searchQuery])

  // ---------------------------------------------------------------------------
  // Actions
  // ---------------------------------------------------------------------------

  const handleMarkRead = async (thread: ThreadSummary) => {
    setActioning(thread.id)
    try {
      await pluginAction(PLUGIN_NAME, 'mark-read', `${thread.id}:${thread.account}`)
      setThreads(prev => prev.map(t => t.id === thread.id ? { ...t, unread: false } : t))
    } catch {
      // Corrects on next refresh
    } finally {
      setActioning(null)
    }
  }

  const handleArchive = async (thread: ThreadSummary) => {
    setActioning(thread.id)
    try {
      await pluginAction(PLUGIN_NAME, 'archive', `${thread.id}:${thread.account}`)
      setThreads(prev => prev.filter(t => t.id !== thread.id))
      if (expandedId === thread.id) setExpandedId(null)
    } catch {
      // Corrects on next refresh
    } finally {
      setActioning(null)
    }
  }

  const handleLabel = async (thread: ThreadSummary, label: string) => {
    setActioning(thread.id)
    try {
      await pluginAction(PLUGIN_NAME, 'label', `${thread.id}:${thread.account}:${label}`)
      setThreads(prev =>
        prev.map(t =>
          t.id === thread.id
            ? { ...t, labels: [...(t.labels ?? []), label] }
            : t,
        ),
      )
    } catch {
      // Corrects on next refresh
    } finally {
      setActioning(null)
    }
  }

  const handleReply = async (thread: ThreadSummary) => {
    const body = replyDrafts[thread.id] ?? ''
    if (!body.trim()) return
    setReplySending(thread.id)
    try {
      await pluginAction(
        PLUGIN_NAME,
        'reply',
        JSON.stringify({ thread_id: thread.id, account: thread.account, body }),
      )
      setReplyDrafts(prev => { const next = { ...prev }; delete next[thread.id]; return next })
    } catch {
      // Corrects on next refresh
    } finally {
      setReplySending(null)
    }
  }

  const handleComposeSend = async () => {
    setComposeSending(true)
    try {
      await pluginAction(
        PLUGIN_NAME,
        'compose',
        JSON.stringify({ to: composeTo, subject: composeSubject, body: composeBody }),
      )
      setComposeOpen(false)
      setComposeTo('')
      setComposeSubject('')
      setComposeBody('')
    } catch {
      // Stays open so user can retry
    } finally {
      setComposeSending(false)
    }
  }

  if (loading && threads.length === 0) {
    return (
      <div className="flex items-center justify-center py-12">
        <p className="text-sm" style={{ color: 'var(--text-secondary)' }}>
          Loading inbox…
        </p>
      </div>
    )
  }

  return (
    <>
      <div className="flex flex-col gap-3 p-4">
        {/* Error banner */}
        {error && (
          <p
            className="rounded px-2 py-1 text-xs"
            style={{
              background: 'color-mix(in srgb, var(--color-error) 12%, transparent)',
              color: 'var(--color-error)',
            }}
          >
            {error}
          </p>
        )}

        {/* Header: unread count + compose + refresh */}
        <div className="flex items-center justify-between">
          <span className="text-xs font-semibold" style={{ color: 'var(--text-primary)' }}>
            {totalUnread > 0 ? `${totalUnread} unread` : 'Inbox'}
          </span>
          <div className="flex gap-2">
            <Button size="sm" variant="outline" onClick={() => void load()} disabled={loading}>
              {loading ? 'Loading…' : 'Refresh'}
            </Button>
            <Button size="sm" onClick={() => setComposeOpen(true)}>
              Compose
            </Button>
          </div>
        </div>

        {/* Search bar */}
        <input
          type="search"
          placeholder="Search by sender, subject, or snippet…"
          value={searchQuery}
          onChange={e => setSearchQuery(e.target.value)}
          className="w-full rounded-md px-3 py-2 text-xs outline-none focus:ring-1"
          style={{
            background: 'var(--mic-bg)',
            border: '1px solid var(--pill-border)',
            color: 'var(--text-primary)',
          }}
          aria-label="Search inbox"
        />

        {/* Account switcher tabs */}
        {accounts.length > 1 && (
          <nav className="flex gap-1.5 overflow-x-auto pb-0.5" aria-label="Account switcher">
            <AccountTab
              label="All"
              badge={totalUnread}
              active={activeAccount === 'all'}
              onClick={() => setActiveAccount('all')}
            />
            {accounts.map(account => (
              <AccountTab
                key={account.alias}
                label={account.alias}
                badge={account.unread}
                active={activeAccount === account.alias}
                onClick={() => setActiveAccount(account.alias)}
              />
            ))}
          </nav>
        )}

        {/* Label filter */}
        <LabelFilter
          labels={availableLabels}
          active={labelFilter}
          onChange={setLabelFilter}
        />

        {/* Thread list */}
        <Card style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}>
          {visibleThreads.length === 0 ? (
            <p className="py-8 text-center text-xs" style={{ color: 'var(--text-secondary)' }}>
              {searchQuery || labelFilter ? 'No matching threads' : 'No unread threads'}
            </p>
          ) : (
            <div
              className="divide-y"
              style={{ borderColor: 'var(--pill-border)', maxHeight: '480px', overflowY: 'auto' }}
            >
              {visibleThreads.map(thread => (
                <ThreadRow
                  key={thread.id}
                  thread={thread}
                  expanded={expandedId === thread.id}
                  actioning={actioning === thread.id}
                  replyDraft={replyDrafts[thread.id] ?? ''}
                  replySending={replySending === thread.id}
                  onToggle={() =>
                    setExpandedId(prev => prev === thread.id ? null : thread.id)
                  }
                  onMarkRead={() => void handleMarkRead(thread)}
                  onArchive={() => void handleArchive(thread)}
                  onLabel={label => void handleLabel(thread, label)}
                  onReplyChange={v =>
                    setReplyDrafts(prev => ({ ...prev, [thread.id]: v }))
                  }
                  onReplySend={() => void handleReply(thread)}
                />
              ))}
            </div>
          )}
        </Card>
      </div>

      {/* Compose overlay */}
      <ComposeOverlay
        open={composeOpen}
        sending={composeSending}
        to={composeTo}
        subject={composeSubject}
        body={composeBody}
        onToChange={setComposeTo}
        onSubjectChange={setComposeSubject}
        onBodyChange={setComposeBody}
        onSend={() => void handleComposeSend()}
        onClose={() => { if (!composeSending) setComposeOpen(false) }}
      />
    </>
  )
}
