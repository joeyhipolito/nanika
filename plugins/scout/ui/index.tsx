import { useState, useEffect, useCallback, useMemo } from 'react'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '@/components/ui/dialog'
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
// Domain types
// ---------------------------------------------------------------------------

interface TopicSummary {
  name: string
  description?: string
  source_count: number
  item_count: number
  last_gathered?: string
}

interface IntelItem {
  id: string
  title: string
  content?: string
  source_url?: string
  author?: string
  timestamp: string
  tags?: string[]
  score?: number
  engagement?: number
}

type SortOrder = 'score' | 'fresh'

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const SOURCE_LABELS: Record<string, string> = {
  'news.ycombinator.com': 'hackernews',
  'reddit.com': 'reddit',
  'dev.to': 'devto',
  'arxiv.org': 'arxiv',
  'github.com': 'github',
  'medium.com': 'medium',
  'lobste.rs': 'lobsters',
  'youtube.com': 'youtube',
  'youtu.be': 'youtube',
  'bluesky.app': 'bluesky',
  'bsky.app': 'bluesky',
  'x.com': 'x',
  'twitter.com': 'x',
  'producthunt.com': 'producthunt',
  'substack.com': 'substack',
  'linkedin.com': 'linkedin',
}

const SOURCE_COLORS: Record<string, string> = {
  hackernews: '#f97316',
  reddit: '#ef4444',
  devto: '#6366f1',
  arxiv: '#8b5cf6',
  github: '#10b981',
  medium: '#14b8a6',
  lobsters: '#dc2626',
  youtube: '#ef4444',
  bluesky: '#3b82f6',
  x: '#0ea5e9',
  producthunt: '#f59e0b',
  substack: '#f97316',
  linkedin: '#2563eb',
}

function detectSource(url?: string): string {
  if (!url) return 'web'
  try {
    const { hostname } = new URL(url)
    const bare = hostname.replace(/^www\./, '')
    for (const [domain, label] of Object.entries(SOURCE_LABELS)) {
      if (bare === domain || bare.endsWith(`.${domain}`)) return label
    }
    return bare.split('.')[0]
  } catch {
    return 'web'
  }
}

function relativeTime(iso: string): string {
  if (!iso) return ''
  const diff = Date.now() - new Date(iso).getTime()
  const s = Math.floor(diff / 1000)
  if (s < 60) return `${s}s ago`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m ago`
  const h = Math.floor(m / 60)
  if (h < 24) return `${h}h ago`
  const d = Math.floor(h / 24)
  if (d < 30) return `${d}d ago`
  const mo = Math.floor(d / 30)
  return `${mo}mo ago`
}

function scoreColor(score: number): string {
  if (score >= 80) return 'var(--color-success)'
  if (score >= 40) return 'var(--color-warning)'
  return 'var(--text-secondary)'
}

function truncate(text: string, max: number): string {
  if (text.length <= max) return text
  return text.slice(0, max).trimEnd() + '…'
}

// ---------------------------------------------------------------------------
// SourceBadge
// ---------------------------------------------------------------------------

function SourceBadge({ url }: { url?: string }) {
  const source = detectSource(url)
  const color = SOURCE_COLORS[source] ?? 'var(--text-secondary)'
  return (
    <span
      className="inline-flex items-center rounded px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide"
      style={{ background: `${color}22`, color }}
    >
      {source}
    </span>
  )
}

// ---------------------------------------------------------------------------
// ScoreChip
// ---------------------------------------------------------------------------

function ScoreChip({ score }: { score?: number }) {
  if (score === undefined || score === 0) return null
  const color = scoreColor(score)
  const label = score >= 80 ? 'high' : score >= 40 ? 'med' : 'low'
  return (
    <span
      className="inline-flex items-center gap-0.5 rounded px-1.5 py-0.5 text-[10px] font-mono font-semibold"
      style={{ background: `${color}22`, color }}
      aria-label={`Score ${score.toFixed(0)}`}
    >
      <span
        className="inline-block h-1.5 w-1.5 rounded-full"
        style={{ background: color }}
        aria-hidden="true"
      />
      {score >= 10 ? score.toFixed(0) : score.toFixed(1)} {label}
    </span>
  )
}

// ---------------------------------------------------------------------------
// IntelDetailDialog
// ---------------------------------------------------------------------------

interface IntelDetailDialogProps {
  item: IntelItem | null
  open: boolean
  onClose: () => void
}

function IntelDetailDialog({ item, open, onClose }: IntelDetailDialogProps) {
  if (!item) return null
  return (
    <Dialog open={open} onOpenChange={v => { if (!v) onClose() }}>
      <DialogContent className="max-w-2xl" style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}>
        <DialogHeader>
          <DialogTitle className="pr-6 text-sm font-semibold leading-snug" style={{ color: 'var(--text-primary)' }}>
            {item.title}
          </DialogTitle>
          <DialogDescription className="sr-only">
            Intel item: {item.title}
          </DialogDescription>
          <div className="mt-2 flex flex-wrap items-center gap-1.5">
            <SourceBadge url={item.source_url} />
            <ScoreChip score={item.score} />
            {item.author && (
              <span className="text-[11px]" style={{ color: 'var(--text-secondary)' }}>
                by {item.author}
              </span>
            )}
            {item.engagement !== undefined && item.engagement > 0 && (
              <span className="text-[11px]" style={{ color: 'var(--text-secondary)' }}>
                · {item.engagement.toLocaleString()} pts
              </span>
            )}
            <span
              className="ml-auto text-[11px] tabular-nums"
              style={{ color: 'var(--text-secondary)' }}
              title={item.timestamp}
            >
              {relativeTime(item.timestamp)}
            </span>
          </div>
        </DialogHeader>

        {item.content && (
          <p
            className="text-sm leading-relaxed"
            style={{ color: 'var(--text-secondary)' }}
          >
            {item.content}
          </p>
        )}

        {item.tags && item.tags.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {item.tags.map(tag => (
              <Badge key={tag} variant="secondary" className="text-[10px]">
                {tag}
              </Badge>
            ))}
          </div>
        )}

        <DialogFooter className="gap-2 sm:gap-0">
          {item.source_url && (
            <a
              href={item.source_url}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex h-8 items-center rounded border px-3 text-xs font-medium transition-colors hover:opacity-80"
              style={{
                borderColor: 'var(--pill-border)',
                color: 'var(--text-primary)',
              }}
            >
              Open source ↗
            </a>
          )}
          <Button size="sm" variant="outline" onClick={onClose}>
            Close
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ---------------------------------------------------------------------------
// IntelCard
// ---------------------------------------------------------------------------

interface IntelCardProps {
  item: IntelItem
  onClick: (item: IntelItem) => void
}

function IntelCard({ item, onClick }: IntelCardProps) {
  return (
    <Card
      className="flex cursor-pointer flex-col gap-2 p-3 transition-opacity hover:opacity-80"
      style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
      role="button"
      tabIndex={0}
      aria-label={`View details for ${item.title}`}
      onClick={() => onClick(item)}
      onKeyDown={e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); onClick(item) } }}
    >
      {/* Header */}
      <div className="flex items-start gap-2">
        <p
          className="min-w-0 flex-1 text-sm font-medium leading-snug"
          style={{ color: 'var(--text-primary)' }}
        >
          {item.title}
        </p>
      </div>

      {/* Snippet */}
      {item.content && (
        <p className="text-xs leading-relaxed" style={{ color: 'var(--text-secondary)' }}>
          {truncate(item.content, 160)}
        </p>
      )}

      {/* Footer: badges + freshness */}
      <div className="flex flex-wrap items-center gap-1.5">
        <SourceBadge url={item.source_url} />
        <ScoreChip score={item.score} />
        {item.engagement !== undefined && item.engagement > 0 && (
          <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
            {item.engagement.toLocaleString()} pts
          </span>
        )}
        <span
          className="ml-auto text-[10px] tabular-nums"
          style={{ color: 'var(--text-secondary)' }}
          title={item.timestamp}
        >
          {relativeTime(item.timestamp)}
        </span>
      </div>
    </Card>
  )
}

// ---------------------------------------------------------------------------
// TopicSidebar
// ---------------------------------------------------------------------------

interface TopicSidebarProps {
  topics: TopicSummary[]
  active: string
  gatheringTopic: string | null
  onSelect: (name: string) => void
  onGather: (name: string) => void
}

function TopicSidebar({ topics, active, gatheringTopic, onSelect, onGather }: TopicSidebarProps) {
  return (
    <nav
      className="flex w-40 flex-shrink-0 flex-col gap-0.5 overflow-y-auto"
      aria-label="Topics"
    >
      {topics.map(t => {
        const isActive = t.name === active
        const isGathering = gatheringTopic === t.name
        return (
          <div key={t.name} className="flex flex-col gap-0.5">
            <button
              role="tab"
              aria-selected={isActive}
              onClick={() => onSelect(t.name)}
              className="flex w-full items-center justify-between rounded px-2 py-1.5 text-left text-xs font-medium transition-colors"
              style={{
                background: isActive ? 'var(--pill-border)' : 'transparent',
                color: isActive ? 'var(--text-primary)' : 'var(--text-secondary)',
              }}
            >
              <span className="min-w-0 truncate">{t.name}</span>
              {t.item_count > 0 && (
                <span
                  className="ml-1 flex-shrink-0 rounded-full px-1.5 py-0.5 text-[9px] tabular-nums"
                  style={{ background: 'var(--pill-border)', color: 'var(--text-secondary)' }}
                >
                  {t.item_count}
                </span>
              )}
            </button>
            <button
              onClick={e => { e.stopPropagation(); onGather(t.name) }}
              disabled={isGathering}
              className="mx-2 rounded px-1.5 py-0.5 text-[10px] font-medium transition-opacity hover:opacity-70 disabled:opacity-40"
              style={{
                border: '1px solid var(--pill-border)',
                color: 'var(--text-secondary)',
              }}
              aria-label={`Gather intel for ${t.name}`}
            >
              {isGathering ? '…' : 'Gather'}
            </button>
          </div>
        )
      })}
    </nav>
  )
}

// ---------------------------------------------------------------------------
// StatsHeader
// ---------------------------------------------------------------------------

interface StatsHeaderProps {
  topic: TopicSummary
  lastGather: string
}

function StatsHeader({ topic, lastGather }: StatsHeaderProps) {
  return (
    <div
      className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded px-2 py-1.5"
      style={{ background: 'var(--mic-bg)', borderBottom: '1px solid var(--pill-border)' }}
    >
      <span
        className="text-xs font-semibold"
        style={{ color: 'var(--text-primary)' }}
      >
        {topic.name}
      </span>
      {topic.description && (
        <span className="min-w-0 truncate text-[11px]" style={{ color: 'var(--text-secondary)' }}>
          {topic.description}
        </span>
      )}
      <div className="ml-auto flex flex-shrink-0 items-center gap-2">
        {topic.source_count > 0 && (
          <span className="text-[10px] tabular-nums" style={{ color: 'var(--text-secondary)' }}>
            {topic.source_count} source{topic.source_count !== 1 ? 's' : ''}
          </span>
        )}
        <span className="text-[10px] tabular-nums" style={{ color: 'var(--text-secondary)' }}>
          {topic.item_count} item{topic.item_count !== 1 ? 's' : ''}
        </span>
        {(topic.last_gathered || lastGather) && (
          <span className="text-[10px] tabular-nums" style={{ color: 'var(--text-secondary)' }}>
            {relativeTime(topic.last_gathered ?? lastGather)}
          </span>
        )}
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// ScoutView
// ---------------------------------------------------------------------------

export default function ScoutView({ isConnected: _isConnected }: PluginViewProps) {
  const [topics, setTopics] = useState<TopicSummary[]>([])
  const [activeTopic, setActiveTopic] = useState<string>('')
  const [items, setItems] = useState<IntelItem[]>([])
  const [topicsLoading, setTopicsLoading] = useState(true)
  const [itemsLoading, setItemsLoading] = useState(false)
  const [gatheringTopic, setGatheringTopic] = useState<string | null>(null)
  const [feedback, setFeedback] = useState<string | null>(null)
  const [lastGather, setLastGather] = useState<string>('')
  const [filter, setFilter] = useState<string>('')
  const [sourceFilter, setSourceFilter] = useState<string>('all')
  const [sortOrder, setSortOrder] = useState<SortOrder>('score')
  const [selectedItem, setSelectedItem] = useState<IntelItem | null>(null)
  const [dialogOpen, setDialogOpen] = useState(false)

  // ── Load topics ───────────────────────────────────────────────────────────

  const loadTopics = useCallback(async () => {
    try {
      const [rawItems, status] = await Promise.allSettled([
        queryPluginItems('scout'),
        queryPluginStatus('scout'),
      ])
      if (rawItems.status === 'fulfilled') {
        const list = rawItems.value as unknown as TopicSummary[]
        setTopics(list)
        if (list.length > 0 && !activeTopic) {
          setActiveTopic(list[0].name)
        }
      }
      if (status.status === 'fulfilled') {
        const s = status.value as Record<string, unknown>
        if (typeof s.last_gather === 'string') setLastGather(s.last_gather)
      }
    } catch {
      // silently ignore
    } finally {
      setTopicsLoading(false)
    }
  }, [activeTopic])

  useEffect(() => {
    loadTopics()
  }, [loadTopics])

  // ── Load items ────────────────────────────────────────────────────────────

  const loadItems = useCallback(async (topic: string) => {
    if (!topic) return
    setItemsLoading(true)
    try {
      const result = await pluginAction('scout', 'intel', topic)
      const raw = result as unknown
      if (Array.isArray(raw)) {
        setItems(raw as IntelItem[])
      } else if (raw && typeof raw === 'object') {
        const asRecord = raw as Record<string, unknown>
        const candidate = asRecord['items'] ?? asRecord['data'] ?? asRecord['results']
        setItems(Array.isArray(candidate) ? (candidate as IntelItem[]) : [])
      } else {
        setItems([])
      }
    } catch {
      setItems([])
    } finally {
      setItemsLoading(false)
    }
  }, [])

  useEffect(() => {
    if (activeTopic) loadItems(activeTopic)
  }, [activeTopic, loadItems])

  // ── Actions ───────────────────────────────────────────────────────────────

  const handleTopicSelect = useCallback((name: string) => {
    setActiveTopic(name)
    setItems([])
    setFilter('')
    setSourceFilter('all')
  }, [])

  const handleGather = useCallback(async (topic: string) => {
    setGatheringTopic(topic)
    setFeedback(null)
    try {
      const result = await pluginAction('scout', 'gather', topic)
      const r = result as Record<string, unknown>
      const gathered = typeof r.items_gathered === 'number' ? r.items_gathered : null
      setFeedback(
        gathered !== null
          ? `${topic}: Gathered ${gathered} items`
          : `${topic}: Gather complete`
      )
      await loadTopics()
      if (activeTopic) await loadItems(activeTopic)
    } catch (err) {
      setFeedback(`Gather failed: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      setGatheringTopic(null)
      setTimeout(() => setFeedback(null), 5_000)
    }
  }, [loadTopics, loadItems, activeTopic])

  const handleCardClick = useCallback((item: IntelItem) => {
    setSelectedItem(item)
    setDialogOpen(true)
  }, [])

  // ── Derived ───────────────────────────────────────────────────────────────

  // Unique sources present in the current item list
  const availableSources = useMemo(() => {
    const seen = new Set<string>()
    for (const item of items) {
      seen.add(detectSource(item.source_url))
    }
    return Array.from(seen).sort()
  }, [items])

  const filteredItems = useMemo(() =>
    items
      .filter(item => {
        if (sourceFilter !== 'all' && detectSource(item.source_url) !== sourceFilter) return false
        if (!filter) return true
        const q = filter.toLowerCase()
        return (
          item.title.toLowerCase().includes(q) ||
          (item.content ?? '').toLowerCase().includes(q) ||
          detectSource(item.source_url).toLowerCase().includes(q) ||
          (item.tags ?? []).some(tag => tag.toLowerCase().includes(q))
        )
      })
      .sort((a, b) =>
        sortOrder === 'score'
          ? (b.score ?? 0) - (a.score ?? 0)
          : new Date(b.timestamp).getTime() - new Date(a.timestamp).getTime()
      ),
    [items, filter, sourceFilter, sortOrder]
  )

  const activeTopicMeta = topics.find(t => t.name === activeTopic)

  // ── Loading skeleton ──────────────────────────────────────────────────────

  if (topicsLoading) {
    return (
      <div className="flex gap-3 p-4">
        <div className="flex w-40 flex-shrink-0 flex-col gap-2">
          {[1, 2, 3].map(i => <Skeleton key={i} className="h-8 w-full" />)}
        </div>
        <div className="flex flex-1 flex-col gap-2">
          {[1, 2, 3, 4].map(i => <Skeleton key={i} className="h-20 w-full" />)}
        </div>
      </div>
    )
  }

  // ── Empty state ───────────────────────────────────────────────────────────

  if (topics.length === 0) {
    return (
      <div className="flex flex-col items-center gap-3 p-8 text-center">
        <p className="text-sm" style={{ color: 'var(--text-secondary)' }}>
          No topics configured.
        </p>
        <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
          Run <code className="font-mono">scout topics add</code> to get started.
        </p>
        <Button size="sm" variant="outline" onClick={() => loadTopics()}>
          Refresh
        </Button>
      </div>
    )
  }

  // ── Main layout ───────────────────────────────────────────────────────────

  return (
    <div className="flex flex-col gap-0">
      {/* Feedback banner */}
      {feedback && (
        <p
          className="px-3 py-1.5 text-xs"
          style={{
            background: feedback.includes('failed')
              ? 'color-mix(in srgb, var(--color-error) 12%, transparent)'
              : 'color-mix(in srgb, var(--color-success) 12%, transparent)',
            color: feedback.includes('failed') ? 'var(--color-error)' : 'var(--color-success)',
          }}
        >
          {feedback}
        </p>
      )}

      <div className="flex min-h-0 flex-1 gap-0">
        {/* Sidebar */}
        <div
          className="flex w-44 flex-shrink-0 flex-col border-r p-3"
          style={{ borderColor: 'var(--pill-border)' }}
        >
          <p className="mb-2 text-[10px] font-semibold uppercase tracking-wider" style={{ color: 'var(--text-secondary)' }}>
            Topics
          </p>
          <TopicSidebar
            topics={topics}
            active={activeTopic}
            gatheringTopic={gatheringTopic}
            onSelect={handleTopicSelect}
            onGather={handleGather}
          />
          <div className="mt-auto pt-3">
            <Button
              size="sm"
              variant="outline"
              className="w-full text-xs"
              onClick={async () => {
                await loadTopics()
                if (activeTopic) await loadItems(activeTopic)
              }}
            >
              Refresh
            </Button>
          </div>
        </div>

        {/* Main content */}
        <div className="flex min-w-0 flex-1 flex-col gap-0">
          {/* Stats header */}
          {activeTopicMeta && (
            <StatsHeader topic={activeTopicMeta} lastGather={lastGather} />
          )}

          {/* Filter bar */}
          {(items.length > 0 || filter || sourceFilter !== 'all') && (
            <div
              className="flex items-center gap-2 border-b px-3 py-2"
              style={{ borderColor: 'var(--pill-border)' }}
            >
              <input
                type="search"
                placeholder="Search…"
                value={filter}
                onChange={e => setFilter(e.target.value)}
                className="min-w-0 flex-1 rounded border px-2 py-1 text-xs outline-none focus:ring-1"
                style={{
                  background: 'var(--mic-bg)',
                  borderColor: 'var(--pill-border)',
                  color: 'var(--text-primary)',
                }}
                aria-label="Filter intel items"
              />

              {availableSources.length > 1 && (
                <Select value={sourceFilter} onValueChange={setSourceFilter}>
                  <SelectTrigger
                    className="h-7 w-[110px] border text-[11px]"
                    style={{
                      background: 'var(--mic-bg)',
                      borderColor: 'var(--pill-border)',
                      color: 'var(--text-primary)',
                    }}
                    aria-label="Filter by source"
                  >
                    <SelectValue placeholder="Source" />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">All sources</SelectItem>
                    {availableSources.map(src => (
                      <SelectItem key={src} value={src}>
                        {src}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}

              <div
                className="flex overflow-hidden rounded border text-[10px] font-medium"
                style={{ borderColor: 'var(--pill-border)' }}
                role="group"
                aria-label="Sort order"
              >
                {(['score', 'fresh'] as SortOrder[]).map(opt => (
                  <button
                    key={opt}
                    onClick={() => setSortOrder(opt)}
                    className="px-2 py-1 transition-colors"
                    style={{
                      background: sortOrder === opt ? 'var(--pill-border)' : 'transparent',
                      color: sortOrder === opt ? 'var(--text-primary)' : 'var(--text-secondary)',
                    }}
                    aria-pressed={sortOrder === opt}
                  >
                    {opt === 'score' ? '↓ score' : '↓ fresh'}
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* Item list */}
          <div className="flex flex-1 flex-col gap-0 overflow-y-auto p-3">
            {itemsLoading ? (
              <div className="flex flex-col gap-2">
                {[1, 2, 3, 4].map(i => <Skeleton key={i} className="h-20 w-full" />)}
              </div>
            ) : items.length === 0 ? (
              <div className="flex flex-col items-center gap-2 py-8 text-center">
                <p className="text-sm" style={{ color: 'var(--text-secondary)' }}>
                  No intel for <strong>{activeTopic}</strong>.
                </p>
                <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
                  Click <strong>Gather</strong> in the sidebar to collect items.
                </p>
              </div>
            ) : filteredItems.length === 0 ? (
              <div className="flex flex-col items-center gap-2 py-8 text-center">
                <p className="text-sm" style={{ color: 'var(--text-secondary)' }}>
                  No items match your filters.
                </p>
                <button
                  className="text-xs underline"
                  style={{ color: 'var(--text-secondary)' }}
                  onClick={() => { setFilter(''); setSourceFilter('all') }}
                >
                  Clear filters
                </button>
              </div>
            ) : (
              <section aria-label={`Intel items for ${activeTopic}`}>
                <div className="flex flex-col gap-2">
                  {filteredItems.map(item => (
                    <IntelCard key={item.id} item={item} onClick={handleCardClick} />
                  ))}
                </div>
                <p className="mt-2 text-center text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                  {filteredItems.length}
                  {(filter || sourceFilter !== 'all') ? ` of ${items.length}` : ''}{' '}
                  item{filteredItems.length !== 1 ? 's' : ''} ·{' '}
                  {sortOrder === 'score' ? 'top by score' : 'newest first'}
                </p>
              </section>
            )}
          </div>
        </div>
      </div>

      {/* Detail dialog */}
      <IntelDetailDialog
        item={selectedItem}
        open={dialogOpen}
        onClose={() => setDialogOpen(false)}
      />
    </div>
  )
}
