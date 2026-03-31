import { useState, useCallback, useEffect, useRef } from 'react'
import type { ConversationSummary, ConversationDetail, BackendMessage, RunMissionResult } from '../types'
import { getModule } from '../modules/registry'
import { useDashboardState } from './dashboardContext'
import { runMission } from '../lib/wails'
import './ConversationPanel.css'

const BASE = '/api/orchestrator'
const SELECTED_ID_KEY = 'nanika.conv.panel.selectedId'

// @ extension triggers available in the input
const AT_EXTENSIONS = [
  { id: 'missions', hint: 'Running missions' },
  { id: 'findings', hint: 'Recent NEN findings' },
  { id: 'events', hint: 'Recent activity' },
  { id: 'personas', hint: 'Active personas' },
]

// Pattern: "run <task>" — intercepted and sent to POST /api/missions/run
const RUN_PATTERN = /^run\s+(.+)$/i

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatRelativeTime(iso: string): string {
  if (!iso) return ''
  const diff = Date.now() - new Date(iso).getTime()
  if (diff < 60_000) return 'just now'
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`
  return `${Math.floor(diff / 86_400_000)}d ago`
}

function truncate(text: string, max: number): string {
  return text.length > max ? text.slice(0, max) + '…' : text
}

// ---------------------------------------------------------------------------
// ThinkingDots
// ---------------------------------------------------------------------------

function ThinkingDots() {
  return (
    <span className="conv-thinking" aria-label="Working…">
      <span className="conv-thinking-dot" style={{ animationDelay: '0ms' }} />
      <span className="conv-thinking-dot" style={{ animationDelay: '160ms' }} />
      <span className="conv-thinking-dot" style={{ animationDelay: '320ms' }} />
    </span>
  )
}

// ---------------------------------------------------------------------------
// MessageBubble
// ---------------------------------------------------------------------------

interface MessageBubbleProps {
  role: 'user' | 'assistant' | 'system'
  content: string
  isStreaming?: boolean
  isThinking?: boolean
}

function MessageBubble({ role, content, isStreaming, isThinking }: MessageBubbleProps) {
  const isEmpty = content === '' && isStreaming

  if (role === 'system') {
    return (
      <div className="conv-bubble conv-bubble--system">
        <span className="conv-bubble-body conv-bubble-text">{content}</span>
      </div>
    )
  }

  return (
    <div className={`conv-bubble conv-bubble--${role}`}>
      <span className="conv-bubble-role" aria-label={role === 'user' ? 'You' : 'Assistant'}>
        {role === 'user' ? 'You' : 'AI'}
      </span>
      <div className="conv-bubble-body">
        {isEmpty ? (
          isThinking ? (
            <ThinkingDots />
          ) : (
            <span className="conv-cursor" aria-hidden="true">▌</span>
          )
        ) : (
          <span className="conv-bubble-text">
            {content}
            {isStreaming && !isEmpty && (
              <span className="conv-cursor" aria-hidden="true">▌</span>
            )}
          </span>
        )}
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// ToolActivityBar
// ---------------------------------------------------------------------------

interface ToolActivityBarProps {
  isThinking: boolean
  isStreaming: boolean
}

function ToolActivityBar({ isThinking, isStreaming }: ToolActivityBarProps) {
  if (!isThinking && !isStreaming) return null
  return (
    <div className="conv-tool-bar" role="status" aria-live="polite">
      <span className="conv-tool-bar-dot" aria-hidden="true" />
      <span className="conv-tool-bar-label">
        {isThinking ? 'Working…' : 'Streaming response…'}
      </span>
    </div>
  )
}

// ---------------------------------------------------------------------------
// AtDropdown — shown when user types @ in the input
// ---------------------------------------------------------------------------

interface AtDropdownProps {
  query: string
  activeIndex: number
  onSelect: (id: string) => void
}

function AtDropdown({ query, activeIndex, onSelect }: AtDropdownProps) {
  const items = AT_EXTENSIONS.filter(e => e.id.startsWith(query))
  if (items.length === 0) return null

  return (
    <ul className="conv-at-dropdown" role="listbox" aria-label="@ extensions">
      {items.map((item, i) => (
        <li
          key={item.id}
          role="option"
          aria-selected={i === activeIndex}
          className={`conv-at-item${i === activeIndex ? ' conv-at-item--active' : ''}`}
          // onMouseDown prevents textarea blur before selection fires
          onMouseDown={e => { e.preventDefault(); onSelect(item.id) }}
        >
          <span className="conv-at-label">@{item.id}</span>
          <span className="conv-at-hint">{item.hint}</span>
        </li>
      ))}
    </ul>
  )
}

// ---------------------------------------------------------------------------
// ConversationPanel
// ---------------------------------------------------------------------------

interface DisplayMessage {
  role: 'user' | 'assistant' | 'system'
  content: string
  isStreaming?: boolean
  isThinking?: boolean
}

export function ConversationPanel() {
  const { activeModuleId, previousModuleId } = useDashboardState()

  const [conversations, setConversations] = useState<ConversationSummary[]>([])
  const [selectedId, setSelectedId] = useState<string | null>(() =>
    localStorage.getItem(SELECTED_ID_KEY),
  )
  const [displayMessages, setDisplayMessages] = useState<DisplayMessage[]>([])
  const [input, setInput] = useState('')
  const [isStreaming, setIsStreaming] = useState(false)
  const [isThinking, setIsThinking] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)

  // @ mention state
  const [atQuery, setAtQuery] = useState<string | null>(null)
  const [atStartIndex, setAtStartIndex] = useState(0)
  const [atDropdownIndex, setAtDropdownIndex] = useState(0)

  const convIdRef = useRef<string | null>(selectedId)
  const messagesEndRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLTextAreaElement>(null)

  // Load conversation list
  const loadConversations = useCallback(async () => {
    try {
      const res = await fetch(`${BASE}/chat`)
      if (!res.ok) return
      const data = await res.json() as ConversationSummary[]
      setConversations(data)
    } catch {
      // daemon offline — ignore
    }
  }, [])

  useEffect(() => {
    void loadConversations()
  }, [loadConversations])

  // Load conversation detail when selectedId changes
  useEffect(() => {
    if (!selectedId) {
      setDisplayMessages([])
      return
    }
    const fetchId = selectedId
    convIdRef.current = selectedId
    setLoadError(null)
    fetch(`${BASE}/chat/${selectedId}`)
      .then(r => r.ok ? r.json() : Promise.reject(r.status))
      .then((data: ConversationDetail) => {
        // Ignore stale response if user switched conversations during fetch
        if (convIdRef.current !== fetchId) return
        const msgs: DisplayMessage[] = data.messages.map((m: BackendMessage) => ({
          role: m.role,
          content: m.content,
        }))
        setDisplayMessages(msgs)
      })
      .catch(() => {
        if (convIdRef.current !== fetchId) return
        setLoadError('Failed to load conversation.')
        setDisplayMessages([])
      })
  }, [selectedId])

  // Scroll to bottom when messages change
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [displayMessages.length])

  const selectConversation = useCallback((id: string) => {
    setSelectedId(id)
    localStorage.setItem(SELECTED_ID_KEY, id)
    convIdRef.current = id
    setIsStreaming(false)
    setIsThinking(false)
  }, [])

  const newConversation = useCallback(() => {
    setSelectedId(null)
    localStorage.removeItem(SELECTED_ID_KEY)
    convIdRef.current = null
    setDisplayMessages([])
    setInput('')
    setIsStreaming(false)
    setIsThinking(false)
    setTimeout(() => inputRef.current?.focus(), 0)
  }, [])

  const deleteConversation = useCallback(async (id: string, e: React.MouseEvent) => {
    e.stopPropagation()
    await fetch(`${BASE}/chat/${id}`, { method: 'DELETE' })
    if (selectedId === id) {
      newConversation()
    }
    setConversations(prev => prev.filter(c => c.id !== id))
  }, [selectedId, newConversation])

  // ---------------------------------------------------------------------------
  // @ mention: inject fetched extension data into the input
  // ---------------------------------------------------------------------------

  const handleAtSelect = useCallback(async (extId: string) => {
    setAtQuery(null)
    setAtDropdownIndex(0)

    const mod = getModule(extId)
    if (!mod?.extension) return

    const items = await mod.extension.fetchItems()
    const label = mod.extension.label
    const block = items.length === 0
      ? `[No ${label} available] `
      : `[${label}]\n${items.map(i => `- ${i.title}${i.subtitle ? ` (${i.subtitle})` : ''}`).join('\n')}\n`

    setInput(prev => {
      const before = prev.slice(0, atStartIndex)
      const afterAt = prev.slice(atStartIndex + 1 + (atQuery?.length ?? 0))
      return before + block + afterAt
    })

    inputRef.current?.focus()
  }, [atStartIndex, atQuery])

  // ---------------------------------------------------------------------------
  // Input change: detect @ mention at cursor
  // ---------------------------------------------------------------------------

  const handleInputChange = useCallback((e: React.ChangeEvent<HTMLTextAreaElement>) => {
    const val = e.target.value
    setInput(val)

    const cursor = e.target.selectionStart ?? val.length
    const beforeCursor = val.slice(0, cursor)
    const match = beforeCursor.match(/@(\w*)$/)
    if (match && match.index !== undefined) {
      setAtQuery(match[1].toLowerCase())
      setAtStartIndex(match.index)
      setAtDropdownIndex(0)
    } else {
      setAtQuery(null)
    }
  }, [])

  // ---------------------------------------------------------------------------
  // Submit: action interceptor + Claude chat
  // ---------------------------------------------------------------------------

  const handleSubmit = useCallback(async () => {
    const msg = input.trim()
    if (!msg || isStreaming) return
    setInput('')
    setAtQuery(null)

    // Build dashboard context string for Claude
    const ctxParts: string[] = []
    if (activeModuleId) ctxParts.push(`Current module: ${activeModuleId}`)
    if (previousModuleId && previousModuleId !== activeModuleId) {
      ctxParts.push(`Previously viewing: ${previousModuleId}`)
    }
    const dashboardCtx = ctxParts.join('\n')

    // --- Action interceptor: "run <task>" ---
    const runMatch = RUN_PATTERN.exec(msg)
    if (runMatch) {
      const task = runMatch[1].trim()
      setDisplayMessages(prev => [...prev, { role: 'user', content: msg }])
      try {
        const data: RunMissionResult | null = await runMission(task)
        if (!data) throw new Error('null response')
        setDisplayMessages(prev => [
          ...prev,
          { role: 'system', content: `Mission queued: "${data.task}"` },
        ])
      } catch {
        setDisplayMessages(prev => [
          ...prev,
          { role: 'system', content: 'Failed to queue mission. Is the daemon running?' },
        ])
      }
      return
    }

    // --- Normal Claude chat ---
    const userMsg: DisplayMessage = { role: 'user', content: msg }
    const assistantMsg: DisplayMessage = { role: 'assistant', content: '', isStreaming: true, isThinking: true }

    setDisplayMessages(prev => [...prev, userMsg, assistantMsg])
    setIsStreaming(true)
    setIsThinking(true)

    let convId: string
    try {
      const res = await fetch(`${BASE}/chat`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          message: msg,
          conversation_id: convIdRef.current ?? undefined,
          dashboard_context: dashboardCtx || undefined,
        }),
      })
      if (!res.ok) throw new Error(`${res.status}`)
      const body = await res.json() as { conversation_id: string }
      convId = body.conversation_id
      convIdRef.current = convId
      if (!selectedId) {
        setSelectedId(convId)
        localStorage.setItem(SELECTED_ID_KEY, convId)
      }
    } catch {
      setDisplayMessages(prev => {
        const next = [...prev]
        const last = next[next.length - 1]
        if (last?.role === 'assistant') {
          next[next.length - 1] = { ...last, content: 'Failed to reach daemon.', isStreaming: false, isThinking: false }
        }
        return next
      })
      setIsStreaming(false)
      setIsThinking(false)
      return
    }

    const streamConvId = convId
    const es = new EventSource(`${BASE}/chat/${convId}/stream`)

    es.addEventListener('token', (e: MessageEvent<string>) => {
      // Ignore tokens from a different conversation (stale SSE connection)
      if (convIdRef.current !== streamConvId) { es.close(); return }
      const { text: chunk } = JSON.parse(e.data) as { text: string }
      setIsThinking(false)
      setDisplayMessages(prev => {
        const next = [...prev]
        const last = next[next.length - 1]
        if (last?.role === 'assistant') {
          next[next.length - 1] = { ...last, content: last.content + chunk, isThinking: false }
        }
        return next
      })
    })

    es.addEventListener('done', () => {
      setIsStreaming(false)
      setIsThinking(false)
      setDisplayMessages(prev => {
        const next = [...prev]
        const last = next[next.length - 1]
        if (last?.role === 'assistant') {
          next[next.length - 1] = { ...last, isStreaming: false, isThinking: false }
        }
        return next
      })
      es.close()
      void loadConversations()
    })

    es.addEventListener('error', (e: MessageEvent<string>) => {
      const errMsg = e.data
        ? (JSON.parse(e.data) as { message: string }).message
        : 'Unknown error'
      setDisplayMessages(prev => {
        const next = [...prev]
        const last = next[next.length - 1]
        if (last?.role === 'assistant') {
          next[next.length - 1] = { ...last, content: errMsg, isStreaming: false, isThinking: false }
        }
        return next
      })
      setIsStreaming(false)
      setIsThinking(false)
      es.close()
      void loadConversations()
    })

    es.onerror = () => {
      setDisplayMessages(prev => {
        const next = [...prev]
        const last = next[next.length - 1]
        if (last?.role === 'assistant') {
          next[next.length - 1] = { ...last, content: 'Stream disconnected.', isStreaming: false, isThinking: false }
        }
        return next
      })
      setIsStreaming(false)
      setIsThinking(false)
      es.close()
    }
  }, [input, isStreaming, selectedId, loadConversations, activeModuleId, previousModuleId])

  const handleInputKeyDown = useCallback((e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // @ dropdown keyboard navigation
    if (atQuery !== null) {
      const items = AT_EXTENSIONS.filter(ext => ext.id.startsWith(atQuery))
      if (e.key === 'ArrowDown') {
        e.preventDefault()
        setAtDropdownIndex(i => Math.min(i + 1, items.length - 1))
        return
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault()
        setAtDropdownIndex(i => Math.max(i - 1, 0))
        return
      }
      if (e.key === 'Enter' && items.length > 0) {
        e.preventDefault()
        const selected = items[atDropdownIndex] ?? items[0]
        void handleAtSelect(selected.id)
        return
      }
      if (e.key === 'Escape') {
        setAtQuery(null)
        return
      }
    }

    if (e.key === 'Enter' && !e.shiftKey && !e.metaKey) {
      e.preventDefault()
      void handleSubmit()
    }
  }, [atQuery, atDropdownIndex, handleAtSelect, handleSubmit])

  // ---------------------------------------------------------------------------
  // Render
  // ---------------------------------------------------------------------------

  const hasMessages = displayMessages.length > 0

  return (
    <div className="conv-panel">
      {/* Sidebar */}
      <aside className="conv-sidebar" aria-label="Conversations">
        <div className="conv-sidebar-header">
          <span className="conv-sidebar-title">Conversations</span>
          <button
            type="button"
            className="conv-new-btn"
            onClick={newConversation}
            aria-label="New conversation"
            title="New conversation"
          >
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="M12 5v14M5 12h14" />
            </svg>
          </button>
        </div>
        <ul className="conv-list" role="listbox" aria-label="Conversation history">
          {conversations.length === 0 && (
            <li className="conv-list-empty">No conversations yet.</li>
          )}
          {conversations.map(conv => (
            <li
              key={conv.id}
              role="option"
              aria-selected={conv.id === selectedId}
              className={`conv-list-item${conv.id === selectedId ? ' conv-list-item--active' : ''}`}
              onClick={() => selectConversation(conv.id)}
            >
              <div className="conv-list-item-body">
                <span className="conv-list-item-preview">
                  {conv.last_preview ? truncate(conv.last_preview, 48) : `Conversation ${conv.id.slice(-6)}`}
                </span>
                <span className="conv-list-item-meta">
                  {conv.last_message_at ? formatRelativeTime(conv.last_message_at) : ''} · {conv.message_count} msg
                </span>
              </div>
              <button
                type="button"
                className="conv-list-item-delete"
                onClick={(e) => void deleteConversation(conv.id, e)}
                aria-label="Delete conversation"
                title="Delete"
              >
                <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M18 6 6 18M6 6l12 12" />
                </svg>
              </button>
            </li>
          ))}
        </ul>
      </aside>

      {/* Main thread */}
      <div className="conv-main">
        {/* Messages */}
        <div
          className="conv-messages"
          aria-live="polite"
          aria-label="Conversation messages"
        >
          {!hasMessages && !loadError && (
            <div className="conv-empty">
              {selectedId ? 'Loading…' : 'Start a new conversation below.'}
            </div>
          )}
          {loadError && (
            <div className="conv-empty conv-empty--error">{loadError}</div>
          )}
          {displayMessages.map((msg, i) => {
            const isLast = i === displayMessages.length - 1
            return (
              <MessageBubble
                key={i}
                role={msg.role}
                content={msg.content}
                isStreaming={isLast && msg.isStreaming}
                isThinking={isLast && msg.isThinking}
              />
            )
          })}
          <div ref={messagesEndRef} />
        </div>

        {/* Tool activity bar */}
        <ToolActivityBar isThinking={isThinking} isStreaming={isStreaming && !isThinking} />

        {/* Input wrapper: contains @ dropdown + form */}
        <div className="conv-input-wrapper">
          {atQuery !== null && (
            <AtDropdown
              query={atQuery}
              activeIndex={atDropdownIndex}
              onSelect={(id) => void handleAtSelect(id)}
            />
          )}
          <form
            className="conv-input-area"
            onSubmit={e => { e.preventDefault(); void handleSubmit() }}
          >
            <textarea
              ref={inputRef}
              className="conv-input"
              value={input}
              onChange={handleInputChange}
              onKeyDown={handleInputKeyDown}
              placeholder={hasMessages ? 'Ask a follow-up… (type @ for context)' : 'Ask anything… (type @ for context)'}
              rows={1}
              disabled={isStreaming}
              autoComplete="off"
              spellCheck={false}
              aria-label="Message input"
            />
            <button
              type="submit"
              className="conv-send-btn"
              disabled={!input.trim() || isStreaming}
              aria-label="Send message"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M5 12h14M12 5l7 7-7 7" />
              </svg>
            </button>
          </form>
          <div className="conv-input-hint">
            <kbd className="conv-kbd">↵</kbd>send
            <span className="conv-input-hint-sep" aria-hidden="true">·</span>
            <kbd className="conv-kbd">⇧↵</kbd>newline
            <span className="conv-input-hint-sep" aria-hidden="true">·</span>
            <span className="conv-input-hint-at">@ for context</span>
          </div>
        </div>
      </div>
    </div>
  )
}
