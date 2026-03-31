import { useState, useEffect, useCallback, useRef } from 'react'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { queryPluginItems, queryPluginStatus, pluginAction } from '@/lib/wails'
import type { PluginViewProps } from '@/types'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface ChatItem {
  id: string
  type: string
}

interface TelegramStatus {
  ok: boolean
  version: string
  chats: number
  token_prefix?: string
  error?: string
}

type MessageType = 'text' | 'voice'

interface SentMessage {
  id: string
  chat_id: string
  type: MessageType
  content: string
  sent_at: string
  reactions: string[]
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const PLUGIN_NAME = 'telegram'
const REACTION_EMOJIS = ['👍', '❤️', '😂', '🔥', '👏']

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatTime(iso: string): string {
  try {
    const d = new Date(iso)
    const now = new Date()
    const isToday = d.toDateString() === now.toDateString()
    if (isToday) return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' }) +
      ' ' + d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  } catch {
    return iso
  }
}

function uniqueId(): string {
  return Math.random().toString(36).slice(2)
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

interface ChatSelectorProps {
  chats: ChatItem[]
  activeChat: string | null
  onSelect: (id: string) => void
}

function ChatSelector({ chats, activeChat, onSelect }: ChatSelectorProps) {
  if (chats.length === 0) {
    return (
      <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
        No chats configured. Add chat IDs to ~/.alluka/channels/telegram.json.
      </p>
    )
  }
  return (
    <nav className="flex gap-1.5 overflow-x-auto pb-0.5" aria-label="Chat selector">
      {chats.map(chat => (
        <button
          key={chat.id}
          onClick={() => onSelect(chat.id)}
          aria-pressed={activeChat === chat.id}
          className="shrink-0 rounded-md px-2.5 py-1 text-xs font-medium font-mono transition-colors"
          style={{
            background: activeChat === chat.id
              ? 'color-mix(in srgb, var(--accent) 15%, transparent)'
              : 'var(--mic-bg)',
            color: activeChat === chat.id ? 'var(--accent)' : 'var(--text-secondary)',
            border: `1px solid ${activeChat === chat.id ? 'var(--accent)' : 'var(--pill-border)'}`,
          }}
        >
          {chat.id}
        </button>
      ))}
    </nav>
  )
}

interface MessageBubbleProps {
  message: SentMessage
  onReact: (id: string, emoji: string) => void
  onReply: (message: SentMessage) => void
}

function MessageBubble({ message, onReact, onReply }: MessageBubbleProps) {
  const [showReactions, setShowReactions] = useState(false)

  return (
    <article className="group flex flex-col gap-1" aria-label={`Sent message: ${message.content.slice(0, 40)}`}>
      {/* Bubble */}
      <div
        className="max-w-[85%] self-end rounded-lg rounded-br-sm px-3 py-2"
        style={{
          background: 'color-mix(in srgb, var(--accent) 18%, transparent)',
          border: '1px solid color-mix(in srgb, var(--accent) 28%, transparent)',
        }}
      >
        {/* Voice message */}
        {message.type === 'voice' && (
          <div className="mb-1.5 flex flex-col gap-1">
            <span className="text-[10px] font-medium" style={{ color: 'var(--accent)' }}>
              Voice message
            </span>
            <audio
              src={message.content}
              controls
              className="h-6 w-full max-w-[200px]"
              aria-label="Voice message playback"
            />
          </div>
        )}
        {/* Text content */}
        {message.type === 'text' && (
          <p
            className="whitespace-pre-wrap text-xs leading-relaxed"
            style={{ color: 'var(--text-primary)' }}
          >
            {message.content}
          </p>
        )}
        {/* Meta: chat + time */}
        <div className="mt-1 flex items-center justify-between gap-2">
          <span className="font-mono text-[9px]" style={{ color: 'var(--text-secondary)' }}>
            → {message.chat_id}
          </span>
          <time dateTime={message.sent_at} className="text-[9px]" style={{ color: 'var(--text-secondary)' }}>
            {formatTime(message.sent_at)}
          </time>
        </div>
      </div>

      {/* Reactions display */}
      {message.reactions.length > 0 && (
        <div className="flex gap-1 self-end">
          {message.reactions.map((emoji, i) => (
            <span
              key={i}
              className="rounded-full px-1.5 py-0.5 text-xs"
              style={{ background: 'var(--mic-bg)', border: '1px solid var(--pill-border)' }}
            >
              {emoji}
            </span>
          ))}
        </div>
      )}

      {/* Hover actions */}
      <div
        className="flex gap-1 self-end opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100"
        role="group"
        aria-label="Message actions"
      >
        <button
          onClick={() => setShowReactions(v => !v)}
          className="rounded px-1.5 py-0.5 text-[10px] transition-opacity hover:opacity-80"
          style={{
            background: 'var(--mic-bg)',
            color: 'var(--text-secondary)',
            border: '1px solid var(--pill-border)',
          }}
          aria-label="Add reaction"
          aria-expanded={showReactions}
        >
          React
        </button>
        <button
          onClick={() => onReply(message)}
          className="rounded px-1.5 py-0.5 text-[10px] transition-opacity hover:opacity-80"
          style={{
            background: 'var(--mic-bg)',
            color: 'var(--text-secondary)',
            border: '1px solid var(--pill-border)',
          }}
          aria-label="Reply to this message"
        >
          Reply
        </button>
      </div>

      {/* Reaction picker */}
      {showReactions && (
        <div
          className="flex gap-1 self-end rounded-lg p-1.5"
          style={{ background: 'var(--mic-bg)', border: '1px solid var(--pill-border)' }}
          role="group"
          aria-label="Pick a reaction"
        >
          {REACTION_EMOJIS.map(emoji => (
            <button
              key={emoji}
              onClick={() => {
                onReact(message.id, emoji)
                setShowReactions(false)
              }}
              className="rounded p-1 text-sm transition-transform hover:scale-125"
              aria-label={`React with ${emoji}`}
            >
              {emoji}
            </button>
          ))}
        </div>
      )}
    </article>
  )
}

interface VoiceRecorderProps {
  chatId: string | null
  onSent: (message: SentMessage) => void
}

function VoiceRecorder({ chatId, onSent }: VoiceRecorderProps) {
  const [recording, setRecording] = useState(false)
  const [audioUrl, setAudioUrl] = useState<string | null>(null)
  const [sending, setSending] = useState(false)
  const [feedback, setFeedback] = useState('')
  const [filePath, setFilePath] = useState('')
  const recorderRef = useRef<MediaRecorder | null>(null)
  const chunksRef = useRef<Blob[]>([])

  const startRecording = async () => {
    if (!navigator.mediaDevices?.getUserMedia) {
      setFeedback('Audio recording not supported in this environment')
      return
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true })
      chunksRef.current = []
      const recorder = new MediaRecorder(stream)
      recorder.ondataavailable = e => {
        if (e.data.size > 0) chunksRef.current.push(e.data)
      }
      recorder.onstop = () => {
        const blob = new Blob(chunksRef.current, { type: 'audio/webm' })
        const url = URL.createObjectURL(blob)
        setAudioUrl(url)
        stream.getTracks().forEach(t => t.stop())
      }
      recorderRef.current = recorder
      recorder.start()
      setRecording(true)
      setAudioUrl(null)
      setFeedback('')
    } catch {
      setFeedback('Microphone access denied')
    }
  }

  const stopRecording = () => {
    recorderRef.current?.stop()
    setRecording(false)
  }

  const sendVoice = async () => {
    if (!chatId || !filePath.trim()) {
      setFeedback('Select a chat and enter an audio file path')
      return
    }
    setSending(true)
    setFeedback('')
    try {
      const payload = JSON.stringify({ chat: chatId, audio: filePath.trim() })
      const result = await pluginAction(PLUGIN_NAME, 'send-voice', payload)
      if (result['ok'] !== false) {
        onSent({
          id: uniqueId(),
          chat_id: chatId,
          type: 'voice',
          content: audioUrl ?? filePath.trim(),
          sent_at: new Date().toISOString(),
          reactions: [],
        })
        setAudioUrl(null)
        setFilePath('')
        setFeedback('Voice message sent')
      } else {
        setFeedback(String(result['error'] ?? 'Send failed'))
      }
    } catch (err) {
      setFeedback(err instanceof Error ? err.message : 'Send failed')
    } finally {
      setSending(false)
      setTimeout(() => setFeedback(''), 4_000)
    }
  }

  return (
    <div
      className="flex flex-col gap-2 rounded-lg p-3"
      style={{ background: 'var(--mic-bg)', border: '1px solid var(--pill-border)' }}
      aria-label="Voice message panel"
    >
      {/* Record controls + preview */}
      <div className="flex items-center gap-2">
        <Button
          size="sm"
          variant={recording ? 'destructive' : 'outline'}
          onClick={recording ? stopRecording : () => void startRecording()}
          className="h-7 px-2.5 text-xs"
          aria-label={recording ? 'Stop recording' : 'Start recording voice message'}
        >
          {recording ? '■ Stop' : '● Record'}
        </Button>
        {recording && (
          <span
            className="animate-pulse text-[10px] font-medium"
            style={{ color: 'var(--color-error)' }}
            aria-live="polite"
          >
            Recording…
          </span>
        )}
        {audioUrl && !recording && (
          <audio
            src={audioUrl}
            controls
            className="h-7 flex-1"
            aria-label="Recorded voice preview"
          />
        )}
      </div>

      {/* File path input + send */}
      <div className="flex gap-1.5">
        <input
          type="text"
          value={filePath}
          onChange={e => setFilePath(e.target.value)}
          placeholder="/path/to/audio.ogg"
          className="flex-1 rounded-md px-2 py-1 text-xs outline-none focus:ring-1"
          style={{
            background: 'var(--bg)',
            color: 'var(--text-primary)',
            border: '1px solid var(--pill-border)',
          }}
          aria-label="Audio file path to send"
        />
        <Button
          size="sm"
          variant="outline"
          disabled={!chatId || !filePath.trim() || sending}
          onClick={() => void sendVoice()}
          className="h-7 shrink-0 px-2.5 text-xs"
          aria-label="Send voice message from file path"
        >
          {sending ? '…' : 'Send'}
        </Button>
      </div>

      {feedback && (
        <p
          className="text-[10px]"
          style={{
            color: feedback.includes('sent') ? 'var(--color-success)' : 'var(--color-error)',
          }}
          aria-live="polite"
        >
          {feedback}
        </p>
      )}
    </div>
  )
}

interface BotStatusCardProps {
  status: TelegramStatus | null
  loading: boolean
}

function BotStatusCard({ status, loading }: BotStatusCardProps) {
  if (loading) {
    return (
      <Card
        className="flex flex-col gap-2 p-4"
        style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
      >
        <div className="h-4 w-24 animate-pulse rounded" style={{ background: 'var(--pill-border)' }} />
        <div className="h-3 w-40 animate-pulse rounded" style={{ background: 'var(--pill-border)' }} />
      </Card>
    )
  }

  if (!status) return null

  return (
    <Card
      className="p-4"
      style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
      role="region"
      aria-label="Bot status"
    >
      <div className="mb-3 flex items-center gap-2">
        <h2 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
          Bot Status
        </h2>
        <Badge variant={status.ok ? 'secondary' : 'destructive'} className="text-[10px]">
          {status.ok ? 'connected' : 'error'}
        </Badge>
      </div>

      {status.error ? (
        <p className="text-xs" style={{ color: 'var(--color-error)' }}>
          {status.error}
        </p>
      ) : (
        <dl className="grid grid-cols-2 gap-x-4 gap-y-2">
          {status.token_prefix && (
            <>
              <dt
                className="text-[10px] uppercase tracking-wider"
                style={{ color: 'var(--text-secondary)' }}
              >
                Token
              </dt>
              <dd className="font-mono text-xs" style={{ color: 'var(--text-primary)' }}>
                {status.token_prefix}
              </dd>
            </>
          )}
          <dt
            className="text-[10px] uppercase tracking-wider"
            style={{ color: 'var(--text-secondary)' }}
          >
            Chats
          </dt>
          <dd className="font-mono text-xs font-bold" style={{ color: 'var(--text-primary)' }}>
            {status.chats}
          </dd>
          <dt
            className="text-[10px] uppercase tracking-wider"
            style={{ color: 'var(--text-secondary)' }}
          >
            Version
          </dt>
          <dd className="font-mono text-[10px]" style={{ color: 'var(--text-secondary)' }}>
            v{status.version}
          </dd>
        </dl>
      )}
    </Card>
  )
}

// ---------------------------------------------------------------------------
// Main view
// ---------------------------------------------------------------------------

export default function TelegramView(_props: PluginViewProps) {
  const [chats, setChats] = useState<ChatItem[]>([])
  const [status, setStatus] = useState<TelegramStatus | null>(null)
  const [messages, setMessages] = useState<SentMessage[]>([])
  const [activeChat, setActiveChat] = useState<string | null>(null)
  const [text, setText] = useState('')
  const [sending, setSending] = useState(false)
  const [feedback, setFeedback] = useState('')
  const [showVoice, setShowVoice] = useState(false)
  const [replyTo, setReplyTo] = useState<SentMessage | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')
  const messagesEndRef = useRef<HTMLDivElement>(null)

  const loadData = useCallback(async () => {
    try {
      const [itemsRes, statusRes] = await Promise.allSettled([
        queryPluginItems(PLUGIN_NAME),
        queryPluginStatus(PLUGIN_NAME),
      ])
      if (itemsRes.status === 'fulfilled') {
        const items = itemsRes.value as unknown as ChatItem[]
        setChats(items)
        // Auto-select first chat on initial load
        setActiveChat(prev => prev ?? (items.length > 0 ? items[0].id : null))
      }
      if (statusRes.status === 'fulfilled') {
        setStatus(statusRes.value as unknown as TelegramStatus)
      }
      setError('')
    } catch {
      setError('Failed to load Telegram data')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void loadData()
  }, [loadData])

  // Auto-scroll to bottom when new messages arrive
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages])

  const addMessage = (msg: SentMessage) => setMessages(prev => [...prev, msg])

  const sendText = async () => {
    const trimmed = text.trim()
    if (!activeChat || !trimmed || sending) return

    const content = replyTo
      ? `↩ ${replyTo.content.slice(0, 40)}${replyTo.content.length > 40 ? '…' : ''}\n\n${trimmed}`
      : trimmed

    setSending(true)
    setFeedback('')
    try {
      const payload = JSON.stringify({ chat: activeChat, text: content })
      const result = await pluginAction(PLUGIN_NAME, 'send', payload)
      if (result['ok'] !== false) {
        addMessage({
          id: uniqueId(),
          chat_id: activeChat,
          type: 'text',
          content,
          sent_at: new Date().toISOString(),
          reactions: [],
        })
        setText('')
        setReplyTo(null)
        setFeedback('Sent')
      } else {
        setFeedback(String(result['error'] ?? 'Send failed'))
      }
    } catch (err) {
      setFeedback(err instanceof Error ? err.message : 'Send failed')
    } finally {
      setSending(false)
      setTimeout(() => setFeedback(''), 3_000)
    }
  }

  const handleReact = (msgId: string, emoji: string) => {
    setMessages(prev =>
      prev.map(m =>
        m.id === msgId
          ? {
              ...m,
              reactions: m.reactions.includes(emoji)
                ? m.reactions.filter(e => e !== emoji)
                : [...m.reactions, emoji],
            }
          : m,
      ),
    )
  }

  const chatMessages = messages.filter(m => m.chat_id === activeChat)
  const totalSent = messages.length

  return (
    <div className="flex flex-col gap-3 p-4">
      {/* Error banner */}
      {error && (
        <p
          className="rounded px-2 py-1 text-xs"
          style={{
            background: 'color-mix(in srgb, var(--color-error) 12%, transparent)',
            color: 'var(--color-error)',
          }}
          role="alert"
        >
          {error}
        </p>
      )}

      <Tabs defaultValue="chats">
        <TabsList className="w-full">
          <TabsTrigger value="chats" className="flex-1 text-xs">
            Chats{totalSent > 0 ? ` (${totalSent})` : ''}
          </TabsTrigger>
          <TabsTrigger value="bot" className="flex-1 text-xs">
            Bot
          </TabsTrigger>
        </TabsList>

        {/* ── Chats tab ─────────────────────────────────────────────────── */}
        <TabsContent value="chats" className="flex flex-col gap-3">
          {/* Chat selector */}
          <ChatSelector chats={chats} activeChat={activeChat} onSelect={setActiveChat} />

          {/* Message list */}
          <Card
            style={{
              background: 'var(--mic-bg)',
              borderColor: 'var(--pill-border)',
            }}
            aria-label="Sent messages"
          >
            <div
              style={{ minHeight: '160px', maxHeight: '300px', overflowY: 'auto' }}
            >
              {chatMessages.length === 0 ? (
                <div className="flex h-40 items-center justify-center">
                  <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
                    {activeChat ? 'No messages sent yet in this chat' : 'Select a chat to start'}
                  </p>
                </div>
              ) : (
                <div className="flex flex-col gap-3 p-3">
                  {chatMessages.map(msg => (
                    <MessageBubble
                      key={msg.id}
                      message={msg}
                      onReact={handleReact}
                      onReply={setReplyTo}
                    />
                  ))}
                  <div ref={messagesEndRef} aria-hidden="true" />
                </div>
              )}
            </div>
          </Card>

          {/* Reply banner */}
          {replyTo && (
            <div
              className="flex items-center justify-between gap-2 rounded-md px-2.5 py-1.5"
              style={{
                background: 'color-mix(in srgb, var(--accent) 10%, transparent)',
                border: '1px solid color-mix(in srgb, var(--accent) 20%, transparent)',
              }}
              aria-label="Reply context"
            >
              <span className="text-xs" style={{ color: 'var(--text-secondary)' }}>
                Replying to:{' '}
                <em>
                  {replyTo.content.slice(0, 50)}
                  {replyTo.content.length > 50 ? '…' : ''}
                </em>
              </span>
              <button
                onClick={() => setReplyTo(null)}
                className="text-[10px] hover:opacity-80"
                style={{ color: 'var(--text-secondary)' }}
                aria-label="Cancel reply"
              >
                ✕
              </button>
            </div>
          )}

          {/* Text compose */}
          <div className="flex flex-col gap-1.5">
            <div className="flex gap-1.5">
              <textarea
                value={text}
                onChange={e => setText(e.target.value)}
                onKeyDown={e => {
                  if (e.key === 'Enter' && !e.shiftKey) {
                    e.preventDefault()
                    void sendText()
                  }
                }}
                placeholder={activeChat ? `Message ${activeChat}… (Enter to send)` : 'Select a chat first'}
                disabled={!activeChat || sending}
                rows={2}
                className="flex-1 resize-none rounded-md px-2.5 py-1.5 text-xs outline-none"
                style={{
                  background: 'var(--bg)',
                  color: 'var(--text-primary)',
                  border: '1px solid var(--pill-border)',
                }}
                aria-label="Message text input"
              />
              <div className="flex flex-col gap-1">
                <Button
                  size="sm"
                  variant="outline"
                  disabled={!activeChat || !text.trim() || sending}
                  onClick={() => void sendText()}
                  className="h-auto flex-1 px-2.5 text-xs"
                  aria-label="Send text message"
                >
                  {sending ? '…' : 'Send'}
                </Button>
                <Button
                  size="sm"
                  variant={showVoice ? 'secondary' : 'outline'}
                  onClick={() => setShowVoice(v => !v)}
                  className="h-auto flex-1 px-2.5 text-xs"
                  aria-label="Toggle voice message panel"
                  aria-expanded={showVoice}
                >
                  Voice
                </Button>
              </div>
            </div>

            {feedback && (
              <p
                className="text-[10px]"
                style={{
                  color: feedback === 'Sent' ? 'var(--color-success)' : 'var(--color-error)',
                }}
                aria-live="polite"
              >
                {feedback}
              </p>
            )}
          </div>

          {/* Voice panel */}
          {showVoice && <VoiceRecorder chatId={activeChat} onSent={addMessage} />}
        </TabsContent>

        {/* ── Bot tab ───────────────────────────────────────────────────── */}
        <TabsContent value="bot" className="flex flex-col gap-3">
          <BotStatusCard status={status} loading={loading} />

          {/* Configured chats list */}
          {chats.length > 0 && (
            <Card
              style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
              role="region"
              aria-label="Configured chats"
            >
              <h3
                className="border-b px-3 py-2 text-[10px] font-medium uppercase tracking-wider"
                style={{ color: 'var(--text-secondary)', borderColor: 'var(--pill-border)' }}
              >
                Configured Chats ({chats.length})
              </h3>
              <ul className="divide-y" style={{ borderColor: 'var(--pill-border)' }}>
                {chats.map(chat => (
                  <li key={chat.id} className="flex items-center justify-between px-3 py-2">
                    <span className="font-mono text-xs" style={{ color: 'var(--text-primary)' }}>
                      {chat.id}
                    </span>
                    <Badge variant="outline" className="text-[9px] capitalize">
                      {chat.type}
                    </Badge>
                  </li>
                ))}
              </ul>
            </Card>
          )}

          <div className="flex justify-end">
            <Button
              size="sm"
              variant="outline"
              onClick={() => void loadData()}
              disabled={loading}
              aria-label="Refresh bot status"
            >
              {loading ? 'Loading…' : 'Refresh'}
            </Button>
          </div>
        </TabsContent>
      </Tabs>
    </div>
  )
}
