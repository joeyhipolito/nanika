import { useState, useEffect, useCallback } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { Tabs, TabsList, TabsTrigger, TabsContent } from '@/components/ui/tabs'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogFooter,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog'
import { queryPluginStatus, queryPluginItems, pluginAction } from '@/lib/wails'
import type { PluginViewProps } from '@/types'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type AudienceType = 'everyone' | 'founding_members' | 'paid' | 'free'
type DraftStatus = 'draft' | 'scheduled' | 'published'

interface SubstackDraft {
  id: string
  title: string
  subtitle?: string
  audience: AudienceType
  tags: string[]
  status: DraftStatus
  word_count?: number
  created_at: string
}

interface SubstackNote {
  id: string
  body: string
  date: string
  reaction_count?: number
  children_count?: number
}

interface SubstackPost {
  id: string
  title: string
  slug?: string
  audience?: AudienceType
  word_count?: number
  reaction_count?: number
  comment_count?: number
  published_at?: string
}

interface SubstackStatus {
  configured: boolean
  authenticated: boolean
  publication_url?: string
  subdomain?: string
}

interface PluginData {
  drafts?: SubstackDraft[]
  notes?: SubstackNote[]
  posts?: SubstackPost[]
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const PLUGIN_NAME = 'substack'

const AUDIENCE_LABELS: Record<AudienceType, string> = {
  everyone: 'Everyone',
  founding_members: 'Founding',
  paid: 'Paid',
  free: 'Free',
}

const AUDIENCE_BADGE_VARIANT: Record<AudienceType, 'default' | 'secondary' | 'outline'> = {
  everyone: 'default',
  founding_members: 'secondary',
  paid: 'secondary',
  free: 'outline',
}

const STATUS_BADGE_VARIANT: Record<DraftStatus, 'default' | 'secondary' | 'destructive' | 'outline'> = {
  draft: 'outline',
  scheduled: 'secondary',
  published: 'default',
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatDate(dateStr: string): string {
  if (!dateStr) return ''
  try {
    const d = new Date(dateStr)
    const diffDays = Math.floor((Date.now() - d.getTime()) / 86_400_000)
    if (diffDays === 0) return 'today'
    if (diffDays === 1) return 'yesterday'
    if (diffDays < 7) return `${diffDays}d ago`
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' })
  } catch {
    return dateStr
  }
}

// ---------------------------------------------------------------------------
// Sub-components: Drafts
// ---------------------------------------------------------------------------

interface DraftCardProps {
  draft: SubstackDraft
  onEdit: (draft: SubstackDraft) => void
}

function DraftCard({ draft, onEdit }: DraftCardProps) {
  const audience = (draft.audience ?? 'everyone') as AudienceType

  return (
    <article>
      <Card
        className="p-3 flex flex-col gap-2"
        style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
      >
        <div className="flex items-start justify-between gap-2">
          <div className="flex-1 min-w-0">
            <p
              className="text-xs font-medium truncate leading-snug"
              style={{ color: 'var(--text-primary)' }}
              title={draft.title}
            >
              {draft.title || '(untitled)'}
            </p>
            {draft.subtitle && (
              <p className="text-[10px] truncate mt-0.5" style={{ color: 'var(--text-secondary)' }}>
                {draft.subtitle}
              </p>
            )}
          </div>
          <div className="flex items-center gap-1 flex-shrink-0">
            <Badge variant={AUDIENCE_BADGE_VARIANT[audience]} className="text-[10px]">
              {AUDIENCE_LABELS[audience]}
            </Badge>
            <Badge variant={STATUS_BADGE_VARIANT[draft.status]} className="text-[10px] capitalize">
              {draft.status}
            </Badge>
          </div>
        </div>

        {(draft.tags ?? []).length > 0 && (
          <div className="flex flex-wrap gap-1">
            {draft.tags.map(tag => (
              <span
                key={tag}
                className="rounded px-1.5 py-0.5 text-[9px]"
                style={{
                  background: 'color-mix(in srgb, var(--accent) 10%, transparent)',
                  color: 'var(--text-secondary)',
                }}
              >
                {tag}
              </span>
            ))}
          </div>
        )}

        <div className="flex items-center justify-between">
          <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
            {draft.word_count ? `${draft.word_count} words · ` : ''}
            {formatDate(draft.created_at)}
          </span>
          <Button
            size="sm"
            variant="outline"
            className="h-6 px-2 text-[10px]"
            onClick={() => onEdit(draft)}
          >
            Edit
          </Button>
        </div>
      </Card>
    </article>
  )
}

// ---------------------------------------------------------------------------
// Sub-components: Editor
// ---------------------------------------------------------------------------

interface EditorFormState {
  id?: string
  title: string
  body: string
  audience: AudienceType
  tagsRaw: string
}

const EMPTY_EDITOR: EditorFormState = {
  title: '',
  body: '',
  audience: 'everyone',
  tagsRaw: '',
}

interface PublishDialogProps {
  open: boolean
  form: EditorFormState
  busy: boolean
  onConfirm: () => void
  onCancel: () => void
}

function PublishDialog({ open, form, busy, onConfirm, onCancel }: PublishDialogProps) {
  const tags = form.tagsRaw
    .split(',')
    .map(t => t.trim())
    .filter(Boolean)

  return (
    <Dialog open={open} onOpenChange={(v: boolean) => { if (!v) onCancel() }}>
      <DialogContent style={{ background: 'var(--bg)', borderColor: 'var(--pill-border)' }}>
        <DialogHeader>
          <DialogTitle style={{ color: 'var(--text-primary)' }}>Publish post</DialogTitle>
          <DialogDescription style={{ color: 'var(--text-secondary)' }}>
            Review settings before publishing to your Substack.
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-3 py-2">
          <div>
            <p className="text-xs font-medium mb-1" style={{ color: 'var(--text-secondary)' }}>Title</p>
            <p className="text-sm" style={{ color: 'var(--text-primary)' }}>
              {form.title || '(untitled)'}
            </p>
          </div>
          <div>
            <p className="text-xs font-medium mb-1" style={{ color: 'var(--text-secondary)' }}>Audience</p>
            <Badge variant={AUDIENCE_BADGE_VARIANT[form.audience]}>
              {AUDIENCE_LABELS[form.audience]}
            </Badge>
          </div>
          {tags.length > 0 && (
            <div>
              <p className="text-xs font-medium mb-1" style={{ color: 'var(--text-secondary)' }}>Tags</p>
              <div className="flex flex-wrap gap-1">
                {tags.map(tag => (
                  <span
                    key={tag}
                    className="rounded px-1.5 py-0.5 text-xs"
                    style={{
                      background: 'color-mix(in srgb, var(--accent) 10%, transparent)',
                      color: 'var(--text-secondary)',
                    }}
                  >
                    {tag}
                  </span>
                ))}
              </div>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onCancel} disabled={busy}>
            Cancel
          </Button>
          <Button onClick={onConfirm} disabled={busy}>
            {busy ? 'Publishing…' : 'Publish'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

interface DraftEditorProps {
  initial?: SubstackDraft
  onPublished: () => void
}

function DraftEditor({ initial, onPublished }: DraftEditorProps) {
  const [form, setForm] = useState<EditorFormState>(
    initial
      ? {
          id: initial.id,
          title: initial.title,
          body: '',
          audience: initial.audience,
          tagsRaw: (initial.tags ?? []).join(', '),
        }
      : EMPTY_EDITOR,
  )
  const [showPublish, setShowPublish] = useState(false)
  const [busy, setBusy] = useState(false)
  const [feedback, setFeedback] = useState<string | null>(null)

  const handleSaveDraft = async () => {
    setBusy(true)
    setFeedback(null)
    try {
      const tags = form.tagsRaw.split(',').map(t => t.trim()).filter(Boolean).join(',')
      const payload = JSON.stringify({ title: form.title, body: form.body, audience: form.audience, tags })
      await pluginAction(PLUGIN_NAME, 'save-draft', payload)
      setFeedback('Draft saved')
    } catch (err) {
      setFeedback(`Save failed: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      setBusy(false)
      setTimeout(() => setFeedback(null), 3_000)
    }
  }

  const handlePublish = async () => {
    setBusy(true)
    setFeedback(null)
    try {
      const tags = form.tagsRaw.split(',').map(t => t.trim()).filter(Boolean).join(',')
      const payload = JSON.stringify({ title: form.title, body: form.body, audience: form.audience, tags })
      await pluginAction(PLUGIN_NAME, 'publish', payload)
      setFeedback('Published successfully')
      setShowPublish(false)
      setForm(EMPTY_EDITOR)
      onPublished()
    } catch (err) {
      setFeedback(`Publish failed: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      setBusy(false)
      setTimeout(() => setFeedback(null), 4_000)
    }
  }

  const inputStyle = {
    background: 'var(--mic-bg)',
    border: '1px solid var(--pill-border)',
    color: 'var(--text-primary)',
  }

  return (
    <div className="flex flex-col gap-3">
      {feedback && (
        <p
          className="text-xs px-2 py-1 rounded"
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

      <div>
        <label
          htmlFor="editor-title"
          className="text-[10px] uppercase tracking-wider font-medium block mb-1"
          style={{ color: 'var(--text-secondary)' }}
        >
          Title
        </label>
        <input
          id="editor-title"
          type="text"
          value={form.title}
          onChange={e => setForm(f => ({ ...f, title: e.target.value }))}
          placeholder="Post title"
          className="w-full rounded-md px-3 py-2 text-sm outline-none"
          style={inputStyle}
        />
      </div>

      <div>
        <label
          htmlFor="editor-body"
          className="text-[10px] uppercase tracking-wider font-medium block mb-1"
          style={{ color: 'var(--text-secondary)' }}
        >
          Body
        </label>
        <textarea
          id="editor-body"
          value={form.body}
          onChange={e => setForm(f => ({ ...f, body: e.target.value }))}
          placeholder="Write your post…"
          rows={8}
          className="w-full rounded-md px-3 py-2 text-sm outline-none resize-none"
          style={inputStyle}
        />
      </div>

      <div className="grid grid-cols-2 gap-3">
        <div>
          <label
            htmlFor="editor-audience"
            className="text-[10px] uppercase tracking-wider font-medium block mb-1"
            style={{ color: 'var(--text-secondary)' }}
          >
            Audience
          </label>
          <select
            id="editor-audience"
            value={form.audience}
            onChange={e => setForm(f => ({ ...f, audience: e.target.value as AudienceType }))}
            className="w-full rounded-md px-3 py-2 text-sm outline-none"
            style={inputStyle}
          >
            <option value="everyone">Everyone</option>
            <option value="paid">Paid subscribers</option>
            <option value="free">Free subscribers</option>
            <option value="founding_members">Founding members</option>
          </select>
        </div>

        <div>
          <label
            htmlFor="editor-tags"
            className="text-[10px] uppercase tracking-wider font-medium block mb-1"
            style={{ color: 'var(--text-secondary)' }}
          >
            Tags
          </label>
          <input
            id="editor-tags"
            type="text"
            value={form.tagsRaw}
            onChange={e => setForm(f => ({ ...f, tagsRaw: e.target.value }))}
            placeholder="tag1, tag2"
            className="w-full rounded-md px-3 py-2 text-sm outline-none"
            style={inputStyle}
          />
        </div>
      </div>

      <div className="flex gap-2 justify-end">
        <Button
          size="sm"
          variant="outline"
          onClick={() => void handleSaveDraft()}
          disabled={busy || !form.title.trim()}
        >
          {busy ? '…' : 'Save draft'}
        </Button>
        <Button
          size="sm"
          onClick={() => setShowPublish(true)}
          disabled={busy || !form.title.trim() || !form.body.trim()}
        >
          Publish…
        </Button>
      </div>

      <PublishDialog
        open={showPublish}
        form={form}
        busy={busy}
        onConfirm={() => void handlePublish()}
        onCancel={() => setShowPublish(false)}
      />
    </div>
  )
}

// ---------------------------------------------------------------------------
// Sub-components: Notes
// ---------------------------------------------------------------------------

function NoteCard({ note }: { note: SubstackNote }) {
  return (
    <article>
      <Card
        className="p-3 flex flex-col gap-1.5"
        style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
      >
        <p className="text-xs leading-relaxed" style={{ color: 'var(--text-primary)' }}>
          {note.body}
        </p>
        <div className="flex items-center gap-3 text-[10px]" style={{ color: 'var(--text-secondary)' }}>
          <span>{formatDate(note.date)}</span>
          {(note.reaction_count ?? 0) > 0 && <span>♡ {note.reaction_count}</span>}
          {(note.children_count ?? 0) > 0 && <span>↩ {note.children_count}</span>}
        </div>
      </Card>
    </article>
  )
}

function NoteComposer({ onPosted }: { onPosted: () => void }) {
  const [body, setBody] = useState('')
  const [busy, setBusy] = useState(false)
  const [feedback, setFeedback] = useState<string | null>(null)

  const handlePost = async () => {
    if (!body.trim()) return
    setBusy(true)
    setFeedback(null)
    try {
      await pluginAction(PLUGIN_NAME, 'post-note', body.trim())
      setBody('')
      setFeedback('Note posted')
      onPosted()
    } catch (err) {
      setFeedback(`Post failed: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      setBusy(false)
      setTimeout(() => setFeedback(null), 3_000)
    }
  }

  return (
    <Card
      className="p-3 flex flex-col gap-2"
      style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
    >
      <p
        className="text-[10px] uppercase tracking-wider font-medium"
        style={{ color: 'var(--text-secondary)' }}
      >
        New note
      </p>
      <textarea
        value={body}
        onChange={e => setBody(e.target.value)}
        placeholder="Share a note with your subscribers…"
        rows={3}
        className="w-full rounded-md px-3 py-2 text-sm outline-none resize-none"
        style={{
          background: 'var(--bg)',
          border: '1px solid var(--pill-border)',
          color: 'var(--text-primary)',
        }}
        aria-label="Note body"
      />
      {feedback && (
        <p
          className="text-[10px]"
          style={{
            color: feedback.includes('failed') ? 'var(--color-error)' : 'var(--color-success)',
          }}
        >
          {feedback}
        </p>
      )}
      <div className="flex justify-end">
        <Button size="sm" onClick={() => void handlePost()} disabled={busy || !body.trim()}>
          {busy ? 'Posting…' : 'Post note'}
        </Button>
      </div>
    </Card>
  )
}

// ---------------------------------------------------------------------------
// Sub-components: Analytics
// ---------------------------------------------------------------------------

function AnalyticsTable({ posts }: { posts: SubstackPost[] }) {
  if (posts.length === 0) {
    return (
      <p className="text-sm py-8 text-center" style={{ color: 'var(--text-secondary)' }}>
        No published posts yet.
      </p>
    )
  }

  return (
    <div className="overflow-x-auto">
      <table className="w-full text-xs">
        <thead>
          <tr style={{ borderBottom: '1px solid var(--pill-border)' }}>
            {(['Title', 'Audience', '♡', '↩', 'Words', 'Published'] as const).map(h => (
              <th
                key={h}
                className="text-left px-2 py-1.5 font-medium uppercase tracking-wider text-[10px] whitespace-nowrap"
                style={{ color: 'var(--text-secondary)' }}
              >
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {posts.map(post => {
            const aud = (post.audience ?? 'everyone') as AudienceType
            return (
              <tr
                key={post.id}
                className="transition-opacity hover:opacity-80"
                style={{ borderBottom: '1px solid var(--pill-border)' }}
              >
                <td className="px-2 py-2 max-w-[160px]">
                  <p
                    className="truncate font-medium"
                    style={{ color: 'var(--text-primary)' }}
                    title={post.title}
                  >
                    {post.title}
                  </p>
                </td>
                <td className="px-2 py-2">
                  <Badge variant={AUDIENCE_BADGE_VARIANT[aud]} className="text-[10px]">
                    {AUDIENCE_LABELS[aud]}
                  </Badge>
                </td>
                <td className="px-2 py-2 font-mono" style={{ color: 'var(--text-secondary)' }}>
                  {post.reaction_count ?? 0}
                </td>
                <td className="px-2 py-2 font-mono" style={{ color: 'var(--text-secondary)' }}>
                  {post.comment_count ?? 0}
                </td>
                <td className="px-2 py-2 font-mono" style={{ color: 'var(--text-secondary)' }}>
                  {post.word_count ?? 0}
                </td>
                <td className="px-2 py-2 whitespace-nowrap" style={{ color: 'var(--text-secondary)' }}>
                  {post.published_at ? formatDate(post.published_at) : '—'}
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Main: SubstackView
// ---------------------------------------------------------------------------

export default function SubstackView(_props: PluginViewProps) {
  const [status, setStatus] = useState<SubstackStatus | null>(null)
  const [data, setData] = useState<PluginData>({})
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [activeTab, setActiveTab] = useState<string>('drafts')
  const [editingDraft, setEditingDraft] = useState<SubstackDraft | undefined>(undefined)

  const loadData = useCallback(async () => {
    try {
      const [statusRes, itemsRes] = await Promise.allSettled([
        queryPluginStatus(PLUGIN_NAME),
        queryPluginItems(PLUGIN_NAME),
      ])
      if (statusRes.status === 'fulfilled') {
        setStatus(statusRes.value as unknown as SubstackStatus)
      }
      if (itemsRes.status === 'fulfilled') {
        const raw = itemsRes.value as unknown
        if (raw && typeof raw === 'object' && !Array.isArray(raw)) {
          setData(raw as PluginData)
        } else if (Array.isArray(raw)) {
          setData({ drafts: raw as SubstackDraft[] })
        }
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
    const id = setInterval(() => void loadData(), 60_000)
    return () => clearInterval(id)
  }, [loadData])

  const handleEditDraft = useCallback((draft: SubstackDraft) => {
    setEditingDraft(draft)
    setActiveTab('editor')
  }, [])

  const handlePublished = useCallback(() => {
    setEditingDraft(undefined)
    void loadData()
  }, [loadData])

  const handleNotePosted = useCallback(() => {
    void loadData()
  }, [loadData])

  if (loading) {
    return (
      <div className="flex flex-col gap-3 p-4 animate-pulse">
        <div className="h-8 rounded" style={{ background: 'var(--mic-bg)' }} />
        <div className="h-10 rounded" style={{ background: 'var(--mic-bg)' }} />
        <div className="h-32 rounded" style={{ background: 'var(--mic-bg)' }} />
      </div>
    )
  }

  if (error) {
    return (
      <div className="p-4">
        <Badge variant="destructive" className="mb-2">error</Badge>
        <p className="text-sm font-mono" style={{ color: 'var(--color-error)' }}>{error}</p>
        <Button variant="outline" size="sm" className="mt-3" onClick={() => void loadData()}>
          Retry
        </Button>
      </div>
    )
  }

  const drafts = data.drafts ?? []
  const notes = data.notes ?? []
  const posts = data.posts ?? []
  const draftCount = drafts.filter(d => d.status === 'draft').length

  return (
    <div className="flex flex-col gap-4 p-4">
      {/* Publication header */}
      <div className="flex items-center justify-between">
        <div>
          <p className="text-xs font-semibold" style={{ color: 'var(--text-primary)' }}>
            {status?.publication_url ?? status?.subdomain ?? 'Substack'}
          </p>
          <p className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
            {draftCount} draft{draftCount !== 1 ? 's' : ''} · {posts.length} published
          </p>
        </div>
        <div className="flex items-center gap-1.5">
          {status && (
            <Badge
              variant={status.authenticated ? 'default' : 'destructive'}
              className="text-[10px]"
            >
              {status.authenticated ? 'connected' : 'not connected'}
            </Badge>
          )}
          <Button size="sm" variant="outline" onClick={() => void loadData()}>
            Refresh
          </Button>
        </div>
      </div>

      {/* Main tabs */}
      <Tabs value={activeTab} onValueChange={setActiveTab}>
        <TabsList
          className="w-full"
          style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
        >
          <TabsTrigger value="drafts" className="flex-1 text-xs">
            Drafts{draftCount > 0 ? ` (${draftCount})` : ''}
          </TabsTrigger>
          <TabsTrigger value="editor" className="flex-1 text-xs">
            {editingDraft ? 'Edit' : 'New post'}
          </TabsTrigger>
          <TabsTrigger value="notes" className="flex-1 text-xs">
            Notes
          </TabsTrigger>
          <TabsTrigger value="analytics" className="flex-1 text-xs">
            Analytics
          </TabsTrigger>
        </TabsList>

        {/* Drafts tab */}
        <TabsContent value="drafts">
          {drafts.length === 0 ? (
            <div className="py-8 text-center">
              <p className="text-sm" style={{ color: 'var(--text-secondary)' }}>
                No drafts yet.
              </p>
              <Button
                size="sm"
                variant="outline"
                className="mt-3"
                onClick={() => setActiveTab('editor')}
              >
                Create a post
              </Button>
            </div>
          ) : (
            <div className="flex flex-col gap-2 mt-2">
              {drafts.map(draft => (
                <DraftCard key={draft.id} draft={draft} onEdit={handleEditDraft} />
              ))}
            </div>
          )}
        </TabsContent>

        {/* Editor tab */}
        <TabsContent value="editor">
          <div className="mt-2">
            {editingDraft && (
              <div className="flex items-center justify-between mb-3">
                <p className="text-xs truncate" style={{ color: 'var(--text-secondary)' }}>
                  Editing: {editingDraft.title}
                </p>
                <Button
                  size="sm"
                  variant="outline"
                  className="h-6 px-2 text-[10px] ml-2 flex-shrink-0"
                  onClick={() => setEditingDraft(undefined)}
                >
                  New instead
                </Button>
              </div>
            )}
            <DraftEditor
              key={editingDraft?.id ?? 'new'}
              initial={editingDraft}
              onPublished={handlePublished}
            />
          </div>
        </TabsContent>

        {/* Notes tab */}
        <TabsContent value="notes">
          <div className="flex flex-col gap-3 mt-2">
            <NoteComposer onPosted={handleNotePosted} />
            {notes.length === 0 ? (
              <p className="text-sm text-center py-4" style={{ color: 'var(--text-secondary)' }}>
                No notes yet.
              </p>
            ) : (
              <div className="flex flex-col gap-2">
                {notes.map(note => (
                  <NoteCard key={note.id} note={note} />
                ))}
              </div>
            )}
          </div>
        </TabsContent>

        {/* Analytics tab */}
        <TabsContent value="analytics">
          <div className="mt-2">
            <Card style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}>
              <AnalyticsTable posts={posts} />
            </Card>
          </div>
        </TabsContent>
      </Tabs>
    </div>
  )
}
