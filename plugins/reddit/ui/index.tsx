import { useState, useEffect, useCallback, useMemo } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { queryPluginStatus, queryPluginItems, pluginAction } from '@/lib/wails'
import type { PluginViewProps } from '@/types'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface RedditPost {
  id: string
  name: string // t3_xxx
  title: string
  author: string
  subreddit: string
  selftext?: string
  url: string
  permalink: string
  score: number
  upvote_ratio: number
  num_comments: number
  created_utc: number
  is_self: boolean
  stickied: boolean
  liked?: boolean | null
}

interface RedditComment {
  id: string
  name: string // t1_xxx
  author: string
  body: string
  score: number
  created_utc: number
  depth: number
  replies?: RedditComment[]
  liked?: boolean | null
}

interface RedditStatus {
  ok: boolean
  authenticated?: boolean
  username?: string
  subreddits?: string[]
}

type SortKey = 'hot' | 'new' | 'top' | 'rising'

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const PLUGIN_NAME = 'reddit'

const SORT_OPTIONS: { value: SortKey; label: string }[] = [
  { value: 'hot', label: 'Hot' },
  { value: 'new', label: 'New' },
  { value: 'top', label: 'Top' },
  { value: 'rising', label: 'Rising' },
]

const FALLBACK_SUBREDDITS = ['all', 'golang', 'programming', 'webdev', 'rust']

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatScore(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`
  return String(n)
}

function formatAge(utc: number): string {
  const diff = Math.floor(Date.now() / 1000 - utc)
  if (diff < 60) return `${diff}s`
  if (diff < 3600) return `${Math.floor(diff / 60)}m`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`
  return `${Math.floor(diff / 86400)}d`
}

function sortPosts(posts: RedditPost[], sort: SortKey): RedditPost[] {
  const sorted = [...posts]
  if (sort === 'new') return sorted.sort((a, b) => b.created_utc - a.created_utc)
  if (sort === 'top') return sorted.sort((a, b) => b.score - a.score)
  if (sort === 'rising') return sorted.sort((a, b) => b.num_comments - a.num_comments)
  // hot: weighted by recency + score
  return sorted.sort((a, b) => {
    const ageA = Date.now() / 1000 - a.created_utc
    const ageB = Date.now() / 1000 - b.created_utc
    return b.score / Math.pow(ageB + 2, 1.5) - a.score / Math.pow(ageA + 2, 1.5)
  })
}

// ---------------------------------------------------------------------------
// VoteButtons
// ---------------------------------------------------------------------------

interface VoteButtonsProps {
  score: number
  liked: boolean | null | undefined
  onVote: (dir: 'up' | 'down') => void
  busy: boolean
  compact?: boolean
}

function VoteButtons({ score, liked, onVote, busy, compact }: VoteButtonsProps) {
  const size = compact ? 'h-5 w-5 text-[10px]' : 'h-6 w-6 text-xs'
  const upActive = liked === true
  const downActive = liked === false
  return (
    <div
      className="flex flex-col items-center gap-0.5"
      role="group"
      aria-label={`Vote. Current score: ${score}`}
    >
      <button
        type="button"
        className={`${size} rounded flex items-center justify-center transition-colors ${
          upActive ? 'text-orange-400' : ''
        }`}
        style={{ color: upActive ? '#fb923c' : 'var(--text-secondary)' }}
        disabled={busy}
        onClick={() => onVote('up')}
        aria-label="Upvote"
        aria-pressed={upActive}
      >
        ▲
      </button>
      <span
        className={`font-mono font-semibold leading-none ${compact ? 'text-[10px]' : 'text-xs'}`}
        style={{ color: upActive ? '#fb923c' : downActive ? '#818cf8' : 'var(--text-primary)' }}
      >
        {formatScore(score)}
      </span>
      <button
        type="button"
        className={`${size} rounded flex items-center justify-center transition-colors`}
        style={{ color: downActive ? '#818cf8' : 'var(--text-secondary)' }}
        disabled={busy}
        onClick={() => onVote('down')}
        aria-label="Downvote"
        aria-pressed={downActive}
      >
        ▼
      </button>
    </div>
  )
}

// ---------------------------------------------------------------------------
// CommentItem
// ---------------------------------------------------------------------------

interface CommentItemProps {
  comment: RedditComment
  onVote: (id: string, dir: 'up' | 'down') => void
  busyId: string | null
}

function CommentItem({ comment, onVote, busyId }: CommentItemProps) {
  const [collapsed, setCollapsed] = useState(false)
  const indentPx = comment.depth * 12

  return (
    <div style={{ marginLeft: `${indentPx}px` }}>
      <div
        className="py-1.5"
        style={{ borderLeft: comment.depth > 0 ? '2px solid var(--pill-border)' : 'none', paddingLeft: comment.depth > 0 ? '8px' : '0' }}
      >
        <div className="flex items-center gap-1.5 mb-0.5">
          <button
            type="button"
            className="text-[10px] font-mono"
            style={{ color: 'var(--text-secondary)' }}
            onClick={() => setCollapsed(c => !c)}
            aria-expanded={!collapsed}
            aria-label={collapsed ? 'Expand comment' : 'Collapse comment'}
          >
            {collapsed ? '[+]' : '[–]'}
          </button>
          <span className="text-[10px] font-medium" style={{ color: 'var(--text-primary)' }}>
            {comment.author}
          </span>
          <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
            {formatScore(comment.score)} pts · {formatAge(comment.created_utc)}
          </span>
        </div>
        {!collapsed && (
          <>
            <p className="text-xs leading-relaxed whitespace-pre-wrap" style={{ color: 'var(--text-primary)' }}>
              {comment.body}
            </p>
            <div className="mt-1">
              <VoteButtons
                score={comment.score}
                liked={comment.liked}
                onVote={dir => onVote(comment.name, dir)}
                busy={busyId === comment.name}
                compact
              />
            </div>
          </>
        )}
      </div>
      {!collapsed && comment.replies?.map(r => (
        <CommentItem key={r.id} comment={r} onVote={onVote} busyId={busyId} />
      ))}
    </div>
  )
}

// ---------------------------------------------------------------------------
// PostDetail
// ---------------------------------------------------------------------------

interface PostDetailProps {
  post: RedditPost
  comments: RedditComment[]
  loadingComments: boolean
  onVoteComment: (id: string, dir: 'up' | 'down') => void
  onComment: (text: string) => void
  busyId: string | null
  submittingComment: boolean
}

function PostDetail({
  post,
  comments,
  loadingComments,
  onVoteComment,
  onComment,
  busyId,
  submittingComment,
}: PostDetailProps) {
  const [replyText, setReplyText] = useState('')

  function handleSubmit() {
    const t = replyText.trim()
    if (!t) return
    onComment(t)
    setReplyText('')
  }

  return (
    <div className="flex flex-col gap-3 p-3">
      {post.is_self && post.selftext && (
        <p className="text-xs leading-relaxed whitespace-pre-wrap" style={{ color: 'var(--text-secondary)' }}>
          {post.selftext}
        </p>
      )}
      {!post.is_self && (
        <a
          href={post.url}
          target="_blank"
          rel="noopener noreferrer"
          className="text-xs underline truncate"
          style={{ color: 'var(--color-link, #60a5fa)' }}
        >
          {post.url}
        </a>
      )}

      {/* Inline comment composer */}
      <div className="flex gap-2 pt-1">
        <textarea
          className="flex-1 rounded text-xs p-2 resize-none min-h-[56px] outline-none focus:ring-1"
          style={{
            background: 'var(--mic-bg)',
            border: '1px solid var(--pill-border)',
            color: 'var(--text-primary)',
          }}
          placeholder="Add a comment…"
          value={replyText}
          onChange={e => setReplyText(e.target.value)}
          onKeyDown={e => {
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) handleSubmit()
          }}
          aria-label="Comment text"
        />
        <Button
          size="sm"
          variant="outline"
          className="self-end h-7 px-2 text-[10px]"
          disabled={!replyText.trim() || submittingComment}
          onClick={handleSubmit}
        >
          {submittingComment ? '…' : 'Reply'}
        </Button>
      </div>

      {/* Comments */}
      <section aria-label="Comments">
        <h3
          className="text-[10px] uppercase tracking-wider font-medium mb-2"
          style={{ color: 'var(--text-secondary)' }}
        >
          {post.num_comments} comments
        </h3>
        {loadingComments ? (
          <div className="flex flex-col gap-2 animate-pulse">
            {[1, 2, 3].map(i => (
              <div key={i} className="h-10 rounded" style={{ background: 'var(--mic-bg)' }} />
            ))}
          </div>
        ) : comments.length === 0 ? (
          <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>No comments yet.</p>
        ) : (
          <div className="flex flex-col gap-1">
            {comments.map(c => (
              <CommentItem key={c.id} comment={c} onVote={onVoteComment} busyId={busyId} />
            ))}
          </div>
        )}
      </section>
    </div>
  )
}

// ---------------------------------------------------------------------------
// PostCard
// ---------------------------------------------------------------------------

interface PostCardProps {
  post: RedditPost
  expanded: boolean
  comments: RedditComment[]
  loadingComments: boolean
  onToggle: () => void
  onVote: (dir: 'up' | 'down') => void
  onVoteComment: (id: string, dir: 'up' | 'down') => void
  onComment: (text: string) => void
  busyId: string | null
  submittingComment: boolean
}

function PostCard({
  post,
  expanded,
  comments,
  loadingComments,
  onToggle,
  onVote,
  onVoteComment,
  onComment,
  busyId,
  submittingComment,
}: PostCardProps) {
  return (
    <article>
      <Card
        className="overflow-hidden"
        style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
      >
        <div className="flex gap-2 p-3">
          {/* Vote column */}
          <VoteButtons
            score={post.score}
            liked={post.liked}
            onVote={onVote}
            busy={busyId === post.name}
          />

          {/* Content */}
          <div className="flex-1 min-w-0">
            <button
              type="button"
              className="text-left w-full"
              onClick={onToggle}
              aria-expanded={expanded}
            >
              <p
                className="text-xs font-medium leading-snug"
                style={{ color: 'var(--text-primary)' }}
              >
                {post.stickied && (
                  <span className="text-[9px] font-bold uppercase mr-1" style={{ color: '#4ade80' }}>
                    📌
                  </span>
                )}
                {post.title}
              </p>
            </button>
            <div className="flex items-center gap-2 mt-1 flex-wrap">
              <Badge variant="outline" className="text-[9px] px-1 h-4">
                r/{post.subreddit}
              </Badge>
              <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                u/{post.author} · {formatAge(post.created_utc)}
              </span>
              <button
                type="button"
                className="text-[10px] flex items-center gap-0.5"
                style={{ color: 'var(--text-secondary)' }}
                onClick={onToggle}
                aria-label={`${post.num_comments} comments, click to ${expanded ? 'collapse' : 'expand'}`}
              >
                💬 {post.num_comments}
              </button>
              {!post.is_self && (
                <a
                  href={post.url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-[10px] underline truncate max-w-[120px]"
                  style={{ color: 'var(--text-secondary)' }}
                  onClick={e => e.stopPropagation()}
                  aria-label="External link"
                >
                  🔗 link
                </a>
              )}
            </div>
          </div>
        </div>

        {expanded && (
          <div style={{ borderTop: '1px solid var(--pill-border)' }}>
            <PostDetail
              post={post}
              comments={comments}
              loadingComments={loadingComments}
              onVoteComment={onVoteComment}
              onComment={onComment}
              busyId={busyId}
              submittingComment={submittingComment}
            />
          </div>
        )}
      </Card>
    </article>
  )
}

// ---------------------------------------------------------------------------
// ComposeDialog
// ---------------------------------------------------------------------------

interface ComposeDialogProps {
  open: boolean
  subreddits: string[]
  onClose: () => void
  onSubmit: (subreddit: string, title: string, body: string) => Promise<void>
  busy: boolean
}

function ComposeDialog({ open, subreddits, onClose, onSubmit, busy }: ComposeDialogProps) {
  const [subreddit, setSubreddit] = useState(subreddits[0] ?? '')
  const [title, setTitle] = useState('')
  const [body, setBody] = useState('')

  async function handleSubmit() {
    if (!subreddit || !title.trim()) return
    await onSubmit(subreddit, title.trim(), body.trim())
    setTitle('')
    setBody('')
  }

  return (
    <Dialog open={open} onOpenChange={(v: boolean) => { if (!v) onClose() }}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>New Post</DialogTitle>
        </DialogHeader>
        <div className="flex flex-col gap-3 py-2">
          <div className="flex flex-col gap-1">
            <label className="text-xs font-medium" style={{ color: 'var(--text-secondary)' }}>
              Subreddit
            </label>
            <Select value={subreddit} onValueChange={setSubreddit}>
              <SelectTrigger className="h-8 text-xs">
                <SelectValue placeholder="r/…" />
              </SelectTrigger>
              <SelectContent>
                {subreddits.map(s => (
                  <SelectItem key={s} value={s} className="text-xs">
                    r/{s}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="flex flex-col gap-1">
            <label className="text-xs font-medium" style={{ color: 'var(--text-secondary)' }}>
              Title
            </label>
            <input
              type="text"
              className="rounded text-xs p-2 outline-none focus:ring-1"
              style={{
                background: 'var(--mic-bg)',
                border: '1px solid var(--pill-border)',
                color: 'var(--text-primary)',
              }}
              placeholder="Post title…"
              value={title}
              onChange={e => setTitle(e.target.value)}
              maxLength={300}
              aria-label="Post title"
            />
          </div>
          <div className="flex flex-col gap-1">
            <label className="text-xs font-medium" style={{ color: 'var(--text-secondary)' }}>
              Body
            </label>
            <textarea
              className="rounded text-xs p-2 resize-none min-h-[80px] outline-none focus:ring-1"
              style={{
                background: 'var(--mic-bg)',
                border: '1px solid var(--pill-border)',
                color: 'var(--text-primary)',
              }}
              placeholder="Post body (optional for link posts)…"
              value={body}
              onChange={e => setBody(e.target.value)}
              aria-label="Post body"
            />
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" size="sm" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button
            size="sm"
            disabled={!subreddit || !title.trim() || busy}
            onClick={handleSubmit}
          >
            {busy ? 'Posting…' : 'Submit'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ---------------------------------------------------------------------------
// RedditView
// ---------------------------------------------------------------------------

export default function RedditView(_props: PluginViewProps) {
  const [status, setStatus] = useState<RedditStatus | null>(null)
  const [posts, setPosts] = useState<RedditPost[] | null>(null)
  const [commentsMap, setCommentsMap] = useState<Record<string, RedditComment[]>>({})
  const [loadingCommentsFor, setLoadingCommentsFor] = useState<string | null>(null)
  const [expandedId, setExpandedId] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [busyId, setBusyId] = useState<string | null>(null)
  const [submittingComment, setSubmittingComment] = useState(false)
  const [composerOpen, setComposerOpen] = useState(false)
  const [composeBusy, setComposeBusy] = useState(false)
  const [feedback, setFeedback] = useState<string | null>(null)
  const [query, setQuery] = useState('')
  const [sort, setSort] = useState<SortKey>('hot')
  const [activeSubreddit, setActiveSubreddit] = useState<string>('all')

  const loadData = useCallback(async () => {
    try {
      const [statusRes, itemsRes] = await Promise.allSettled([
        queryPluginStatus(PLUGIN_NAME),
        queryPluginItems(PLUGIN_NAME),
      ])
      if (statusRes.status === 'fulfilled') {
        setStatus(statusRes.value as unknown as RedditStatus)
      }
      if (itemsRes.status === 'fulfilled') {
        setPosts(itemsRes.value as unknown as RedditPost[])
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
    const id = setInterval(loadData, 60_000)
    return () => clearInterval(id)
  }, [loadData])

  const subreddits = useMemo(() => {
    const fromStatus = status?.subreddits ?? []
    const fromPosts = [...new Set((posts ?? []).map(p => p.subreddit))]
    const merged = [...new Set([...fromStatus, ...fromPosts])]
    return merged.length > 0 ? merged : FALLBACK_SUBREDDITS
  }, [status, posts])

  const filteredPosts = useMemo(() => {
    let list = posts ?? []
    if (activeSubreddit && activeSubreddit !== 'all') {
      list = list.filter(p => p.subreddit.toLowerCase() === activeSubreddit.toLowerCase())
    }
    if (query) {
      const q = query.toLowerCase()
      list = list.filter(
        p =>
          p.title.toLowerCase().includes(q) ||
          p.author.toLowerCase().includes(q) ||
          p.subreddit.toLowerCase().includes(q),
      )
    }
    return sortPosts(list, sort)
  }, [posts, activeSubreddit, query, sort])

  function flash(msg: string) {
    setFeedback(msg)
    setTimeout(() => setFeedback(null), 4_000)
  }

  async function handleVotePost(post: RedditPost, dir: 'up' | 'down') {
    setBusyId(post.name)
    try {
      await pluginAction(PLUGIN_NAME, dir === 'up' ? 'upvote' : 'downvote', post.name)
      setPosts(prev =>
        (prev ?? []).map(p =>
          p.name === post.name
            ? { ...p, liked: p.liked === (dir === 'up' ? true : false) ? null : dir === 'up', score: p.score + (dir === 'up' ? 1 : -1) }
            : p,
        ),
      )
    } catch (err) {
      flash(`Vote failed: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      setBusyId(null)
    }
  }

  async function handleVoteComment(commentName: string, dir: 'up' | 'down') {
    setBusyId(commentName)
    try {
      await pluginAction(PLUGIN_NAME, dir === 'up' ? 'upvote' : 'downvote', commentName)
    } catch {
      // silent for comments
    } finally {
      setBusyId(null)
    }
  }

  async function handleTogglePost(post: RedditPost) {
    if (expandedId === post.id) {
      setExpandedId(null)
      return
    }
    setExpandedId(post.id)
    if (!commentsMap[post.id]) {
      setLoadingCommentsFor(post.id)
      try {
        const data = await pluginAction(PLUGIN_NAME, 'comments', post.name)
        const items = Array.isArray(data) ? data : (data.items as RedditComment[] | undefined) ?? []
        setCommentsMap(prev => ({ ...prev, [post.id]: items }))
      } catch {
        setCommentsMap(prev => ({ ...prev, [post.id]: [] }))
      } finally {
        setLoadingCommentsFor(null)
      }
    }
  }

  async function handleComment(post: RedditPost, text: string) {
    setSubmittingComment(true)
    try {
      await pluginAction(PLUGIN_NAME, 'comment', `${post.name}||${text}`)
      flash('Comment posted.')
      // Prepend optimistic comment
      const optimistic: RedditComment = {
        id: `opt_${Date.now()}`,
        name: `t1_opt_${Date.now()}`,
        author: status?.username ?? 'you',
        body: text,
        score: 1,
        created_utc: Math.floor(Date.now() / 1000),
        depth: 0,
        liked: true,
      }
      setCommentsMap(prev => ({
        ...prev,
        [post.id]: [optimistic, ...(prev[post.id] ?? [])],
      }))
    } catch (err) {
      flash(`Comment failed: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      setSubmittingComment(false)
    }
  }

  async function handleCompose(subreddit: string, title: string, body: string) {
    setComposeBusy(true)
    try {
      await pluginAction(PLUGIN_NAME, 'submit', `${subreddit}||${title}||${body}`)
      flash('Post submitted.')
      setComposerOpen(false)
      await loadData()
    } catch (err) {
      flash(`Submit failed: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      setComposeBusy(false)
    }
  }

  // ── Render ──────────────────────────────────────────────────────────────────

  if (loading) {
    return (
      <div className="flex flex-col gap-3 p-4 animate-pulse">
        <div className="h-8 rounded" style={{ background: 'var(--mic-bg)' }} />
        <div className="h-6 rounded w-2/3" style={{ background: 'var(--mic-bg)' }} />
        {[1, 2, 3].map(i => (
          <div key={i} className="h-16 rounded" style={{ background: 'var(--mic-bg)' }} />
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

  return (
    <div className="flex flex-col gap-3 p-4">
      {/* Feedback */}
      {feedback && (
        <p
          className="text-xs px-2 py-1 rounded"
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

      {/* Header: status + compose */}
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-1.5">
          <span className="text-xs font-semibold" style={{ color: 'var(--text-primary)' }}>
            Reddit
          </span>
          {status?.username && (
            <Badge variant="outline" className="text-[9px] h-4 px-1">
              u/{status.username}
            </Badge>
          )}
        </div>
        <Button
          size="sm"
          variant="outline"
          className="h-6 px-2 text-[10px]"
          onClick={() => setComposerOpen(true)}
        >
          + New Post
        </Button>
      </div>

      {/* Search + Sort */}
      <div className="flex gap-2">
        <input
          type="search"
          className="flex-1 rounded text-xs p-2 h-7 outline-none focus:ring-1"
          style={{
            background: 'var(--mic-bg)',
            border: '1px solid var(--pill-border)',
            color: 'var(--text-primary)',
          }}
          placeholder="Search posts…"
          value={query}
          onChange={e => setQuery(e.target.value)}
          aria-label="Search posts"
        />
        <Select value={sort} onValueChange={(v: string) => setSort(v as SortKey)}>
          <SelectTrigger className="h-7 w-24 text-[10px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {SORT_OPTIONS.map(o => (
              <SelectItem key={o.value} value={o.value} className="text-xs">
                {o.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {/* Subreddit tabs */}
      <nav
        className="flex gap-1 overflow-x-auto pb-0.5"
        role="tablist"
        aria-label="Subreddits"
        style={{ scrollbarWidth: 'none' }}
      >
        {['all', ...subreddits].map(s => (
          <button
            key={s}
            type="button"
            role="tab"
            aria-selected={activeSubreddit === s}
            className="flex-shrink-0 text-[10px] px-2 py-0.5 rounded-full transition-colors"
            style={{
              background: activeSubreddit === s ? 'var(--text-primary)' : 'var(--mic-bg)',
              color: activeSubreddit === s ? 'var(--bg-primary, #0f1117)' : 'var(--text-secondary)',
              border: '1px solid var(--pill-border)',
            }}
            onClick={() => setActiveSubreddit(s)}
          >
            {s === 'all' ? 'All' : `r/${s}`}
          </button>
        ))}
      </nav>

      {/* Post list */}
      {filteredPosts.length === 0 ? (
        <p className="text-xs text-center py-4" style={{ color: 'var(--text-secondary)' }}>
          {query ? 'No posts match your search.' : 'No posts.'}
        </p>
      ) : (
        <div className="flex flex-col gap-2">
          {filteredPosts.map(post => (
            <PostCard
              key={post.id}
              post={post}
              expanded={expandedId === post.id}
              comments={commentsMap[post.id] ?? []}
              loadingComments={loadingCommentsFor === post.id}
              onToggle={() => handleTogglePost(post)}
              onVote={dir => handleVotePost(post, dir)}
              onVoteComment={handleVoteComment}
              onComment={text => handleComment(post, text)}
              busyId={busyId}
              submittingComment={submittingComment}
            />
          ))}
        </div>
      )}

      {/* Footer */}
      <div className="flex justify-end">
        <Button size="sm" variant="outline" onClick={loadData} aria-label="Refresh feed">
          Refresh
        </Button>
      </div>

      {/* Compose Dialog */}
      <ComposeDialog
        open={composerOpen}
        subreddits={subreddits}
        onClose={() => setComposerOpen(false)}
        onSubmit={handleCompose}
        busy={composeBusy}
      />
    </div>
  )
}
