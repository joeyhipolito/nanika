import { useState, useEffect, useCallback, useRef } from 'react'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { queryPluginStatus, queryPluginItems, pluginAction } from '@/lib/wails'
import type { PluginViewProps } from '@/types'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type Visibility = 'PUBLIC' | 'CONNECTIONS'

interface LinkedInStatus {
  configured: boolean
  authenticated: boolean
  person_urn: string
  token_expiry: string
  chrome_debug_configured: boolean
}

interface LinkedInPost {
  id: string
  urn: string
  text: string
  created_at: string
  visibility: string
  like_count: number
  comment_count: number
  share_count: number
  impression_count: number
}

interface FeedItem {
  urn: string
  actor: string
  text: string
  created_at: string
  like_count: number
  comment_count: number
  has_reacted: boolean
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const PLUGIN_NAME = 'linkedin'
const MAX_CHARS = 3000

const REACTION_TYPES = ['LIKE', 'CELEBRATE', 'EMPATHY', 'INTEREST', 'APPRECIATION'] as const
type ReactionType = typeof REACTION_TYPES[number]

const REACTION_LABELS: Record<ReactionType, string> = {
  LIKE: '👍',
  CELEBRATE: '🎉',
  EMPATHY: '❤️',
  INTEREST: '💡',
  APPRECIATION: '🙏',
}

// ---------------------------------------------------------------------------
// Pill — inline badge replacement (avoids cross-tsconfig Badge type errors)
// ---------------------------------------------------------------------------

type PillVariant = 'default' | 'secondary' | 'destructive' | 'outline'

const PILL_STYLES: Record<PillVariant, React.CSSProperties> = {
  default: {
    background: 'var(--accent)',
    color: 'var(--bg)',
    border: '1px solid transparent',
  },
  secondary: {
    background: 'color-mix(in srgb, var(--text-secondary) 15%, transparent)',
    color: 'var(--text-secondary)',
    border: '1px solid transparent',
  },
  destructive: {
    background: 'color-mix(in srgb, var(--color-error) 15%, transparent)',
    color: 'var(--color-error)',
    border: '1px solid transparent',
  },
  outline: {
    background: 'transparent',
    color: 'var(--text-secondary)',
    border: '1px solid var(--pill-border)',
  },
}

interface PillProps {
  variant?: PillVariant
  className?: string
  children: React.ReactNode
}

function Pill({ variant = 'default', className, children }: PillProps) {
  return (
    <span
      className={`inline-flex items-center rounded-full px-2.5 py-0.5 text-xs font-semibold ${className ?? ''}`}
      style={PILL_STYLES[variant]}
    >
      {children}
    </span>
  )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatRelativeTime(dateStr: string): string {
  if (!dateStr) return ''
  const diffMs = Date.now() - new Date(dateStr).getTime()
  const min = Math.floor(diffMs / 60_000)
  if (min < 1) return 'just now'
  if (min < 60) return `${min}m ago`
  const h = Math.floor(min / 60)
  if (h < 24) return `${h}h ago`
  const d = Math.floor(h / 24)
  if (d < 30) return `${d}d ago`
  return new Date(dateStr).toLocaleDateString([], { month: 'short', day: 'numeric' })
}

function formatTokenExpiry(expiry: string): string {
  if (!expiry) return 'unknown'
  try {
    const d = new Date(expiry)
    const diffDays = Math.floor((d.getTime() - Date.now()) / 86_400_000)
    if (diffDays < 0) return 'expired'
    if (diffDays === 0) return 'expires today'
    if (diffDays === 1) return 'expires tomorrow'
    return `expires in ${diffDays}d`
  } catch {
    return expiry
  }
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function Feedback({ message }: { message: string | null }) {
  if (!message) return null
  const isError = message.includes('failed') || message.includes('error') || message.includes('Error')
  return (
    <p
      className="rounded px-2 py-1 text-xs"
      role="status"
      aria-live="polite"
      style={{
        background: isError
          ? 'color-mix(in srgb, var(--color-error) 12%, transparent)'
          : 'color-mix(in srgb, var(--color-success) 12%, transparent)',
        color: isError ? 'var(--color-error)' : 'var(--color-success)',
      }}
    >
      {message}
    </p>
  )
}

interface StatChipProps {
  label: string
  value: number
}

function StatChip({ label, value }: StatChipProps) {
  return (
    <span
      className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px]"
      style={{
        background: 'color-mix(in srgb, var(--text-secondary) 10%, transparent)',
        color: 'var(--text-secondary)',
      }}
    >
      {label} {value.toLocaleString()}
    </span>
  )
}

// ---------------------------------------------------------------------------
// Compose tab
// ---------------------------------------------------------------------------

interface ComposeTabProps {
  onFeedback: (msg: string) => void
}

function ComposeTab({ onFeedback }: ComposeTabProps) {
  const [text, setText] = useState('')
  const [visibility, setVisibility] = useState<Visibility>('PUBLIC')
  const [posting, setPosting] = useState(false)
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  const charCount = text.length
  const remaining = MAX_CHARS - charCount
  const isOverLimit = remaining < 0
  const isNearLimit = remaining <= 200 && remaining >= 0

  const handlePost = async () => {
    const trimmed = text.trim()
    if (!trimmed || isOverLimit) return
    setPosting(true)
    try {
      const payload = JSON.stringify({ text: trimmed, visibility })
      const result = await pluginAction(PLUGIN_NAME, 'post', payload)
      const ok = (result as { ok?: boolean }).ok !== false
      if (ok) {
        setText('')
        onFeedback('Post published successfully')
      } else {
        onFeedback(`Post failed: ${String((result as { error?: string }).error ?? 'unknown')}`)
      }
    } catch (err) {
      onFeedback(`Post error: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      setPosting(false)
    }
  }

  return (
    <div className="flex flex-col gap-3">
      {/* Textarea */}
      <div className="relative">
        <textarea
          ref={textareaRef}
          value={text}
          onChange={e => setText(e.target.value)}
          placeholder="What do you want to talk about?"
          rows={6}
          className="w-full resize-none rounded-md border p-3 text-sm leading-relaxed focus:outline-none focus:ring-2"
          style={{
            background: 'var(--mic-bg)',
            borderColor: isOverLimit ? 'var(--color-error)' : 'var(--pill-border)',
            color: 'var(--text-primary)',
            outline: 'none',
          }}
          aria-label="Post content"
          aria-describedby="char-count"
          maxLength={MAX_CHARS + 50}
        />
      </div>

      {/* Controls row */}
      <div className="flex items-center justify-between gap-3">
        {/* Visibility select */}
        <select
          value={visibility}
          onChange={e => setVisibility(e.target.value as Visibility)}
          className="h-8 w-36 rounded border px-2 text-xs focus:outline-none"
          style={{
            background: 'var(--mic-bg)',
            borderColor: 'var(--pill-border)',
            color: 'var(--text-primary)',
          }}
          aria-label="Post visibility"
        >
          <option value="PUBLIC">Public</option>
          <option value="CONNECTIONS">Connections</option>
        </select>

        {/* Char counter + post button */}
        <div className="flex items-center gap-3">
          <span
            id="char-count"
            className="text-xs font-mono tabular-nums"
            style={{
              color: isOverLimit
                ? 'var(--color-error)'
                : isNearLimit
                  ? 'var(--color-warning)'
                  : 'var(--text-secondary)',
            }}
            aria-label={`${remaining} characters remaining`}
          >
            {isOverLimit ? `-${Math.abs(remaining)}` : remaining}
          </span>
          <Button
            size="sm"
            disabled={posting || !text.trim() || isOverLimit}
            onClick={() => void handlePost()}
            aria-busy={posting}
          >
            {posting ? 'Posting…' : 'Post'}
          </Button>
        </div>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Posts tab
// ---------------------------------------------------------------------------

interface PostsTabProps {
  posts: LinkedInPost[] | null
  loading: boolean
  onRefresh: () => void
  onFeedback: (msg: string) => void
}

function PostsTab({ posts, loading, onRefresh, onFeedback: _ }: PostsTabProps) {
  if (loading && !posts) {
    return (
      <div className="flex flex-col gap-2 animate-pulse">
        {[1, 2, 3].map(i => (
          <div key={i} className="h-20 rounded" style={{ background: 'var(--mic-bg)' }} />
        ))}
      </div>
    )
  }

  if (!posts || posts.length === 0) {
    return (
      <div className="flex flex-col gap-3">
        <p className="text-sm py-4 text-center" style={{ color: 'var(--text-secondary)' }}>
          No recent posts found.
        </p>
        <div className="flex justify-end">
          <Button size="sm" variant="outline" onClick={onRefresh} disabled={loading}>
            {loading ? 'Loading…' : 'Refresh'}
          </Button>
        </div>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-3">
      {posts.map(post => (
        <article key={post.id}>
          <Card
            className="p-3 flex flex-col gap-2"
            style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
          >
            {/* Header */}
            <div className="flex items-center justify-between gap-2">
              <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                {formatRelativeTime(post.created_at)}
              </span>
              <Pill variant="outline" className="text-[10px] capitalize">
                {(post.visibility ?? 'PUBLIC').toLowerCase()}
              </Pill>
            </div>

            {/* Text preview */}
            <p
              className="text-xs leading-relaxed line-clamp-3"
              style={{ color: 'var(--text-primary)' }}
              title={post.text}
            >
              {post.text || <em style={{ color: 'var(--text-secondary)' }}>No text</em>}
            </p>

            {/* Stats */}
            <div className="flex flex-wrap gap-1.5">
              {post.like_count > 0 && <StatChip label="👍" value={post.like_count} />}
              {post.comment_count > 0 && <StatChip label="💬" value={post.comment_count} />}
              {post.share_count > 0 && <StatChip label="🔁" value={post.share_count} />}
              {post.impression_count > 0 && <StatChip label="👁" value={post.impression_count} />}
            </div>
          </Card>
        </article>
      ))}

      <div className="flex justify-end">
        <Button size="sm" variant="outline" onClick={onRefresh} disabled={loading}>
          {loading ? 'Loading…' : 'Refresh'}
        </Button>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Feed tab
// ---------------------------------------------------------------------------

interface FeedTabProps {
  feed: FeedItem[] | null
  loading: boolean
  onRefresh: () => void
  onFeedback: (msg: string) => void
}

function FeedTab({ feed, loading, onRefresh, onFeedback }: FeedTabProps) {
  const [busy, setBusy] = useState<string | null>(null)
  const [commentingUrn, setCommentingUrn] = useState<string | null>(null)
  const [commentText, setCommentText] = useState('')

  const handleReact = useCallback(async (urn: string, type: ReactionType) => {
    setBusy(`react:${urn}`)
    try {
      await pluginAction(PLUGIN_NAME, 'react', JSON.stringify({ urn, type }))
      onFeedback(`Reacted ${REACTION_LABELS[type]} to post`)
    } catch (err) {
      onFeedback(`React failed: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      setBusy(null)
    }
  }, [onFeedback])

  const handleComment = useCallback(async (urn: string) => {
    const trimmed = commentText.trim()
    if (!trimmed) return
    setBusy(`comment:${urn}`)
    try {
      await pluginAction(PLUGIN_NAME, 'comment', JSON.stringify({ urn, text: trimmed }))
      onFeedback('Comment posted')
      setCommentingUrn(null)
      setCommentText('')
    } catch (err) {
      onFeedback(`Comment failed: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      setBusy(null)
    }
  }, [commentText, onFeedback])

  if (loading && !feed) {
    return (
      <div className="flex flex-col gap-2 animate-pulse">
        {[1, 2, 3, 4].map(i => (
          <div key={i} className="h-24 rounded" style={{ background: 'var(--mic-bg)' }} />
        ))}
      </div>
    )
  }

  if (!feed || feed.length === 0) {
    return (
      <div className="flex flex-col gap-3">
        <p className="text-xs py-2" style={{ color: 'var(--text-secondary)' }}>
          Feed requires Chrome with remote debugging. Run{' '}
          <code
            className="rounded px-1 py-0.5"
            style={{ background: 'var(--mic-bg)', color: 'var(--text-primary)' }}
          >
            linkedin chrome --launch
          </code>
          , then refresh.
        </p>
        <div className="flex justify-end">
          <Button size="sm" variant="outline" onClick={onRefresh} disabled={loading}>
            {loading ? 'Loading…' : 'Refresh'}
          </Button>
        </div>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-3">
      {feed.map(item => {
        const isCommentingThis = commentingUrn === item.urn
        const isReactBusy = busy === `react:${item.urn}`
        const isCommentBusy = busy === `comment:${item.urn}`

        return (
          <article key={item.urn}>
            <Card
              className="p-3 flex flex-col gap-2"
              style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
            >
              {/* Actor + time */}
              <div className="flex items-baseline justify-between gap-2">
                <span
                  className="text-xs font-medium truncate"
                  style={{ color: 'var(--text-primary)' }}
                >
                  {item.actor || 'LinkedIn member'}
                </span>
                <span className="text-[10px] shrink-0" style={{ color: 'var(--text-secondary)' }}>
                  {formatRelativeTime(item.created_at)}
                </span>
              </div>

              {/* Post text */}
              <p
                className="text-xs leading-relaxed line-clamp-4"
                style={{ color: 'var(--text-secondary)' }}
                title={item.text}
              >
                {item.text || <em>No text</em>}
              </p>

              {/* Engagement stats */}
              <div className="flex gap-1.5">
                {item.like_count > 0 && <StatChip label="👍" value={item.like_count} />}
                {item.comment_count > 0 && <StatChip label="💬" value={item.comment_count} />}
              </div>

              {/* Actions */}
              <div className="flex flex-wrap items-center gap-1.5 pt-0.5">
                {REACTION_TYPES.map(type => (
                  <button
                    key={type}
                    disabled={isReactBusy || isCommentBusy}
                    onClick={() => void handleReact(item.urn, type)}
                    className="rounded px-1.5 py-0.5 text-xs transition-opacity hover:opacity-70 disabled:opacity-40"
                    style={{
                      background: item.has_reacted
                        ? 'color-mix(in srgb, var(--accent) 15%, transparent)'
                        : 'color-mix(in srgb, var(--text-secondary) 10%, transparent)',
                      border: `1px solid ${item.has_reacted ? 'var(--accent)' : 'var(--pill-border)'}`,
                    }}
                    aria-label={`React with ${type}`}
                    title={type}
                  >
                    {isReactBusy ? '…' : REACTION_LABELS[type]}
                  </button>
                ))}

                <button
                  disabled={isReactBusy || isCommentBusy}
                  onClick={() => {
                    setCommentingUrn(prev => (prev === item.urn ? null : item.urn))
                    setCommentText('')
                  }}
                  className="rounded px-2 py-0.5 text-[10px] font-medium transition-opacity hover:opacity-70 disabled:opacity-40"
                  style={{
                    background: isCommentingThis
                      ? 'color-mix(in srgb, var(--accent) 15%, transparent)'
                      : 'color-mix(in srgb, var(--text-secondary) 10%, transparent)',
                    border: `1px solid ${isCommentingThis ? 'var(--accent)' : 'var(--pill-border)'}`,
                    color: isCommentingThis ? 'var(--accent)' : 'var(--text-secondary)',
                  }}
                  aria-expanded={isCommentingThis}
                  aria-label="Toggle comment box"
                >
                  💬 Comment
                </button>
              </div>

              {/* Inline comment box */}
              {isCommentingThis && (
                <div className="flex flex-col gap-1.5 pt-1">
                  <textarea
                    value={commentText}
                    onChange={e => setCommentText(e.target.value)}
                    placeholder="Write a comment…"
                    rows={2}
                    className="w-full resize-none rounded border p-2 text-xs leading-relaxed focus:outline-none"
                    style={{
                      background: 'var(--bg)',
                      borderColor: 'var(--pill-border)',
                      color: 'var(--text-primary)',
                    }}
                    aria-label="Comment text"
                    autoFocus
                  />
                  <div className="flex justify-end gap-1.5">
                    <Button
                      size="sm"
                      variant="outline"
                      className="h-6 px-2 text-[10px]"
                      onClick={() => { setCommentingUrn(null); setCommentText('') }}
                      disabled={isCommentBusy}
                    >
                      Cancel
                    </Button>
                    <Button
                      size="sm"
                      className="h-6 px-2 text-[10px]"
                      disabled={isCommentBusy || !commentText.trim()}
                      onClick={() => void handleComment(item.urn)}
                      aria-busy={isCommentBusy}
                    >
                      {isCommentBusy ? '…' : 'Post'}
                    </Button>
                  </div>
                </div>
              )}
            </Card>
          </article>
        )
      })}

      <div className="flex justify-end">
        <Button size="sm" variant="outline" onClick={onRefresh} disabled={loading}>
          {loading ? 'Loading…' : 'Refresh'}
        </Button>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Profile tab
// ---------------------------------------------------------------------------

interface ProfileTabProps {
  status: LinkedInStatus | null
  loading: boolean
  onRefresh: () => void
}

function ProfileTab({ status, loading, onRefresh }: ProfileTabProps) {
  if (loading && !status) {
    return (
      <div className="flex flex-col gap-2 animate-pulse">
        {[1, 2, 3].map(i => (
          <div key={i} className="h-10 rounded" style={{ background: 'var(--mic-bg)' }} />
        ))}
      </div>
    )
  }

  if (!status) {
    return (
      <p className="text-sm py-4 text-center" style={{ color: 'var(--text-secondary)' }}>
        Status unavailable.{' '}
        <button
          className="underline"
          style={{ color: 'var(--accent)' }}
          onClick={onRefresh}
        >
          Retry
        </button>
      </p>
    )
  }

  return (
    <div className="flex flex-col gap-3">
      <Card
        className="p-4 flex flex-col gap-3"
        style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
      >
        {/* Auth status */}
        <div className="flex items-center justify-between">
          <span className="text-xs font-medium" style={{ color: 'var(--text-secondary)' }}>
            OAuth
          </span>
          <Pill
            variant={status.authenticated ? 'secondary' : 'destructive'}
            className="text-[10px]"
          >
            {status.authenticated ? 'authenticated' : 'not authenticated'}
          </Pill>
        </div>

        {/* Configured */}
        <div className="flex items-center justify-between">
          <span className="text-xs font-medium" style={{ color: 'var(--text-secondary)' }}>
            Configured
          </span>
          <Pill
            variant={status.configured ? 'secondary' : 'outline'}
            className="text-[10px]"
          >
            {status.configured ? 'yes' : 'no'}
          </Pill>
        </div>

        {/* Person URN */}
        {status.person_urn && (
          <div className="flex items-start justify-between gap-4">
            <span className="text-xs font-medium shrink-0" style={{ color: 'var(--text-secondary)' }}>
              Person URN
            </span>
            <span
              className="text-[10px] font-mono text-right truncate"
              style={{ color: 'var(--text-primary)', maxWidth: '200px' }}
              title={status.person_urn}
            >
              {status.person_urn}
            </span>
          </div>
        )}

        {/* Token expiry */}
        {status.token_expiry && (
          <div className="flex items-center justify-between">
            <span className="text-xs font-medium" style={{ color: 'var(--text-secondary)' }}>
              Token
            </span>
            <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
              {formatTokenExpiry(status.token_expiry)}
            </span>
          </div>
        )}

        {/* Chrome CDP indicator */}
        <div className="flex items-center justify-between">
          <span className="text-xs font-medium" style={{ color: 'var(--text-secondary)' }}>
            Chrome CDP
          </span>
          <div className="flex items-center gap-1.5">
            <span
              className="inline-block w-2 h-2 rounded-full"
              style={{
                background: status.chrome_debug_configured
                  ? 'var(--color-success)'
                  : 'var(--color-error)',
              }}
              aria-hidden="true"
            />
            <span
              className="text-[10px]"
              style={{
                color: status.chrome_debug_configured
                  ? 'var(--color-success)'
                  : 'var(--color-error)',
              }}
            >
              {status.chrome_debug_configured ? 'connected' : 'not connected'}
            </span>
          </div>
        </div>
      </Card>

      {!status.chrome_debug_configured && (
        <p className="text-[10px] leading-relaxed" style={{ color: 'var(--text-secondary)' }}>
          Feed reading requires Chrome with remote debugging. Run{' '}
          <code
            className="rounded px-1 py-0.5"
            style={{ background: 'var(--mic-bg)', color: 'var(--text-primary)' }}
          >
            linkedin chrome --launch
          </code>{' '}
          to start it.
        </p>
      )}

      <div className="flex justify-end">
        <Button size="sm" variant="outline" onClick={onRefresh} disabled={loading}>
          {loading ? 'Loading…' : 'Refresh'}
        </Button>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// LinkedInView — root
// ---------------------------------------------------------------------------

type TabKey = 'compose' | 'posts' | 'feed' | 'profile'
const TABS: TabKey[] = ['compose', 'posts', 'feed', 'profile']

export default function LinkedInView(_props: PluginViewProps) {
  const [status, setStatus] = useState<LinkedInStatus | null>(null)
  const [posts, setPosts] = useState<LinkedInPost[] | null>(null)
  const [feed, setFeed] = useState<FeedItem[] | null>(null)
  const [loading, setLoading] = useState(true)
  const [feedback, setFeedback] = useState<string | null>(null)
  const [activeTab, setActiveTab] = useState<TabKey>('compose')

  const pushFeedback = useCallback((msg: string) => {
    setFeedback(msg)
    setTimeout(() => setFeedback(null), 4_000)
  }, [])

  const loadAll = useCallback(async () => {
    setLoading(true)
    try {
      const [statusRes, itemsRes] = await Promise.allSettled([
        queryPluginStatus(PLUGIN_NAME),
        queryPluginItems(PLUGIN_NAME),
      ])
      if (statusRes.status === 'fulfilled') {
        setStatus(statusRes.value as unknown as LinkedInStatus)
      }
      if (itemsRes.status === 'fulfilled') {
        const val = itemsRes.value as unknown
        if (val && typeof val === 'object' && 'posts' in val) {
          const data = val as { posts?: LinkedInPost[]; feed?: FeedItem[] }
          setPosts(data.posts ?? null)
          setFeed(data.feed ?? null)
        } else if (Array.isArray(val)) {
          setPosts(val as LinkedInPost[])
        }
      }
    } catch {
      // errors handled per-tab
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void loadAll()
    const id = setInterval(() => void loadAll(), 60_000)
    return () => clearInterval(id)
  }, [loadAll])

  return (
    <div className="flex flex-col gap-3 p-4">
      {/* Global feedback banner */}
      <Feedback message={feedback} />

      {/* Tab bar */}
      <div
        role="tablist"
        className="flex h-8 w-full rounded p-0.5"
        style={{
          background: 'var(--mic-bg)',
          borderBottom: '1px solid var(--pill-border)',
        }}
      >
        {TABS.map(tab => (
          <button
            key={tab}
            role="tab"
            aria-selected={activeTab === tab}
            onClick={() => setActiveTab(tab)}
            className="h-7 px-3 text-xs capitalize rounded transition-colors"
            style={{
              color: activeTab === tab ? 'var(--text-primary)' : 'var(--text-secondary)',
              background: activeTab === tab
                ? 'color-mix(in srgb, var(--text-secondary) 12%, transparent)'
                : 'transparent',
              fontWeight: activeTab === tab ? 500 : 400,
            }}
          >
            {tab}
          </button>
        ))}
      </div>

      {/* Tab panels */}
      <div role="tabpanel" className="mt-0">
        {activeTab === 'compose' && (
          <ComposeTab onFeedback={pushFeedback} />
        )}
        {activeTab === 'posts' && (
          <PostsTab
            posts={posts}
            loading={loading}
            onRefresh={() => void loadAll()}
            onFeedback={pushFeedback}
          />
        )}
        {activeTab === 'feed' && (
          <FeedTab
            feed={feed}
            loading={loading}
            onRefresh={() => void loadAll()}
            onFeedback={pushFeedback}
          />
        )}
        {activeTab === 'profile' && (
          <ProfileTab
            status={status}
            loading={loading}
            onRefresh={() => void loadAll()}
          />
        )}
      </div>
    </div>
  )
}
