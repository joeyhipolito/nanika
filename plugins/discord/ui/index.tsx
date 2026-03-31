import { useState, useEffect, useCallback, useRef, type KeyboardEvent, type ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { queryPluginItems, queryPluginStatus, pluginAction } from '@/lib/wails'
import type { PluginViewProps } from '@/types'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface DiscordChannel {
  id: string
  name: string
  guild?: string
  type?: string
}

interface DiscordStatusData {
  ok: boolean
  connected?: boolean
  bot_name?: string
  guild_count?: number
  channel_count?: number
  error?: string
}

interface Reaction {
  emoji: string
  count: number
  reacted: boolean
}

interface DiscordMessage {
  id: string
  author: string
  avatar_color: string
  content: string
  timestamp: string
  reactions: Reaction[]
  is_voice: boolean
  voice_url?: string
  reply_to?: string
}

type ConnectionState = 'connected' | 'disconnected' | 'loading'
type TabId = 'messages' | 'channels' | 'status'

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const PLUGIN_NAME = 'discord'

const AVATAR_COLORS = [
  '#5865f2', '#ed4245', '#57f287', '#fee75c',
  '#eb459e', '#3ba55c', '#faa61a', '#9c84ef',
]

function avatarColor(name: string): string {
  let hash = 0
  for (let i = 0; i < name.length; i++) hash = name.charCodeAt(i) + ((hash << 5) - hash)
  return AVATAR_COLORS[Math.abs(hash) % AVATAR_COLORS.length]
}

function initials(name: string): string {
  return name
    .split(/\s+/)
    .map((w) => w[0] ?? '')
    .slice(0, 2)
    .join('')
    .toUpperCase()
}

function formatTime(iso: string): string {
  try {
    const d = new Date(iso)
    const now = new Date()
    const diffMs = now.getTime() - d.getTime()
    const diffMin = Math.floor(diffMs / 60000)
    if (diffMin < 1) return 'just now'
    if (diffMin < 60) return `${diffMin}m ago`
    const diffH = Math.floor(diffMin / 60)
    if (diffH < 24) return `${diffH}h ago`
    return d.toLocaleDateString()
  } catch {
    return iso
  }
}

// ---------------------------------------------------------------------------
// Inline SVG icons (no external icon lib in plugin bundles)
// ---------------------------------------------------------------------------

function SendIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <line x1="22" y1="2" x2="11" y2="13" />
      <polygon points="22 2 15 22 11 13 2 9 22 2" />
    </svg>
  )
}

function MicIcon({ active }: { active?: boolean }) {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill={active ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
      <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
      <line x1="12" y1="19" x2="12" y2="23" />
      <line x1="8" y1="23" x2="16" y2="23" />
    </svg>
  )
}

function ReplyIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <polyline points="9 17 4 12 9 7" />
      <path d="M20 18v-2a4 4 0 0 0-4-4H4" />
    </svg>
  )
}

function PlayIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
      <polygon points="5 3 19 12 5 21 5 3" />
    </svg>
  )
}

function PauseIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
      <rect x="6" y="4" width="4" height="16" />
      <rect x="14" y="4" width="4" height="16" />
    </svg>
  )
}

// ---------------------------------------------------------------------------
// Tab bar — native div-based, no Radix dependency
// ---------------------------------------------------------------------------

const TAB_BTN =
  'inline-flex items-center justify-center whitespace-nowrap rounded-sm px-3 py-1.5 text-sm font-medium transition-all focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring'
const TAB_ACTIVE = 'bg-background text-foreground shadow-sm'
const TAB_INACTIVE = 'text-muted-foreground hover:bg-background/50 hover:text-foreground'

interface TabBarProps {
  active: TabId
  onChange: (t: TabId) => void
}

function TabBar({ active, onChange }: TabBarProps) {
  const tabs: { id: TabId; label: string }[] = [
    { id: 'messages', label: 'Messages' },
    { id: 'channels', label: 'Channels' },
    { id: 'status', label: 'Status' },
  ]
  return (
    <div
      role="tablist"
      className="mx-4 mt-2 flex-shrink-0 self-start inline-flex h-10 items-center justify-center rounded-md bg-muted p-1 text-muted-foreground"
    >
      {tabs.map((t) => (
        <button
          key={t.id}
          role="tab"
          aria-selected={active === t.id}
          onClick={() => onChange(t.id)}
          className={`${TAB_BTN} ${active === t.id ? TAB_ACTIVE : TAB_INACTIVE}`}
        >
          {t.label}
        </button>
      ))}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Channel select — native <select>, no Radix dependency
// ---------------------------------------------------------------------------

interface ChannelSelectProps {
  value: string
  onValueChange: (v: string) => void
  channels: DiscordChannel[]
}

function ChannelSelect({ value, onValueChange, channels }: ChannelSelectProps) {
  return (
    <select
      value={value}
      onChange={(e) => onValueChange(e.target.value)}
      aria-label="Select channel"
      className={[
        'h-8 w-56 rounded-md border border-input bg-background px-2 text-xs',
        'focus:outline-none focus:ring-2 focus:ring-ring',
        'disabled:cursor-not-allowed disabled:opacity-50',
      ].join(' ')}
    >
      {!value && <option value="" disabled>Select a channel…</option>}
      {channels.length === 0 ? (
        <option value="" disabled>No channels configured</option>
      ) : (
        channels.map((ch) => (
          <option key={ch.id} value={ch.id}>
            #{ch.name}{ch.guild ? ` (${ch.guild})` : ''}
          </option>
        ))
      )}
    </select>
  )
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

interface VoicePlayerProps {
  url: string
  duration?: number
}

function VoicePlayer({ url, duration }: VoicePlayerProps) {
  const audioRef = useRef<HTMLAudioElement>(null)
  const [playing, setPlaying] = useState(false)
  const [progress, setProgress] = useState(0)

  const toggle = useCallback(() => {
    const audio = audioRef.current
    if (!audio) return
    if (playing) {
      audio.pause()
      setPlaying(false)
    } else {
      void audio.play()
      setPlaying(true)
    }
  }, [playing])

  useEffect(() => {
    const audio = audioRef.current
    if (!audio) return
    const onTimeUpdate = () => {
      if (audio.duration > 0) setProgress((audio.currentTime / audio.duration) * 100)
    }
    const onEnded = () => { setPlaying(false); setProgress(0) }
    audio.addEventListener('timeupdate', onTimeUpdate)
    audio.addEventListener('ended', onEnded)
    return () => {
      audio.removeEventListener('timeupdate', onTimeUpdate)
      audio.removeEventListener('ended', onEnded)
    }
  }, [])

  return (
    <div className="flex items-center gap-2 rounded-md bg-muted px-3 py-2 w-48">
      <audio ref={audioRef} src={url} preload="metadata" />
      <button
        onClick={toggle}
        className="flex-shrink-0 text-foreground hover:text-primary transition-colors"
        aria-label={playing ? 'Pause voice message' : 'Play voice message'}
      >
        {playing ? <PauseIcon /> : <PlayIcon />}
      </button>
      <div className="flex-1 h-1.5 rounded-full bg-border overflow-hidden">
        <div
          className="h-full rounded-full bg-primary transition-all"
          style={{ width: `${progress}%` }}
        />
      </div>
      {duration != null && (
        <span className="text-xs text-muted-foreground flex-shrink-0">
          {Math.floor(duration)}s
        </span>
      )}
    </div>
  )
}

interface MessageRowProps {
  message: DiscordMessage
  onReact: (msgId: string, emoji: string) => void
  onReply: (msgId: string) => void
}

function MessageRow({ message, onReact, onReply }: MessageRowProps) {
  const [hovering, setHovering] = useState(false)
  const color = avatarColor(message.author)

  return (
    <div
      className="group relative flex gap-3 px-4 py-2 hover:bg-muted/40 transition-colors rounded-md"
      onMouseEnter={() => setHovering(true)}
      onMouseLeave={() => setHovering(false)}
    >
      {/* Avatar */}
      <div
        className="flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center text-xs font-semibold text-white mt-0.5"
        style={{ backgroundColor: color }}
        aria-hidden
      >
        {initials(message.author)}
      </div>

      {/* Body */}
      <div className="flex-1 min-w-0">
        <div className="flex items-baseline gap-2">
          <span className="text-sm font-semibold text-foreground">{message.author}</span>
          <time className="text-xs text-muted-foreground">{formatTime(message.timestamp)}</time>
        </div>

        {message.reply_to && (
          <p className="text-xs text-muted-foreground mb-1 flex items-center gap-1">
            <ReplyIcon />
            <span className="truncate">Replying to a message</span>
          </p>
        )}

        {message.is_voice && message.voice_url ? (
          <VoicePlayer url={message.voice_url} />
        ) : (
          <p className="text-sm text-foreground break-words">{message.content}</p>
        )}

        {/* Reactions */}
        {message.reactions.length > 0 && (
          <div className="flex flex-wrap gap-1 mt-1.5">
            {message.reactions.map((r) => (
              <button
                key={r.emoji}
                onClick={() => onReact(message.id, r.emoji)}
                className={[
                  'inline-flex items-center gap-1 text-xs px-2 py-0.5 rounded-full border transition-colors',
                  r.reacted
                    ? 'border-primary/50 bg-primary/10 text-primary'
                    : 'border-border bg-muted hover:bg-accent',
                ].join(' ')}
                aria-label={`React with ${r.emoji}, ${r.count} reactions`}
              >
                <span>{r.emoji}</span>
                <span>{r.count}</span>
              </button>
            ))}
          </div>
        )}
      </div>

      {/* Hover actions */}
      {hovering && (
        <div className="absolute right-4 top-2 flex items-center gap-1 bg-background border border-border rounded-md px-1 py-0.5 shadow-sm z-10">
          {['👍', '❤️', '😂'].map((emoji) => (
            <button
              key={emoji}
              onClick={() => onReact(message.id, emoji)}
              className="p-1 rounded hover:bg-muted transition-colors text-sm"
              aria-label={`React ${emoji}`}
            >
              {emoji}
            </button>
          ))}
          <button
            onClick={() => onReply(message.id)}
            className="p-1 rounded hover:bg-muted transition-colors text-muted-foreground hover:text-foreground"
            aria-label="Reply to message"
          >
            <ReplyIcon />
          </button>
        </div>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Status helpers
// ---------------------------------------------------------------------------

function connectionState(status: DiscordStatusData | null): ConnectionState {
  if (status === null) return 'loading'
  return status.ok && status.connected !== false ? 'connected' : 'disconnected'
}

function ConnectionBadge({ state }: { state: ConnectionState }) {
  if (state === 'loading') {
    return <Badge variant="secondary">Connecting…</Badge>
  }
  if (state === 'connected') {
    return (
      <Badge className="bg-green-500/15 text-green-600 border-green-500/30 border">
        Connected
      </Badge>
    )
  }
  return <Badge variant="destructive">Disconnected</Badge>
}

// ---------------------------------------------------------------------------
// Tab content wrappers
// ---------------------------------------------------------------------------

function TabPanel({ id, active, className, children }: { id: TabId; active: TabId; className?: string; children: ReactNode }) {
  if (id !== active) return null
  return (
    <div role="tabpanel" id={`tab-panel-${id}`} className={className}>
      {children}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export default function DiscordPanel({ isConnected }: PluginViewProps) {
  const [activeTab, setActiveTab] = useState<TabId>('messages')
  const [channels, setChannels] = useState<DiscordChannel[]>([])
  const [status, setStatus] = useState<DiscordStatusData | null>(null)
  const [selectedChannel, setSelectedChannel] = useState<string>('')
  const [messages, setMessages] = useState<DiscordMessage[]>([])
  const [inputText, setInputText] = useState('')
  const [replyTo, setReplyTo] = useState<string | null>(null)
  const [sending, setSending] = useState(false)
  const [recording, setRecording] = useState(false)
  const [sendError, setSendError] = useState<string | null>(null)
  const messagesEndRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLTextAreaElement>(null)

  // Scroll to bottom on new messages
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages])

  // Load channels and status
  const refresh = useCallback(async () => {
    try {
      const [rawItems, rawStatus] = await Promise.all([
        queryPluginItems(PLUGIN_NAME),
        queryPluginStatus(PLUGIN_NAME),
      ])
      const parsedChannels = rawItems.map((item) => ({
        id: String(item.id ?? item.channel_id ?? ''),
        name: String(item.name ?? item.channel_name ?? item.id ?? 'Unknown'),
        guild: item.guild != null ? String(item.guild) : undefined,
        type: item.type != null ? String(item.type) : undefined,
      }))
      setChannels(parsedChannels)
      if (parsedChannels.length > 0 && !selectedChannel) {
        setSelectedChannel(parsedChannels[0].id)
      }
      setStatus({
        ok: rawStatus.ok === true,
        connected: rawStatus.connected !== false,
        bot_name: rawStatus.bot_name != null ? String(rawStatus.bot_name) : undefined,
        guild_count: rawStatus.guild_count != null ? Number(rawStatus.guild_count) : undefined,
        channel_count: rawStatus.channel_count != null ? Number(rawStatus.channel_count) : undefined,
        error: rawStatus.error != null ? String(rawStatus.error) : undefined,
      })
    } catch {
      setStatus({ ok: false, connected: false, error: 'Failed to load plugin data' })
    }
  }, [selectedChannel])

  useEffect(() => {
    void refresh()
  }, [refresh])

  const handleSend = useCallback(async () => {
    const text = inputText.trim()
    if (!text || !selectedChannel) return
    setSending(true)
    setSendError(null)
    try {
      await pluginAction(PLUGIN_NAME, 'reply', selectedChannel)
      const optimistic: DiscordMessage = {
        id: `local-${Date.now()}`,
        author: 'You',
        avatar_color: AVATAR_COLORS[0],
        content: text,
        timestamp: new Date().toISOString(),
        reactions: [],
        is_voice: false,
        reply_to: replyTo ?? undefined,
      }
      setMessages((prev) => [...prev, optimistic])
      setInputText('')
      setReplyTo(null)
    } catch {
      setSendError('Failed to send message. Check bot configuration.')
    } finally {
      setSending(false)
      inputRef.current?.focus()
    }
  }, [inputText, selectedChannel, replyTo])

  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault()
        void handleSend()
      }
    },
    [handleSend],
  )

  const handleVoice = useCallback(async () => {
    if (!selectedChannel) return
    setRecording(true)
    setSendError(null)
    try {
      await pluginAction(PLUGIN_NAME, 'send-voice', selectedChannel)
    } catch {
      setSendError('Voice send failed. Use the discord CLI directly.')
    } finally {
      setRecording(false)
    }
  }, [selectedChannel])

  const handleReact = useCallback((msgId: string, emoji: string) => {
    setMessages((prev) =>
      prev.map((msg) => {
        if (msg.id !== msgId) return msg
        const existing = msg.reactions.find((r) => r.emoji === emoji)
        if (existing) {
          return {
            ...msg,
            reactions: msg.reactions
              .map((r) =>
                r.emoji === emoji
                  ? { ...r, count: r.reacted ? r.count - 1 : r.count + 1, reacted: !r.reacted }
                  : r,
              )
              .filter((r) => r.count > 0),
          }
        }
        return { ...msg, reactions: [...msg.reactions, { emoji, count: 1, reacted: true }] }
      }),
    )
  }, [])

  const handleReply = useCallback((msgId: string) => {
    setReplyTo(msgId)
    inputRef.current?.focus()
  }, [])

  const connState = connectionState(status)
  const selectedChannelName = channels.find((c) => c.id === selectedChannel)?.name ?? selectedChannel

  return (
    <div className="flex flex-col h-full min-h-0">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-border flex-shrink-0">
        <div className="flex items-center gap-2">
          <span className="text-sm font-semibold text-foreground">Discord</span>
          {status?.bot_name && (
            <span className="text-xs text-muted-foreground">· {status.bot_name}</span>
          )}
        </div>
        <ConnectionBadge state={connState} />
      </div>

      {/* Tab bar */}
      <TabBar active={activeTab} onChange={setActiveTab} />

      <div className="flex flex-col flex-1 min-h-0">
        {/* ── Messages tab ── */}
        <TabPanel id="messages" active={activeTab} className="flex flex-col flex-1 min-h-0">
          {/* Channel selector */}
          <div className="px-4 py-2 flex-shrink-0 border-b border-border">
            <ChannelSelect
              value={selectedChannel}
              onValueChange={setSelectedChannel}
              channels={channels}
            />
          </div>

          {/* Message list */}
          <div
            className="flex-1 overflow-y-auto py-2 min-h-0"
            role="log"
            aria-label={`Messages in #${selectedChannelName}`}
            aria-live="polite"
          >
            {messages.length === 0 ? (
              <div className="flex flex-col items-center justify-center h-full gap-2 text-muted-foreground">
                <div className="text-4xl" aria-hidden>💬</div>
                <p className="text-sm">
                  {selectedChannel
                    ? `No messages in #${selectedChannelName}`
                    : 'Select a channel to start messaging'}
                </p>
                <p className="text-xs opacity-60">
                  Message history is not available — use the compose area to send.
                </p>
              </div>
            ) : (
              messages.map((msg) => (
                <MessageRow
                  key={msg.id}
                  message={msg}
                  onReact={handleReact}
                  onReply={handleReply}
                />
              ))
            )}
            <div ref={messagesEndRef} />
          </div>

          {/* Compose area */}
          <div className="flex-shrink-0 border-t border-border px-4 py-3">
            {replyTo && (
              <div className="flex items-center gap-2 mb-2 text-xs text-muted-foreground">
                <ReplyIcon />
                <span>
                  Replying to{' '}
                  <span className="font-medium text-foreground">
                    {messages.find((m) => m.id === replyTo)?.author ?? 'message'}
                  </span>
                </span>
                <button
                  onClick={() => setReplyTo(null)}
                  className="ml-auto hover:text-foreground transition-colors"
                  aria-label="Cancel reply"
                >
                  ✕
                </button>
              </div>
            )}

            {sendError && (
              <p className="text-xs text-destructive mb-2" role="alert">{sendError}</p>
            )}

            <div className="flex items-end gap-2">
              <textarea
                ref={inputRef}
                value={inputText}
                onChange={(e) => setInputText(e.target.value)}
                onKeyDown={handleKeyDown}
                placeholder={
                  selectedChannel
                    ? `Message #${selectedChannelName}…`
                    : 'Select a channel first…'
                }
                disabled={!selectedChannel || sending}
                rows={1}
                className={[
                  'flex-1 resize-none rounded-md border border-input bg-background px-3 py-2',
                  'text-sm placeholder:text-muted-foreground',
                  'focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2',
                  'disabled:cursor-not-allowed disabled:opacity-50',
                  'max-h-32 overflow-y-auto',
                ].join(' ')}
                aria-label="Message input"
                aria-multiline="true"
              />
              <Button
                variant="outline"
                size="icon"
                onClick={() => void handleVoice()}
                disabled={!selectedChannel || sending || recording}
                aria-label={recording ? 'Sending voice message…' : 'Send voice message'}
                className={recording ? 'text-destructive border-destructive/50' : ''}
              >
                <MicIcon active={recording} />
              </Button>
              <Button
                size="icon"
                onClick={() => void handleSend()}
                disabled={!inputText.trim() || !selectedChannel || sending}
                aria-label="Send message"
              >
                <SendIcon />
              </Button>
            </div>
          </div>
        </TabPanel>

        {/* ── Channels tab ── */}
        <TabPanel id="channels" active={activeTab} className="flex-1 overflow-y-auto px-4 py-2">
          {channels.length === 0 ? (
            <div className="flex flex-col items-center justify-center h-32 gap-2 text-muted-foreground">
              <p className="text-sm">No channels configured</p>
              <p className="text-xs opacity-60">
                Run <code className="font-mono bg-muted px-1 rounded">discord doctor</code> to check setup.
              </p>
            </div>
          ) : (
            <ul className="space-y-1" role="list" aria-label="Configured Discord channels">
              {channels.map((ch) => (
                <li key={ch.id}>
                  <button
                    onClick={() => setSelectedChannel(ch.id)}
                    className={[
                      'w-full flex items-center gap-3 px-3 py-2 rounded-md text-left transition-colors',
                      selectedChannel === ch.id
                        ? 'bg-primary/10 text-primary'
                        : 'hover:bg-muted text-foreground',
                    ].join(' ')}
                    aria-current={selectedChannel === ch.id ? 'true' : undefined}
                  >
                    <span className="text-muted-foreground font-medium">#</span>
                    <div className="flex-1 min-w-0">
                      <p className="text-sm font-medium truncate">{ch.name}</p>
                      {ch.guild && (
                        <p className="text-xs text-muted-foreground truncate">{ch.guild}</p>
                      )}
                    </div>
                    <span className="text-xs text-muted-foreground font-mono opacity-60 truncate max-w-24">
                      {ch.id}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          <div className="mt-4 flex justify-end">
            <Button variant="outline" size="sm" onClick={() => void refresh()}>
              Refresh
            </Button>
          </div>
        </TabPanel>

        {/* ── Status tab ── */}
        <TabPanel id="status" active={activeTab} className="flex-1 overflow-y-auto px-4 py-2">
          <div className="space-y-4">
            <div className="rounded-lg border border-border p-4 space-y-3">
              <h3 className="text-sm font-semibold">Connection</h3>
              <div className="grid grid-cols-2 gap-x-4 gap-y-2 text-sm">
                <span className="text-muted-foreground">Status</span>
                <ConnectionBadge state={connState} />
                {status?.bot_name && (
                  <>
                    <span className="text-muted-foreground">Bot</span>
                    <span className="font-mono text-xs">{status.bot_name}</span>
                  </>
                )}
                {status?.guild_count != null && (
                  <>
                    <span className="text-muted-foreground">Servers</span>
                    <span>{status.guild_count}</span>
                  </>
                )}
                {status?.channel_count != null && (
                  <>
                    <span className="text-muted-foreground">Channels</span>
                    <span>{status.channel_count}</span>
                  </>
                )}
                {isConnected != null && (
                  <>
                    <span className="text-muted-foreground">Dashboard WS</span>
                    <Badge variant={isConnected ? 'default' : 'secondary'}>
                      {isConnected ? 'Live' : 'Polling'}
                    </Badge>
                  </>
                )}
              </div>
            </div>

            {status?.error && (
              <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-4">
                <p className="text-sm font-semibold text-destructive mb-1">Error</p>
                <p className="text-xs text-muted-foreground">{status.error}</p>
              </div>
            )}

            <div className="rounded-lg border border-border p-4 space-y-2">
              <h3 className="text-sm font-semibold">Quick Reference</h3>
              <ul className="space-y-1 text-xs text-muted-foreground font-mono">
                <li>discord reply --channel &lt;id&gt; --message &quot;…&quot;</li>
                <li>discord send-voice-message --channel &lt;id&gt; --audio file.mp3</li>
                <li>discord query status --json</li>
                <li>discord doctor</li>
              </ul>
            </div>

            <div className="flex justify-end">
              <Button variant="outline" size="sm" onClick={() => void refresh()}>
                Refresh Status
              </Button>
            </div>
          </div>
        </TabPanel>
      </div>
    </div>
  )
}
