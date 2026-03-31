import { useState, useEffect, useCallback } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { Tabs, TabsList, TabsTrigger, TabsContent } from '@/components/ui/tabs'
import { queryPluginStatus, queryPluginItems, pluginAction } from '@/lib/wails'
import type { PluginViewProps } from '@/types'

// ── Types ────────────────────────────────────────────────────────────────────

interface YoutubeStatus {
  configured: boolean
  has_api_key: boolean
  authenticated: boolean
  channel_count: number
  budget: number
}

interface ChannelItem {
  type: string
  value: string
}

interface VideoCandidate {
  id: string
  platform: string
  url: string
  title: string
  body: string
  author: string
  created_at: string
  meta?: Record<string, string>
}

type CommentStatus = 'queued' | 'posting' | 'posted' | 'failed'

interface CommentEntry {
  sessionId: string
  videoId: string
  text: string
  status: CommentStatus
  postedAt?: string
  error?: string
}

// ── Constants ────────────────────────────────────────────────────────────────

const PLUGIN = 'youtube'
const POLL_MS = 60_000

const COMMENT_STATUS_VARIANT: Record<CommentStatus, 'default' | 'secondary' | 'destructive' | 'outline'> = {
  queued: 'default',
  posting: 'secondary',
  posted: 'outline',
  failed: 'destructive',
}

// ── Helpers ──────────────────────────────────────────────────────────────────

function formatViewCount(raw: string | undefined): string {
  if (!raw) return ''
  const n = parseInt(raw, 10)
  if (isNaN(n)) return raw
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M views`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K views`
  return `${n} views`
}

function formatRelativeTime(dateStr: string): string {
  const diffMs = Date.now() - new Date(dateStr).getTime()
  const min = Math.floor(diffMs / 60_000)
  if (min < 1) return 'just now'
  if (min < 60) return `${min}m ago`
  const h = Math.floor(min / 60)
  if (h < 24) return `${h}h ago`
  return `${Math.floor(h / 24)}d ago`
}

let _sessionCounter = 0
function nextSessionId(): string {
  return `sess-${++_sessionCounter}`
}

// ── Sub-components ───────────────────────────────────────────────────────────

interface OAuthIndicatorProps {
  authenticated: boolean
  hasApiKey: boolean
  configured: boolean
}

function OAuthIndicator({ authenticated, hasApiKey, configured }: OAuthIndicatorProps) {
  return (
    <div className="flex items-center gap-2 flex-wrap">
      <Badge variant={configured ? 'secondary' : 'destructive'} className="text-[10px]">
        {configured ? 'configured' : 'not configured'}
      </Badge>
      <Badge variant={hasApiKey ? 'secondary' : 'outline'} className="text-[10px]">
        {hasApiKey ? 'API key ✓' : 'no API key'}
      </Badge>
      <span
        className="inline-flex items-center gap-1 text-[10px] font-medium px-2 py-0.5 rounded-full"
        style={{
          background: authenticated
            ? 'color-mix(in srgb, var(--color-success) 15%, transparent)'
            : 'color-mix(in srgb, var(--color-warning) 15%, transparent)',
          color: authenticated ? 'var(--color-success)' : 'var(--color-warning)',
        }}
        aria-label={`OAuth status: ${authenticated ? 'active' : 'required'}`}
      >
        <span className="w-1.5 h-1.5 rounded-full flex-shrink-0" style={{ background: 'currentColor' }} aria-hidden="true" />
        {authenticated ? 'OAuth active' : 'OAuth required'}
      </span>
    </div>
  )
}

interface QuotaBarProps {
  budget: number
}

function QuotaBar({ budget }: QuotaBarProps) {
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center justify-between">
        <span className="text-[10px] uppercase tracking-wider" style={{ color: 'var(--text-secondary)' }}>
          Daily Quota Budget
        </span>
        <span className="text-[10px] font-mono" style={{ color: 'var(--text-primary)' }}>
          {budget.toLocaleString()} units
        </span>
      </div>
      <div
        className="h-1.5 rounded-full overflow-hidden"
        style={{ background: 'var(--pill-border)' }}
        role="meter"
        aria-label={`Daily quota budget: ${budget} units`}
        aria-valuemin={0}
        aria-valuemax={budget}
        aria-valuenow={budget}
      >
        <div
          className="h-full rounded-full"
          style={{ width: '100%', background: 'var(--color-success)' }}
        />
      </div>
      <p className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
        Run{' '}
        <code className="font-mono" style={{ color: 'var(--text-primary)' }}>
          youtube doctor
        </code>{' '}
        to see actual quota usage
      </p>
    </div>
  )
}

interface ChannelCardProps {
  channel: ChannelItem
}

function ChannelCard({ channel }: ChannelCardProps) {
  const handle = channel.value
  const initial = (handle[0] ?? 'Y').toUpperCase()
  const isId = handle.startsWith('UC') && handle.length > 20

  return (
    <article>
      <Card
        className="p-3 flex items-center gap-3"
        style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
      >
        <div
          className="w-8 h-8 rounded-full flex items-center justify-center flex-shrink-0 text-xs font-bold"
          style={{
            background: 'color-mix(in srgb, #d97757 20%, transparent)',
            color: '#d97757',
          }}
          aria-hidden="true"
        >
          {initial}
        </div>
        <div className="flex-1 min-w-0">
          <p
            className="text-xs font-medium font-mono truncate"
            style={{ color: 'var(--text-primary)' }}
            title={handle}
          >
            {handle}
          </p>
          <p className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
            {isId ? 'Channel ID' : 'Channel handle'}
          </p>
        </div>
      </Card>
    </article>
  )
}

interface VideoCardProps {
  video: VideoCandidate
  onComment: (videoId: string) => void
}

function VideoCard({ video, onComment }: VideoCardProps) {
  const thumbnail = video.meta?.['thumbnail_url']
  const viewCount = formatViewCount(video.meta?.['view_count'])
  const titleTrunc = video.title.length > 90 ? video.title.slice(0, 90) + '…' : video.title
  const meta = [video.author, formatRelativeTime(video.created_at), viewCount]
    .filter(Boolean)
    .join(' · ')

  return (
    <article>
      <Card
        className="overflow-hidden"
        style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
      >
        {thumbnail && (
          <div className="w-full aspect-video overflow-hidden bg-black">
            <img
              src={thumbnail}
              alt=""
              className="w-full h-full object-cover"
              loading="lazy"
            />
          </div>
        )}
        <div className="p-3 flex flex-col gap-2">
          <div>
            <a
              href={video.url}
              target="_blank"
              rel="noopener noreferrer"
              className="text-xs font-medium leading-snug hover:underline block"
              style={{ color: 'var(--text-primary)' }}
              title={video.title}
            >
              {titleTrunc}
            </a>
            <p className="text-[10px] mt-0.5" style={{ color: 'var(--text-secondary)' }}>
              {meta}
            </p>
          </div>
          <Button
            size="sm"
            variant="outline"
            className="h-6 px-2 text-[10px] self-start"
            onClick={() => onComment(video.id)}
          >
            Comment
          </Button>
        </div>
      </Card>
    </article>
  )
}

interface CommentQueueItemProps {
  entry: CommentEntry
}

function CommentQueueItem({ entry }: CommentQueueItemProps) {
  return (
    <Card
      className="p-3 flex flex-col gap-1.5"
      style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
    >
      <div className="flex items-center gap-2">
        <code
          className="text-[10px] font-mono flex-1 truncate"
          style={{ color: 'var(--text-secondary)' }}
          title={entry.videoId}
        >
          {entry.videoId}
        </code>
        <Badge variant={COMMENT_STATUS_VARIANT[entry.status]} className="text-[10px] flex-shrink-0">
          {entry.status}
        </Badge>
      </div>
      <p className="text-xs leading-relaxed" style={{ color: 'var(--text-primary)' }}>
        {entry.text}
      </p>
      {entry.error && (
        <p className="text-[10px]" style={{ color: 'var(--color-error)' }}>
          {entry.error}
        </p>
      )}
      {entry.postedAt && (
        <p className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
          Posted {formatRelativeTime(entry.postedAt)}
        </p>
      )}
    </Card>
  )
}

// ── YouTubeView ──────────────────────────────────────────────────────────────

export default function YouTubeView(_props: PluginViewProps) {
  const [status, setStatus] = useState<YoutubeStatus | null>(null)
  const [channels, setChannels] = useState<ChannelItem[]>([])
  const [videos, setVideos] = useState<VideoCandidate[]>([])
  const [comments, setComments] = useState<CommentEntry[]>([])
  const [loading, setLoading] = useState(true)
  const [scanning, setScanning] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [feedback, setFeedback] = useState<string | null>(null)
  const [activeTab, setActiveTab] = useState('channels')

  const [scanTopic, setScanTopic] = useState('')
  const [commentVideoId, setCommentVideoId] = useState('')
  const [commentText, setCommentText] = useState('')
  const [commentBusy, setCommentBusy] = useState(false)

  const loadData = useCallback(async () => {
    try {
      const [statusRes, itemsRes] = await Promise.allSettled([
        queryPluginStatus(PLUGIN),
        queryPluginItems(PLUGIN),
      ])
      if (statusRes.status === 'fulfilled') {
        setStatus(statusRes.value as unknown as YoutubeStatus)
      }
      if (itemsRes.status === 'fulfilled') {
        setChannels(itemsRes.value as unknown as ChannelItem[])
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
    const id = setInterval(() => { void loadData() }, POLL_MS)
    return () => clearInterval(id)
  }, [loadData])

  const showFeedback = useCallback((msg: string, isError = false) => {
    setFeedback(isError ? `error: ${msg}` : msg)
    setTimeout(() => setFeedback(null), 4_000)
  }, [])

  const handleScan = useCallback(async () => {
    setScanning(true)
    setFeedback(null)
    try {
      const result = await pluginAction(PLUGIN, 'scan', scanTopic.trim())
      const raw = result as unknown
      const candidates: VideoCandidate[] = Array.isArray(raw)
        ? (raw as VideoCandidate[])
        : Array.isArray((raw as { items?: unknown[] }).items)
          ? ((raw as { items: VideoCandidate[] }).items)
          : []
      setVideos(candidates)
      setActiveTab('videos')
      showFeedback(
        candidates.length === 0
          ? 'Scan returned no results'
          : `Found ${candidates.length} video${candidates.length === 1 ? '' : 's'}`
      )
    } catch (err) {
      showFeedback(err instanceof Error ? err.message : String(err), true)
    } finally {
      setScanning(false)
    }
  }, [scanTopic, showFeedback])

  const handleGoToComment = useCallback((videoId: string) => {
    setCommentVideoId(videoId)
    setActiveTab('comments')
  }, [])

  const handlePostComment = useCallback(async () => {
    const vid = commentVideoId.trim()
    const txt = commentText.trim()
    if (!vid || !txt) return

    const entry: CommentEntry = {
      sessionId: nextSessionId(),
      videoId: vid,
      text: txt,
      status: 'posting',
    }
    setComments(prev => [entry, ...prev])
    setCommentVideoId('')
    setCommentText('')
    setCommentBusy(true)

    try {
      // Encode video ID and comment text as JSON in item_id.
      // The youtube plugin's "comment" action handler must parse this format.
      const itemId = JSON.stringify({ video_id: vid, text: txt })
      const result = await pluginAction(PLUGIN, 'comment', itemId)
      const ok = (result as { ok?: boolean }).ok !== false
      setComments(prev =>
        prev.map(e =>
          e.sessionId === entry.sessionId
            ? {
                ...e,
                status: ok ? 'posted' : 'failed',
                postedAt: ok ? new Date().toISOString() : undefined,
                error: ok ? undefined : String((result as { error?: unknown }).error ?? 'action failed'),
              }
            : e
        )
      )
      showFeedback(ok ? 'Comment posted' : 'Comment failed', !ok)
    } catch (err) {
      setComments(prev =>
        prev.map(e =>
          e.sessionId === entry.sessionId
            ? { ...e, status: 'failed', error: err instanceof Error ? err.message : String(err) }
            : e
        )
      )
      showFeedback(err instanceof Error ? err.message : String(err), true)
    } finally {
      setCommentBusy(false)
    }
  }, [commentVideoId, commentText, showFeedback])

  if (loading) {
    return (
      <div className="flex flex-col gap-4 p-4 animate-pulse">
        <div className="h-16 rounded" style={{ background: 'var(--mic-bg)' }} />
        <div className="grid grid-cols-2 gap-2">
          {[1, 2, 3, 4].map(i => (
            <div key={i} className="h-14 rounded" style={{ background: 'var(--mic-bg)' }} />
          ))}
        </div>
      </div>
    )
  }

  const feedbackIsError = feedback?.startsWith('error:') ?? false

  return (
    <div className="flex flex-col gap-3 p-4">
      {/* OAuth indicator + quota bar */}
      {status && (
        <div
          className="rounded-lg p-3 flex flex-col gap-3"
          style={{ background: 'var(--mic-bg)', border: '1px solid var(--pill-border)' }}
        >
          <OAuthIndicator
            authenticated={status.authenticated}
            hasApiKey={status.has_api_key}
            configured={status.configured}
          />
          <QuotaBar budget={status.budget || 10_000} />
        </div>
      )}

      {/* Error banner */}
      {error && (
        <div className="flex items-center gap-2">
          <Badge variant="destructive" className="text-[10px]">error</Badge>
          <p className="text-xs font-mono flex-1 truncate" style={{ color: 'var(--color-error)' }}>{error}</p>
          <Button variant="outline" size="sm" className="h-6 px-2 text-[10px] flex-shrink-0" onClick={() => void loadData()}>
            Retry
          </Button>
        </div>
      )}

      {/* Feedback toast */}
      {feedback && (
        <p
          className="text-xs px-2 py-1 rounded"
          style={{
            background: feedbackIsError
              ? 'color-mix(in srgb, var(--color-error) 12%, transparent)'
              : 'color-mix(in srgb, var(--color-success) 12%, transparent)',
            color: feedbackIsError ? 'var(--color-error)' : 'var(--color-success)',
          }}
          role="status"
          aria-live="polite"
        >
          {feedbackIsError ? feedback.replace(/^error: /, '') : feedback}
        </p>
      )}

      {/* Tabs */}
      <Tabs value={activeTab} onValueChange={setActiveTab}>
        <TabsList className="w-full grid grid-cols-4 h-8">
          <TabsTrigger value="channels" className="text-[11px]">
            Channels
            {channels.length > 0 && (
              <span className="ml-1 text-[9px] opacity-60">{channels.length}</span>
            )}
          </TabsTrigger>
          <TabsTrigger value="videos" className="text-[11px]">
            Videos
            {videos.length > 0 && (
              <span className="ml-1 text-[9px] opacity-60">{videos.length}</span>
            )}
          </TabsTrigger>
          <TabsTrigger value="comments" className="text-[11px]">
            Comments
            {comments.length > 0 && (
              <span className="ml-1 text-[9px] opacity-60">{comments.length}</span>
            )}
          </TabsTrigger>
          <TabsTrigger value="config" className="text-[11px]">Config</TabsTrigger>
        </TabsList>

        {/* ── Channels ──────────────────────────────────────────────────── */}
        <TabsContent value="channels" className="mt-3">
          {channels.length === 0 ? (
            <div className="py-6 text-center flex flex-col gap-1">
              <p className="text-sm" style={{ color: 'var(--text-secondary)' }}>
                No channels configured.
              </p>
              <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
                Run{' '}
                <code className="font-mono" style={{ color: 'var(--text-primary)' }}>
                  youtube configure
                </code>{' '}
                to add channels.
              </p>
            </div>
          ) : (
            <div className="flex flex-col gap-2">
              {channels.map(ch => (
                <ChannelCard key={ch.value} channel={ch} />
              ))}
            </div>
          )}
        </TabsContent>

        {/* ── Videos ────────────────────────────────────────────────────── */}
        <TabsContent value="videos" className="mt-3">
          <div className="flex flex-col gap-3">
            {/* Scan trigger */}
            <div
              className="rounded-lg p-3 flex flex-col gap-2"
              style={{ background: 'var(--mic-bg)', border: '1px solid var(--pill-border)' }}
            >
              <label
                htmlFor="yt-scan-topic"
                className="text-[10px] uppercase tracking-wider"
                style={{ color: 'var(--text-secondary)' }}
              >
                Scan Topic
              </label>
              <div className="flex gap-2">
                <input
                  id="yt-scan-topic"
                  type="text"
                  value={scanTopic}
                  onChange={e => setScanTopic(e.target.value)}
                  onKeyDown={e => { if (e.key === 'Enter' && !scanning) void handleScan() }}
                  placeholder="golang cli, typescript…"
                  className="flex-1 min-w-0 rounded text-xs px-2 py-1.5 outline-none focus:ring-1"
                  style={{
                    background: 'color-mix(in srgb, var(--mic-bg) 60%, transparent)',
                    border: '1px solid var(--pill-border)',
                    color: 'var(--text-primary)',
                  }}
                  aria-describedby="yt-scan-hint"
                  disabled={scanning}
                />
                <Button
                  size="sm"
                  variant="outline"
                  className="h-7 px-3 text-[11px] flex-shrink-0"
                  disabled={scanning}
                  onClick={() => void handleScan()}
                >
                  {scanning ? 'Scanning…' : 'Scan'}
                </Button>
              </div>
              <p id="yt-scan-hint" className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                Leave blank to scan configured channels. Comma-separated for multiple topics.
              </p>
            </div>

            {/* Video results */}
            {videos.length === 0 ? (
              <p className="text-sm py-2" style={{ color: 'var(--text-secondary)' }}>
                No results yet — trigger a scan above.
              </p>
            ) : (
              <div className="flex flex-col gap-2">
                {videos.map(v => (
                  <VideoCard key={v.id} video={v} onComment={handleGoToComment} />
                ))}
              </div>
            )}
          </div>
        </TabsContent>

        {/* ── Comments ──────────────────────────────────────────────────── */}
        <TabsContent value="comments" className="mt-3">
          <div className="flex flex-col gap-3">
            {/* Post comment form */}
            <div
              className="rounded-lg p-3 flex flex-col gap-2"
              style={{ background: 'var(--mic-bg)', border: '1px solid var(--pill-border)' }}
            >
              <h3
                className="text-[10px] uppercase tracking-wider"
                style={{ color: 'var(--text-secondary)' }}
              >
                Post Comment
              </h3>

              <div className="flex flex-col gap-1">
                <label htmlFor="yt-comment-video-id" className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                  Video ID
                </label>
                <input
                  id="yt-comment-video-id"
                  type="text"
                  value={commentVideoId}
                  onChange={e => setCommentVideoId(e.target.value)}
                  placeholder="dQw4w9WgXcQ"
                  className="rounded text-xs font-mono px-2 py-1.5 outline-none focus:ring-1"
                  style={{
                    background: 'color-mix(in srgb, var(--mic-bg) 60%, transparent)',
                    border: '1px solid var(--pill-border)',
                    color: 'var(--text-primary)',
                  }}
                  disabled={commentBusy}
                />
              </div>

              <div className="flex flex-col gap-1">
                <label htmlFor="yt-comment-text" className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                  Comment
                </label>
                <textarea
                  id="yt-comment-text"
                  value={commentText}
                  onChange={e => setCommentText(e.target.value)}
                  placeholder="Great video!"
                  rows={3}
                  className="rounded text-xs px-2 py-1.5 outline-none focus:ring-1 resize-none"
                  style={{
                    background: 'color-mix(in srgb, var(--mic-bg) 60%, transparent)',
                    border: '1px solid var(--pill-border)',
                    color: 'var(--text-primary)',
                  }}
                  disabled={commentBusy}
                />
              </div>

              <div className="flex items-center justify-between gap-2 pt-0.5">
                {!status?.authenticated && (
                  <p className="text-[10px]" style={{ color: 'var(--color-warning)' }}>
                    Run{' '}
                    <code className="font-mono" style={{ color: 'var(--text-primary)' }}>
                      youtube auth
                    </code>{' '}
                    to enable posting
                  </p>
                )}
                <Button
                  size="sm"
                  variant="outline"
                  className="h-7 px-3 text-[11px] ml-auto"
                  disabled={
                    commentBusy ||
                    !commentVideoId.trim() ||
                    !commentText.trim() ||
                    !status?.authenticated
                  }
                  onClick={() => void handlePostComment()}
                >
                  {commentBusy ? 'Posting…' : 'Post'}
                </Button>
              </div>
            </div>

            {/* Session comment history */}
            {comments.length > 0 && (
              <section aria-label="Comment session history">
                <h3
                  className="text-[10px] uppercase tracking-wider mb-2"
                  style={{ color: 'var(--text-secondary)' }}
                >
                  Session History
                  <span
                    className="ml-1.5 inline-flex items-center justify-center w-4 h-4 rounded-full text-[9px] font-bold"
                    style={{ background: 'var(--pill-border)', color: 'var(--text-primary)' }}
                    aria-label={`${comments.length} comments`}
                  >
                    {comments.length}
                  </span>
                </h3>
                <div className="flex flex-col gap-2">
                  {comments.map(entry => (
                    <CommentQueueItem key={entry.sessionId} entry={entry} />
                  ))}
                </div>
              </section>
            )}

            {comments.length === 0 && (
              <p className="text-sm py-2" style={{ color: 'var(--text-secondary)' }}>
                No comments posted this session.
              </p>
            )}
          </div>
        </TabsContent>

        {/* ── Config ────────────────────────────────────────────────────── */}
        <TabsContent value="config" className="mt-3">
          {status ? (
            <div className="flex flex-col gap-3">
              {/* Stat cards */}
              <dl className="grid grid-cols-2 gap-2">
                <Card
                  className="p-3 flex flex-col gap-0.5"
                  style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
                >
                  <dt className="text-[10px] uppercase tracking-wider" style={{ color: 'var(--text-secondary)' }}>
                    Channels
                  </dt>
                  <dd className="text-xl font-mono font-bold leading-none" style={{ color: 'var(--text-primary)' }}>
                    {status.channel_count}
                  </dd>
                </Card>
                <Card
                  className="p-3 flex flex-col gap-0.5"
                  style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
                >
                  <dt className="text-[10px] uppercase tracking-wider" style={{ color: 'var(--text-secondary)' }}>
                    Budget
                  </dt>
                  <dd className="text-xl font-mono font-bold leading-none" style={{ color: 'var(--text-primary)' }}>
                    {(status.budget || 10_000).toLocaleString()}
                  </dd>
                </Card>
              </dl>

              {/* Status checklist */}
              <div
                className="rounded-lg p-3 flex flex-col gap-2.5"
                style={{ background: 'var(--mic-bg)', border: '1px solid var(--pill-border)' }}
              >
                {[
                  { label: 'Configuration', ok: status.configured, okLabel: 'ready', failLabel: 'missing' },
                  { label: 'API Key', ok: status.has_api_key, okLabel: 'present', failLabel: 'missing' },
                ].map(({ label, ok, okLabel, failLabel }) => (
                  <div key={label} className="flex items-center justify-between">
                    <span className="text-xs" style={{ color: 'var(--text-secondary)' }}>{label}</span>
                    <Badge variant={ok ? 'secondary' : 'destructive'} className="text-[10px]">
                      {ok ? okLabel : failLabel}
                    </Badge>
                  </div>
                ))}
                <div className="flex items-center justify-between">
                  <span className="text-xs" style={{ color: 'var(--text-secondary)' }}>OAuth Token</span>
                  <span
                    className="text-[10px] font-medium px-2 py-0.5 rounded-full"
                    style={{
                      background: status.authenticated
                        ? 'color-mix(in srgb, var(--color-success) 15%, transparent)'
                        : 'color-mix(in srgb, var(--color-warning) 15%, transparent)',
                      color: status.authenticated ? 'var(--color-success)' : 'var(--color-warning)',
                    }}
                  >
                    {status.authenticated ? 'active' : 'not authenticated'}
                  </span>
                </div>
              </div>

              {/* CLI quick reference */}
              <div
                className="rounded-lg p-3"
                style={{ background: 'var(--mic-bg)', border: '1px solid var(--pill-border)' }}
              >
                <h3
                  className="text-[10px] uppercase tracking-wider mb-2"
                  style={{ color: 'var(--text-secondary)' }}
                >
                  CLI Reference
                </h3>
                <div className="flex flex-col gap-1.5">
                  {[
                    ['youtube configure', 'Set up API key, OAuth credentials, and channels'],
                    ['youtube auth', 'Authenticate via OAuth for posting'],
                    ['youtube doctor', 'Check config health and quota usage'],
                    ['youtube scan', 'Scan configured channels for recent videos'],
                    ['youtube comment <id> "text"', 'Post a comment (50 quota units)'],
                    ['youtube like <id>', 'Like a video (50 quota units)'],
                  ].map(([cmd, desc]) => (
                    <div key={cmd} className="flex items-baseline gap-2 flex-wrap">
                      <code
                        className="text-[10px] font-mono flex-shrink-0"
                        style={{ color: 'var(--text-primary)' }}
                      >
                        {cmd}
                      </code>
                      <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                        {desc}
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            </div>
          ) : (
            <p className="text-sm py-6 text-center" style={{ color: 'var(--text-secondary)' }}>
              Status unavailable.
            </p>
          )}
        </TabsContent>
      </Tabs>

      {/* Refresh */}
      <div className="flex justify-end">
        <Button
          size="sm"
          variant="outline"
          className="h-6 px-2 text-[10px]"
          onClick={() => void loadData()}
          aria-label="Refresh YouTube plugin data"
        >
          Refresh
        </Button>
      </div>
    </div>
  )
}
