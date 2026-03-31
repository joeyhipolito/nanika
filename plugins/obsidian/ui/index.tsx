import React, { useState, useEffect, useCallback, useRef, useMemo } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { queryPluginStatus, queryPluginItems, pluginAction } from '@/lib/wails'
import type { PluginViewProps } from '@/types'

// ---------------------------------------------------------------------------
// Lightweight Tabs — matches the shadcn Tabs API surface used below.
// @/components/ui/tabs is not in the shared module registry, so we inline a
// minimal implementation using React context.
// ---------------------------------------------------------------------------

const TabsContext = React.createContext<{ value: string; onValueChange: (v: string) => void }>({
  value: '',
  onValueChange: () => undefined,
})

interface TabsProps {
  value: string
  onValueChange: (v: string) => void
  children: React.ReactNode
}

function Tabs({ value, onValueChange, children }: TabsProps) {
  return (
    <TabsContext.Provider value={{ value, onValueChange }}>
      <div>{children}</div>
    </TabsContext.Provider>
  )
}

interface TabsListProps {
  className?: string
  style?: React.CSSProperties
  children?: React.ReactNode
}

function TabsList({ className, style, children }: TabsListProps) {
  return (
    <div role="tablist" className={`flex ${className ?? ''}`} style={style}>
      {children}
    </div>
  )
}

interface TabsTriggerProps {
  value: string
  className?: string
  children?: React.ReactNode
}

function TabsTrigger({ value, className, children }: TabsTriggerProps) {
  const ctx = React.useContext(TabsContext)
  const isActive = ctx.value === value
  return (
    <button
      type="button"
      role="tab"
      aria-selected={isActive}
      onClick={() => ctx.onValueChange(value)}
      className={`rounded px-2 py-1 transition-colors ${className ?? ''}`}
      style={{
        background: isActive ? 'var(--pill-border)' : 'transparent',
        color: isActive ? 'var(--text-primary)' : 'var(--text-secondary)',
        border: 'none',
        cursor: 'pointer',
      }}
    >
      {children}
    </button>
  )
}

interface TabsContentProps {
  value: string
  children?: React.ReactNode
}

function TabsContent({ value, children }: TabsContentProps) {
  const ctx = React.useContext(TabsContext)
  if (ctx.value !== value) return null
  return <div className="mt-2">{children}</div>
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface VaultStatus {
  total_notes: number
  inbox_depth: number
  stale_captures: number
  orphan_notes: number
  link_density: number
  classification_distribution?: Record<string, number>
}

interface InboxItem {
  path: string
  name: string
  mod_time: number
}

interface NoteItem {
  path: string
  name: string
  mod_time: number
  size?: number
}

interface SearchResult {
  path: string
  title: string
  snippet?: string
  score?: number
  tags?: string[]
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function relativeTime(unixMs: number): string {
  const diffSec = Math.floor((Date.now() - unixMs) / 1000)
  if (diffSec < 60) return `${diffSec}s ago`
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`
  return `${Math.floor(diffSec / 86400)}d ago`
}

function displayName(path: string): string {
  return path.split('/').pop()?.replace(/\.md$/, '') ?? path
}

function parentDir(path: string): string {
  const parts = path.split('/')
  return parts.length > 1 ? parts.slice(0, -1).join('/') : '/'
}

function isFeedbackError(msg: string): boolean {
  return msg.toLowerCase().includes('fail') || msg.toLowerCase().includes('error')
}

// ---------------------------------------------------------------------------
// StatCard
// ---------------------------------------------------------------------------

interface StatCardProps {
  label: string
  value: number | string
  warn?: boolean
  accent?: boolean
}

function StatCard({ label, value, warn = false, accent = false }: StatCardProps) {
  return (
    <Card
      className="flex flex-col gap-1 p-3 min-w-0"
      style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
    >
      <span
        className="text-2xl font-mono font-semibold tabular-nums"
        style={{
          color: warn
            ? 'var(--color-warning)'
            : accent
              ? 'var(--accent)'
              : 'var(--text-primary)',
        }}
      >
        {value}
      </span>
      <span className="text-xs truncate" style={{ color: 'var(--text-secondary)' }}>
        {label}
      </span>
    </Card>
  )
}

// ---------------------------------------------------------------------------
// FeedbackBar
// ---------------------------------------------------------------------------

function FeedbackBar({ message }: { message: string }) {
  const isErr = isFeedbackError(message)
  return (
    <p
      className="text-xs px-2 py-1 rounded"
      style={{
        background: isErr
          ? 'color-mix(in srgb, var(--color-error) 12%, transparent)'
          : 'color-mix(in srgb, var(--color-success) 12%, transparent)',
        color: isErr ? 'var(--color-error)' : 'var(--color-success)',
      }}
    >
      {message}
    </p>
  )
}

// ---------------------------------------------------------------------------
// InboxRow — promote / archive / enrich
// ---------------------------------------------------------------------------

interface InboxRowProps {
  item: InboxItem
  busy: string | null
  onPromote: (path: string) => Promise<void>
  onArchive: (path: string) => Promise<void>
  onEnrich: (path: string) => Promise<void>
}

function InboxRow({ item, busy, onPromote, onArchive, onEnrich }: InboxRowProps) {
  const isBusy = busy === item.path
  const name = displayName(item.path)

  return (
    <div
      className="flex items-center gap-3 px-3 py-2"
      style={{ borderBottom: '1px solid var(--pill-border)' }}
    >
      <div className="flex-1 min-w-0">
        <p
          className="text-sm font-medium truncate"
          style={{ color: 'var(--text-primary)' }}
          title={item.path}
        >
          {name}
        </p>
        <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
          {relativeTime(item.mod_time * 1000)}
        </p>
      </div>
      <div className="flex shrink-0 gap-1">
        <Button
          variant="outline"
          size="sm"
          disabled={isBusy}
          onClick={() => onPromote(item.path)}
          className="text-xs"
          style={{ borderColor: 'var(--pill-border)', color: 'var(--text-secondary)' }}
          aria-label={`Promote ${name}`}
        >
          {isBusy ? '…' : 'Promote'}
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={isBusy}
          onClick={() => onEnrich(item.path)}
          className="text-xs"
          style={{ borderColor: 'var(--pill-border)', color: 'var(--text-secondary)' }}
          aria-label={`Enrich ${name}`}
        >
          {isBusy ? '…' : 'Enrich'}
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={isBusy}
          onClick={() => onArchive(item.path)}
          className="text-xs"
          style={{ borderColor: 'var(--pill-border)', color: 'var(--text-secondary)' }}
          aria-label={`Archive ${name}`}
        >
          {isBusy ? '…' : 'Archive'}
        </Button>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// SearchResultRow
// ---------------------------------------------------------------------------

interface SearchResultRowProps {
  result: SearchResult
}

function SearchResultRow({ result }: SearchResultRowProps) {
  return (
    <div
      className="flex flex-col gap-0.5 px-3 py-2"
      style={{ borderBottom: '1px solid var(--pill-border)' }}
    >
      <p
        className="text-sm font-medium truncate"
        style={{ color: 'var(--text-primary)' }}
        title={result.path}
      >
        {result.title || displayName(result.path)}
      </p>
      {result.snippet && (
        <p
          className="text-xs line-clamp-2"
          style={{ color: 'var(--text-secondary)' }}
        >
          {result.snippet}
        </p>
      )}
      <div className="flex items-center gap-2 mt-0.5">
        <span className="text-xs font-mono truncate" style={{ color: 'var(--text-secondary)', opacity: 0.6 }}>
          {result.path}
        </span>
        {result.score !== undefined && (
          <Badge
            variant="outline"
            className="text-xs shrink-0"
            style={{ borderColor: 'var(--pill-border)', color: 'var(--text-secondary)' }}
          >
            {(result.score * 100).toFixed(0)}%
          </Badge>
        )}
        {result.tags?.map((tag) => (
          <Badge
            key={tag}
            variant="secondary"
            className="text-xs shrink-0"
          >
            {tag}
          </Badge>
        ))}
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// NoteRow — recent notes and vault tree
// ---------------------------------------------------------------------------

interface NoteRowProps {
  note: NoteItem
}

function NoteRow({ note }: NoteRowProps) {
  return (
    <div
      className="flex items-center gap-3 px-3 py-2"
      style={{ borderBottom: '1px solid var(--pill-border)' }}
    >
      <div className="flex-1 min-w-0">
        <p
          className="text-sm font-medium truncate"
          style={{ color: 'var(--text-primary)' }}
          title={note.path}
        >
          {displayName(note.path)}
        </p>
        <p className="text-xs font-mono truncate" style={{ color: 'var(--text-secondary)', opacity: 0.7 }}>
          {note.path}
        </p>
      </div>
      <span className="text-xs shrink-0" style={{ color: 'var(--text-secondary)' }}>
        {relativeTime(note.mod_time * 1000)}
      </span>
    </div>
  )
}

// ---------------------------------------------------------------------------
// VaultTree — notes grouped by top-level directory
// ---------------------------------------------------------------------------

interface VaultTreeProps {
  notes: NoteItem[]
}

function VaultTree({ notes }: VaultTreeProps) {
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set())

  const grouped = useMemo(() => {
    const map = new Map<string, NoteItem[]>()
    for (const note of notes) {
      const dir = parentDir(note.path)
      const top = dir === '/' ? '(root)' : dir.split('/')[0]
      const existing = map.get(top) ?? []
      existing.push(note)
      map.set(top, existing)
    }
    return Array.from(map.entries()).sort(([a], [b]) => a.localeCompare(b))
  }, [notes])

  const toggle = (dir: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev)
      if (next.has(dir)) {
        next.delete(dir)
      } else {
        next.add(dir)
      }
      return next
    })
  }

  if (notes.length === 0) {
    return (
      <p className="text-sm py-6 text-center" style={{ color: 'var(--text-secondary)' }}>
        No notes found
      </p>
    )
  }

  return (
    <div className="flex flex-col">
      {grouped.map(([dir, dirNotes]) => {
        const isCollapsed = collapsed.has(dir)
        return (
          <div key={dir}>
            <button
              type="button"
              onClick={() => toggle(dir)}
              className="flex items-center gap-2 w-full px-3 py-1.5 text-left hover:opacity-80 transition-opacity"
              style={{ borderBottom: '1px solid var(--pill-border)' }}
              aria-expanded={!isCollapsed}
            >
              <span
                className="text-xs font-mono"
                style={{ color: 'var(--text-secondary)', opacity: 0.5, minWidth: '0.75rem' }}
              >
                {isCollapsed ? '▶' : '▼'}
              </span>
              <span className="text-xs font-semibold uppercase tracking-wide" style={{ color: 'var(--text-secondary)' }}>
                {dir}
              </span>
              <Badge
                variant="outline"
                className="text-xs ml-auto"
                style={{ borderColor: 'var(--pill-border)', color: 'var(--text-secondary)' }}
              >
                {dirNotes.length}
              </Badge>
            </button>
            {!isCollapsed && (
              <div style={{ paddingLeft: '1rem' }}>
                {dirNotes.map((note) => (
                  <NoteRow key={note.path} note={note} />
                ))}
              </div>
            )}
          </div>
        )
      })}
    </div>
  )
}

// ---------------------------------------------------------------------------
// LoadingList — skeleton placeholder
// ---------------------------------------------------------------------------

function LoadingList({ rows = 3 }: { rows?: number }) {
  return (
    <div className="flex flex-col gap-2 animate-pulse">
      {Array.from({ length: rows }, (_, i) => (
        <div
          key={i}
          className="h-10 rounded"
          style={{ background: 'var(--pill-border)' }}
        />
      ))}
    </div>
  )
}

// ---------------------------------------------------------------------------
// ObsidianView
// ---------------------------------------------------------------------------

const PLUGIN = 'obsidian'
const POLL_MS = 30_000
const SEARCH_DEBOUNCE_MS = 400

export default function ObsidianView({ isConnected: _isConnected }: PluginViewProps) {
  // ── Core data ──
  const [status, setStatus] = useState<VaultStatus | null>(null)
  const [inboxItems, setInboxItems] = useState<InboxItem[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  // ── Actions ──
  const [busy, setBusy] = useState<string | null>(null)
  const [feedback, setFeedback] = useState<string | null>(null)

  // ── Quick capture ──
  const [captureText, setCaptureText] = useState('')

  // ── Tabs ──
  const [activeTab, setActiveTab] = useState('inbox')

  // ── Inbox filter ──
  const [inboxFilter, setInboxFilter] = useState('')

  // ── Search tab ──
  const [searchQuery, setSearchQuery] = useState('')
  const [searchResults, setSearchResults] = useState<SearchResult[]>([])
  const [searchLoading, setSearchLoading] = useState(false)
  const searchDebounceRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  // ── Recent tab ──
  const [recentNotes, setRecentNotes] = useState<NoteItem[]>([])
  const [recentLoading, setRecentLoading] = useState(false)
  const recentLoadedRef = useRef(false)

  // ── Vault tab ──
  const [vaultNotes, setVaultNotes] = useState<NoteItem[]>([])
  const [vaultLoading, setVaultLoading] = useState(false)
  const vaultLoadedRef = useRef(false)

  // ── Feedback helper ──
  const showFeedback = useCallback((msg: string) => {
    setFeedback(msg)
    setTimeout(() => setFeedback(null), 4_000)
  }, [])

  // ── Load core data ──
  const loadData = useCallback(async () => {
    try {
      const [statusRes, itemsRes] = await Promise.allSettled([
        queryPluginStatus(PLUGIN),
        queryPluginItems(PLUGIN),
      ])

      if (statusRes.status === 'fulfilled') {
        setStatus(statusRes.value as unknown as VaultStatus)
      }

      if (itemsRes.status === 'fulfilled') {
        const raw = itemsRes.value
        const arr = Array.isArray(raw)
          ? (raw as unknown as InboxItem[])
          : ((raw as unknown as { items?: InboxItem[] }).items ?? [])
        setInboxItems(arr)
      }

      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void loadData()
    const id = setInterval(loadData, POLL_MS)
    return () => clearInterval(id)
  }, [loadData])

  // ── Load recent notes on tab switch ──
  const loadRecent = useCallback(async () => {
    if (recentLoadedRef.current) return
    recentLoadedRef.current = true
    setRecentLoading(true)
    try {
      const res = await pluginAction(PLUGIN, 'list', '')
      const raw = res as unknown
      let items: NoteItem[] = []
      if (Array.isArray(raw)) {
        items = raw as NoteItem[]
      } else if (raw && typeof raw === 'object') {
        const envelope = raw as { items?: NoteItem[]; notes?: NoteItem[] }
        items = envelope.items ?? envelope.notes ?? []
      }
      const sorted = [...items].sort((a, b) => b.mod_time - a.mod_time).slice(0, 50)
      setRecentNotes(sorted)
    } catch {
      // Non-fatal — tab shows empty state
    } finally {
      setRecentLoading(false)
    }
  }, [])

  // ── Load vault notes on tab switch ──
  const loadVault = useCallback(async () => {
    if (vaultLoadedRef.current) return
    vaultLoadedRef.current = true
    setVaultLoading(true)
    try {
      const res = await pluginAction(PLUGIN, 'list', '/')
      const raw = res as unknown
      let items: NoteItem[] = []
      if (Array.isArray(raw)) {
        items = raw as NoteItem[]
      } else if (raw && typeof raw === 'object') {
        const envelope = raw as { items?: NoteItem[]; notes?: NoteItem[] }
        items = envelope.items ?? envelope.notes ?? []
      }
      setVaultNotes(items)
    } catch {
      // Non-fatal
    } finally {
      setVaultLoading(false)
    }
  }, [])

  const handleTabChange = useCallback(
    (tab: string) => {
      setActiveTab(tab)
      if (tab === 'recent') void loadRecent()
      if (tab === 'vault') void loadVault()
    },
    [loadRecent, loadVault],
  )

  // ── Search ──
  const runSearch = useCallback(async (query: string) => {
    if (!query.trim()) {
      setSearchResults([])
      return
    }
    setSearchLoading(true)
    try {
      const res = await pluginAction(PLUGIN, 'search', query.trim())
      const raw = res as unknown
      let results: SearchResult[] = []
      if (Array.isArray(raw)) {
        results = raw as SearchResult[]
      } else if (raw && typeof raw === 'object') {
        const envelope = raw as { results?: SearchResult[]; items?: SearchResult[] }
        results = envelope.results ?? envelope.items ?? []
      }
      setSearchResults(results)
    } catch {
      setSearchResults([])
    } finally {
      setSearchLoading(false)
    }
  }, [])

  useEffect(() => {
    if (searchDebounceRef.current) clearTimeout(searchDebounceRef.current)
    searchDebounceRef.current = setTimeout(() => {
      void runSearch(searchQuery)
    }, SEARCH_DEBOUNCE_MS)
    return () => {
      if (searchDebounceRef.current) clearTimeout(searchDebounceRef.current)
    }
  }, [searchQuery, runSearch])

  // ── Actions ──
  const handlePromote = useCallback(
    async (path: string) => {
      setBusy(path)
      setFeedback(null)
      try {
        const res = await pluginAction(PLUGIN, 'promote', path)
        const ok = (res as { ok?: boolean }).ok !== false
        showFeedback(ok ? 'Promoted' : `Failed: ${(res as { error?: string }).error ?? 'unknown'}`)
        if (ok) await loadData()
      } catch (err) {
        showFeedback(`Error: ${err instanceof Error ? err.message : String(err)}`)
      } finally {
        setBusy(null)
      }
    },
    [loadData, showFeedback],
  )

  const handleArchive = useCallback(
    async (path: string) => {
      setBusy(path)
      setFeedback(null)
      try {
        const res = await pluginAction(PLUGIN, 'archive', path)
        const ok = (res as { ok?: boolean }).ok !== false
        showFeedback(ok ? 'Archived' : `Failed: ${(res as { error?: string }).error ?? 'unknown'}`)
        if (ok) await loadData()
      } catch (err) {
        showFeedback(`Error: ${err instanceof Error ? err.message : String(err)}`)
      } finally {
        setBusy(null)
      }
    },
    [loadData, showFeedback],
  )

  const handleEnrich = useCallback(
    async (path: string) => {
      setBusy(path)
      setFeedback(null)
      try {
        const res = await pluginAction(PLUGIN, 'enrich', path)
        const ok = (res as { ok?: boolean }).ok !== false
        showFeedback(ok ? 'Enriched' : `Failed: ${(res as { error?: string }).error ?? 'unknown'}`)
      } catch (err) {
        showFeedback(`Error: ${err instanceof Error ? err.message : String(err)}`)
      } finally {
        setBusy(null)
      }
    },
    [showFeedback],
  )

  const handleCapture = useCallback(async () => {
    const text = captureText.trim()
    if (!text) return
    setBusy('capture')
    setFeedback(null)
    try {
      const res = await pluginAction(PLUGIN, 'capture', text)
      const ok = (res as { ok?: boolean }).ok !== false
      if (ok) {
        setCaptureText('')
        showFeedback('Captured')
        recentLoadedRef.current = false
        await loadData()
      } else {
        showFeedback(`Failed: ${(res as { error?: string }).error ?? 'unknown'}`)
      }
    } catch (err) {
      showFeedback(`Error: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      setBusy(null)
    }
  }, [captureText, loadData, showFeedback])

  // ── Filtered inbox ──
  const filteredInbox = useMemo(() => {
    const q = inboxFilter.trim().toLowerCase()
    if (!q) return inboxItems
    return inboxItems.filter(
      (item) =>
        displayName(item.path).toLowerCase().includes(q) ||
        item.path.toLowerCase().includes(q),
    )
  }, [inboxItems, inboxFilter])

  // ── Loading skeleton ──
  if (loading) {
    return (
      <div className="flex flex-col gap-3 p-4 animate-pulse">
        <div className="grid grid-cols-4 gap-2">
          {[0, 1, 2, 3].map((i) => (
            <div key={i} className="h-16 rounded-lg" style={{ background: 'var(--pill-border)' }} />
          ))}
        </div>
        <div className="h-8 rounded" style={{ background: 'var(--pill-border)' }} />
        <div className="h-8 rounded" style={{ background: 'var(--pill-border)' }} />
        <LoadingList />
      </div>
    )
  }

  // ── Error state ──
  if (error) {
    return (
      <div className="p-4">
        <Badge variant="destructive" className="mb-2">error</Badge>
        <p className="text-sm font-mono" style={{ color: 'var(--color-error)' }}>{error}</p>
        <Button variant="outline" size="sm" className="mt-3" onClick={loadData}>
          Retry
        </Button>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-3 p-4">

      {/* ── Health stats bar ── */}
      <div className="grid grid-cols-4 gap-2" role="region" aria-label="Vault health">
        <StatCard label="total notes" value={status?.total_notes ?? 0} accent />
        <StatCard
          label="inbox depth"
          value={status?.inbox_depth ?? 0}
          warn={(status?.inbox_depth ?? 0) > 10}
        />
        <StatCard
          label="orphans"
          value={status?.orphan_notes ?? 0}
          warn={(status?.orphan_notes ?? 0) > 0}
        />
        <StatCard
          label="stale"
          value={status?.stale_captures ?? 0}
          warn={(status?.stale_captures ?? 0) > 0}
        />
      </div>

      {/* ── Quick capture ── */}
      <div className="flex gap-2">
        <input
          type="text"
          placeholder="Quick capture…"
          value={captureText}
          onChange={(e) => setCaptureText(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && void handleCapture()}
          disabled={busy === 'capture'}
          className="flex-1 rounded-md border px-3 py-1.5 text-sm outline-none focus:ring-1"
          style={{
            background: 'var(--mic-bg)',
            borderColor: 'var(--pill-border)',
            color: 'var(--text-primary)',
          }}
          aria-label="Quick capture text"
        />
        <Button
          size="sm"
          disabled={!captureText.trim() || busy === 'capture'}
          onClick={() => void handleCapture()}
        >
          {busy === 'capture' ? '…' : 'Capture'}
        </Button>
      </div>

      {/* ── Feedback ── */}
      {feedback && <FeedbackBar message={feedback} />}

      {/* ── Tabbed navigation ── */}
      <Tabs value={activeTab} onValueChange={handleTabChange}>
        <TabsList
          className="w-full"
          style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
        >
          <TabsTrigger value="inbox" className="flex-1 text-xs">
            Inbox
            {inboxItems.length > 0 && (
              <Badge
                variant="outline"
                className="ml-1.5 text-xs"
                style={{ borderColor: 'var(--pill-border)' }}
              >
                {inboxItems.length}
              </Badge>
            )}
          </TabsTrigger>
          <TabsTrigger value="search" className="flex-1 text-xs">Search</TabsTrigger>
          <TabsTrigger value="recent" className="flex-1 text-xs">Recent</TabsTrigger>
          <TabsTrigger value="vault" className="flex-1 text-xs">Vault</TabsTrigger>
        </TabsList>

        {/* ── Inbox tab ── */}
        <TabsContent value="inbox">
          <section aria-label="Inbox">
            <div className="mb-2">
              <input
                type="search"
                placeholder="Filter inbox…"
                value={inboxFilter}
                onChange={(e) => setInboxFilter(e.target.value)}
                className="w-full rounded-md border px-3 py-1.5 text-sm outline-none focus:ring-1"
                style={{
                  background: 'var(--mic-bg)',
                  borderColor: 'var(--pill-border)',
                  color: 'var(--text-primary)',
                }}
                aria-label="Filter inbox items"
              />
            </div>

            {filteredInbox.length === 0 ? (
              <p className="text-sm py-6 text-center" style={{ color: 'var(--text-secondary)' }}>
                {inboxFilter ? 'No matching items' : 'Inbox is empty'}
              </p>
            ) : (
              <div
                className="rounded-md overflow-hidden"
                style={{ border: '1px solid var(--pill-border)' }}
              >
                {filteredInbox.map((item) => (
                  <InboxRow
                    key={item.path}
                    item={item}
                    busy={busy}
                    onPromote={handlePromote}
                    onArchive={handleArchive}
                    onEnrich={handleEnrich}
                  />
                ))}
              </div>
            )}
          </section>
        </TabsContent>

        {/* ── Search tab ── */}
        <TabsContent value="search">
          <section aria-label="Vault search">
            <div className="mb-3">
              <input
                type="search"
                placeholder="Search vault…"
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                className="w-full rounded-md border px-3 py-1.5 text-sm outline-none focus:ring-1"
                style={{
                  background: 'var(--mic-bg)',
                  borderColor: 'var(--pill-border)',
                  color: 'var(--text-primary)',
                }}
                aria-label="Search vault"
                autoFocus
              />
            </div>

            {searchLoading ? (
              <LoadingList rows={4} />
            ) : searchResults.length > 0 ? (
              <div
                className="rounded-md overflow-hidden"
                style={{ border: '1px solid var(--pill-border)' }}
              >
                {searchResults.map((result) => (
                  <SearchResultRow key={result.path} result={result} />
                ))}
              </div>
            ) : searchQuery.trim() ? (
              <p className="text-sm py-6 text-center" style={{ color: 'var(--text-secondary)' }}>
                No results for &ldquo;{searchQuery}&rdquo;
              </p>
            ) : (
              <p className="text-sm py-6 text-center" style={{ color: 'var(--text-secondary)' }}>
                Type to search your vault
              </p>
            )}
          </section>
        </TabsContent>

        {/* ── Recent tab ── */}
        <TabsContent value="recent">
          <section aria-label="Recent notes">
            <div className="flex items-center justify-between mb-2">
              <h2 className="text-xs font-semibold uppercase tracking-wide" style={{ color: 'var(--text-secondary)' }}>
                Recently modified
              </h2>
              {recentNotes.length > 0 && (
                <Badge
                  variant="outline"
                  className="text-xs"
                  style={{ borderColor: 'var(--pill-border)', color: 'var(--text-secondary)' }}
                >
                  {recentNotes.length}
                </Badge>
              )}
            </div>

            {recentLoading ? (
              <LoadingList rows={5} />
            ) : recentNotes.length === 0 ? (
              <p className="text-sm py-6 text-center" style={{ color: 'var(--text-secondary)' }}>
                No notes found
              </p>
            ) : (
              <div
                className="rounded-md overflow-hidden"
                style={{ border: '1px solid var(--pill-border)' }}
              >
                {recentNotes.map((note) => (
                  <NoteRow key={note.path} note={note} />
                ))}
              </div>
            )}
          </section>
        </TabsContent>

        {/* ── Vault tab ── */}
        <TabsContent value="vault">
          <section aria-label="Vault browser">
            <div className="flex items-center justify-between mb-2">
              <h2 className="text-xs font-semibold uppercase tracking-wide" style={{ color: 'var(--text-secondary)' }}>
                Vault tree
              </h2>
              {vaultNotes.length > 0 && (
                <Badge
                  variant="outline"
                  className="text-xs"
                  style={{ borderColor: 'var(--pill-border)', color: 'var(--text-secondary)' }}
                >
                  {vaultNotes.length} notes
                </Badge>
              )}
            </div>

            {vaultLoading ? (
              <LoadingList rows={6} />
            ) : (
              <div
                className="rounded-md overflow-hidden"
                style={{ border: '1px solid var(--pill-border)' }}
              >
                <VaultTree notes={vaultNotes} />
              </div>
            )}
          </section>
        </TabsContent>
      </Tabs>
    </div>
  )
}
