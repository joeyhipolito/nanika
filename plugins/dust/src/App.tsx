"use client"

// Dust — Raycast-style desktop shell.
//
// Layout (820 × 520, transparent frameless alwaysOnTop):
//
//   ┌──────────────────────────────────────────────────────────────────────┐
//   │  🔍 search input                                           [⌃K]     │  52 px
//   ├─────────────────────────┬────────────────────────────────────────────┤
//   │  Results (280 px wide)  │  PluginInfo panel + ComponentRenderer       │  440 px
//   ├─────────────────────────┴────────────────────────────────────────────┤
//   │  ↑↓ navigate  ↵ open  esc hide                            ⌃K actions │  28 px
//   └──────────────────────────────────────────────────────────────────────┘

import {
  forwardRef,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { ComponentRenderer } from './ComponentRenderer'
import type { ActionHandler, OpenFileHandler } from './ComponentRenderer'
import { QuickEditor } from './QuickEditor'
import { parseSlash } from './slashGrammar'
import type { SlashCommand } from './slashGrammar'
import type { Component } from './types'

// ---------------------------------------------------------------------------
// Type glyph — maps a capability match to a single-char glyph indicating
// what kind of UI the capability renders.
// ---------------------------------------------------------------------------

type WindowMode = 'default' | 'expanded' | 'collapsed'

type GlyphType = 'chat' | 'table' | 'diff' | 'tracker' | 'mission'

const GLYPH_CHAR: Record<GlyphType, string> = {
  chat: '≡',
  table: '⊟',
  diff: '±',
  tracker: '✓',
  mission: '◎',
}

function glyphFor(match: CapabilityMatch): GlyphType {
  const id = match.capability.id.toLowerCase()
  const name = match.plugin_id.toLowerCase()
  const kw = match.capability.keywords.map(k => k.toLowerCase())
  if (name === 'tracker' || id.includes('tracker') || kw.includes('tracker')) return 'tracker'
  if (name === 'mission' || id.includes('mission') || kw.includes('mission')) return 'mission'
  if (id.includes('diff') || kw.includes('diff')) return 'diff'
  if (id === 'widget' || kw.includes('table') || kw.includes('widget')) return 'table'
  return 'chat'
}

// ---------------------------------------------------------------------------
// IPC wire types (mirror the Rust structs serialised by Tauri)
// ---------------------------------------------------------------------------

type Capability = {
  id: string
  name: string
  description: string
  keywords: string[]
}

type CapabilityMatch = {
  plugin_id: string
  plugin_name: string
  capability: Capability
  score: number
}

type PluginManifest = {
  id: string
  name: string
  version: string
  description: string
  binary?: string
  capabilities: Capability[]
}

type PluginInfo = {
  manifest: PluginManifest
  healthy: boolean
}

// ---------------------------------------------------------------------------
// DisplayResult — either a real capability match or the synthetic Ask Claude
// fallback that surfaces when search returns nothing for a non-empty query.
// ---------------------------------------------------------------------------

type DisplayResult =
  | { kind: 'match'; match: CapabilityMatch }
  | { kind: 'ask_claude'; query: string }

type DispatchStatus = 'idle' | 'in_flight'
type ThreadsStatus = 'idle' | 'loading' | 'ready' | 'error'

const TERRACOTTA = '#DA7757'

// Debug instrumentation gate — toggle without rebuild via either:
//   ?debug=chat URL param, or `window.__DUST_DEBUG__ = true` in devtools.
// Default off; zero overhead at runtime besides one boolean check.
const DEBUG_CHAT: boolean =
  typeof window !== 'undefined' &&
  (window.location?.search?.includes('debug=chat') ||
    Boolean((window as unknown as { __DUST_DEBUG__?: boolean }).__DUST_DEBUG__))

// ---------------------------------------------------------------------------
// Thread-rail types (mirror chat plugin's Rust Thread/StoredMessage structs)
// ---------------------------------------------------------------------------

type ThreadMeta = {
  id: string
  title: string
  created_at: number
  updated_at: number
}

type StoredMessage = {
  id: string
  thread_id: string
  role: string
  content: string
  created_at: number
}

// dispatch_action now forwards the full ActionResult envelope from the chat
// plugin ({ success, message?, data? }). Older call sites ignore the return
// value; this shim lets data-bearing actions read `.data` safely.
type ActionResultEnvelope<T> = { success?: boolean; message?: string; data?: T }

function unwrapData<T>(res: unknown): T | null {
  if (res == null) return null
  if (typeof res === 'object' && res !== null && 'success' in (res as object)) {
    const env = res as ActionResultEnvelope<T>
    return env.data ?? null
  }
  return res as T
}

// Pure-JS relative-time formatter — avoids Intl.RelativeTimeFormat (bundle).
function agoLabel(updatedMs: number, now: number = Date.now()): string {
  const s = Math.max(0, Math.floor((now - updatedMs) / 1000))
  if (s < 60) return 'just now'
  if (s < 3600) return `${Math.floor(s / 60)}m ago`
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`
  if (s < 2 * 86400) return 'yesterday'
  if (s < 7 * 86400) return `${Math.floor(s / 86400)}d ago`
  return new Date(updatedMs).toLocaleDateString('en-US', { month: 'short', day: 'numeric' })
}

// ---------------------------------------------------------------------------
// App — root shell
// ---------------------------------------------------------------------------

export function App() {
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<CapabilityMatch[]>([])
  const [selectedIndex, setSelectedIndex] = useState(0)
  // Thread continuity: updated when a DataUpdated event returns a thread_id.
  const [activeThreadId, setActiveThreadId] = useState<string | null>(null)
  // Chat messages: replaced wholesale by each data_updated event from the chat plugin.
  const [chatMessages, setChatMessages] = useState<Component[]>([])
  const [pluginInfo, setPluginInfo] = useState<PluginInfo | null>(null)
  const [detail, setDetail] = useState<Component[] | null>(null)
  const [showActionPalette, setShowActionPalette] = useState(false)
  const [isVisible, setIsVisible] = useState(false)
  const [windowMode, setWindowMode] = useState<WindowMode>('default')
  const [editingFile, setEditingFile] = useState<{ path: string; basename: string; line?: number } | null>(null)
  // Slash-command error surface — non-null when the last Enter parsed as a
  // slash command but no registered capability matched the prefix. Also reused
  // as the chat-surface error banner (fetchThreads / loadThreadMessages /
  // handleNewThread failures). Cleared on every keystroke and successful dispatch.
  const [slashError, setSlashError] = useState<string | null>(null)
  // Thread rail state — rail is hidden by default, shown via ⌘T when chat active.
  const [threads, setThreads] = useState<ThreadMeta[]>([])
  const [threadsVisible, setThreadsVisible] = useState(false)
  const [threadCursor, setThreadCursor] = useState(0)
  const [threadsStatus, setThreadsStatus] = useState<ThreadsStatus>('idle')
  // Two-state placeholder driver for ChatPane. 'in_flight' from the moment a
  // chat dispatch leaves the React side until the first data_updated / error
  // event lands; resets to 'idle' on thread switch / new thread / unmount.
  const [dispatchStatus, setDispatchStatus] = useState<DispatchStatus>('idle')
  const inputRef = useRef<HTMLInputElement>(null)
  // Cancels stale render_ui loads when Enter is pressed rapidly between plugins
  const detailLoadId = useRef(0)
  const hideTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  // Ref mirrors windowMode so event listeners don't capture stale closures
  const windowModeRef = useRef<WindowMode>('default')
  // Trailing-edge debounce for list_threads refresh on unknown thread_id events.
  const refreshTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  // Mirror of `threads` so the (deps-stable) chat-event listener can compare.
  const threadsRef = useRef<ThreadMeta[]>([])
  // Mirror of `isAskClaude` for the global ⌘T keymap effect (deps are [transitionTo]).
  const isAskClaudeRef = useRef(false)
  // Mirror of the most recent chat_subscribe target. Read inside the
  // (deps-stable) chat-event listener to drop deltas from a thread we no
  // longer subscribe to (rapid ⌘T thread-switch race). All writes to
  // `activeThreadId` go through `setActiveThread` so this stays in sync.
  const subscribedThreadRef = useRef<string | null>(null)
  // Tracks the in-flight optimistic user-turn echo. Set when dispatchChat
  // pushes the provisional component; cleared on first data_updated (success)
  // or on catch (failure). Also cleared by exitChat. Used by the rollback
  // filter and by the listener success-path cleanup.
  const provisionalUserIdRef = useRef<string | null>(null)

  const setActiveThread = useCallback((id: string | null) => {
    subscribedThreadRef.current = id
    setActiveThreadId(id)
  }, [])

  // Pure derivations from `query` — kept above displayResults so the memo
  // can name them in deps without re-parsing.
  const slash = useMemo(() => parseSlash(query), [query])
  // Sticky chat predicate: once a thread is live (messages or assigned id),
  // plain-text Enter routes to chat instead of search-result selection.
  const isChatActive = chatMessages.length > 0 || activeThreadId !== null

  // displayResults routing — see DESIGN-CHAT-UX.md §(a) case table.
  // 1. Slash query never falls through to capability search.
  // 2. Sticky mode (chat active) keeps ChatPane mounted regardless of typed text.
  // 3. Otherwise existing behaviour: results list, or Ask-Claude fallback when
  //    the query is non-empty and search returned nothing.
  const displayResults: DisplayResult[] = useMemo(() => {
    if (slash) return [{ kind: 'ask_claude' as const, query }]
    if (isChatActive) return [{ kind: 'ask_claude' as const, query }]
    if (results.length > 0 || query.trim() === '') {
      return results.map(m => ({ kind: 'match' as const, match: m }))
    }
    return [{ kind: 'ask_claude' as const, query }]
  }, [slash, isChatActive, results, query])

  // ------------------------------------------------------------------
  // Search — debounced 10 ms; resets selection but never clears detail.
  // Slash queries skip the IPC entirely: fuzzy-matching the literal "/ask tr"
  // is wasted work and lets a slow result mutation flicker the UI between
  // ChatPane and DetailPane (see DESIGN-CHAT-UX.md §a notes).
  // ------------------------------------------------------------------
  useEffect(() => {
    if (slash) {
      setResults([])
      setSelectedIndex(0)
      return
    }
    const tKeystroke = performance.now()
    const t = setTimeout(() => {
      invoke<CapabilityMatch[]>('search_capabilities', { query })
        .then(r => {
          setResults(r)
          setSelectedIndex(0)
          // [bench] keystroke→results: delta from onChange to first setResults commit
          console.debug(`[bench] results-updated keystroke=${tKeystroke.toFixed(3)} now=${performance.now().toFixed(3)}`)
        })
        .catch(console.error)
    }, 10)
    return () => clearTimeout(t)
  }, [query, slash])

  const activeResult = displayResults[selectedIndex]
  const isAskClaude = activeResult?.kind === 'ask_claude'

  // Mirror `isAskClaude` into a ref so the global ⌘T handler (mounted with
  // [transitionTo] deps) can read the current value without resubscribing
  // every time the user changes search selection.
  useEffect(() => {
    isAskClaudeRef.current = isAskClaude
    // Closing the chat pane (user types a new query) should also drop the
    // rail — otherwise it would silently re-appear on the next Ask Claude.
    if (!isAskClaude) setThreadsVisible(false)
  }, [isAskClaude])

  // Align the rail cursor with the active thread whenever the rail opens
  // (so `⌘T` lands on what's loaded, not on row 0).
  useEffect(() => {
    if (!threadsVisible || threads.length === 0) return
    const idx = activeThreadId
      ? threads.findIndex(t => t.id === activeThreadId)
      : -1
    setThreadCursor(idx >= 0 ? idx : 0)
  }, [threadsVisible, threads, activeThreadId])

  // ------------------------------------------------------------------
  // Slash-command dispatch — runs before the result-list path. A non-null
  // parseSlash result commits us to one of three branches: dispatch /ask to
  // the chat plugin, dispatch any other matching command to its owning
  // plugin, or surface a terracotta "unknown command" error. A null parse
  // falls through to the existing result-selection behaviour.
  // ------------------------------------------------------------------
  const dispatchChat = useCallback(
    (text: string) => {
      const tid = activeThreadId
      // Optimistic user-turn echo — matches plugin.rs::user_turn_component
      // bit-for-bit so the server's wholesale-replace payload supplants it
      // without a DOM flicker. provisional_id is UI-only (rollback bookkeeping).
      const pid = `prov_${Math.random().toString(36).slice(2, 8)}`
      provisionalUserIdRef.current = pid
      setChatMessages(prev => [
        ...prev,
        {
          type: 'agent_turn',
          role: 'user',
          content: text,
          streaming: false,
          timestamp: Date.now(),
          provisional_id: pid,
        } as Component & { provisional_id: string },
      ])
      // Flip placeholder copy to "Waiting for response…" immediately; the
      // first data_updated event will flip it back to idle.
      setDispatchStatus('in_flight')
      // Clear input synchronously — we don't wait for the plugin ack since
      // it carries no semantic payload and blocking would hurt typing flow.
      setQuery('')
      // Subscribe before dispatching so we never miss the first delta.
      invoke('chat_subscribe', { threadId: tid })
        .then(() =>
          invoke('dispatch_action', {
            pluginId: 'chat',
            capabilityId: 'ask',
            actionId: 'ask',
            params: { text, thread_id: tid },
          }),
        )
        .catch((err: unknown) => {
          setDispatchStatus('idle')
          const msg = err instanceof Error ? err.message : String(err)
          // Roll back the provisional echo — filter by id so any unrelated
          // turns that slipped in survive. Surface the failure via the
          // slashError banner (not chatMessages, which is server state).
          setChatMessages(prev =>
            prev.filter(c =>
              !(c && typeof c === 'object' && 'provisional_id' in c &&
                (c as { provisional_id?: string }).provisional_id === pid),
            ),
          )
          provisionalUserIdRef.current = null
          setSlashError(`Failed to dispatch /ask: ${msg.slice(0, 140)}`)
        })
    },
    [activeThreadId],
  )

  const dispatchSlash = useCallback(
    (cmd: SlashCommand): boolean => {
      // Resolve the prefix directly against the full registry. Using the
      // fuzzy-filtered `results` slice fails because the search was run
      // against the raw slash query (e.g. "/tracker create foo"), which
      // doesn't fuzzy-match any plugin corpus — the leading `/` + args
      // kill the score. Re-query with just the prefix to find the plugin
      // whose Command.prefix == cmd.prefix.
      ;(async () => {
        const matches = await invoke<CapabilityMatch[]>('search_capabilities', {
          query: cmd.prefix,
        })
        const match = matches.find(r => r.capability.keywords.includes(cmd.prefix))
        if (!match) {
          setSlashError(`Unknown command: /${cmd.prefix}`)
          return
        }
        setSlashError(null)

        // /ask <text> — chat plugin streaming dispatch. Body is the post-prefix
        // args verbatim; "/ask hello" sends text="hello", not "/ask hello".
        if (cmd.prefix === 'ask') {
          dispatchChat(cmd.args)
          return
        }

        // Non-chat slash — clear the input after an accepted dispatch (chat
        // branch already clears via dispatchChat).
        setQuery('')

        // Generic command dispatch: first whitespace-delimited token is the
        // op/action, the rest is a free-form tail passed as `title` — matches
        // the tracker `create <title>` convention.
        const trimmed = cmd.args.trimStart()
        const spaceIdx = trimmed.indexOf(' ')
        const op = spaceIdx === -1 ? trimmed : trimmed.slice(0, spaceIdx)
        const tail = spaceIdx === -1 ? '' : trimmed.slice(spaceIdx + 1)

        try {
          await invoke('dispatch_action', {
            pluginId: match.plugin_id,
            capabilityId: match.capability.id,
            actionId: op,
            params: tail ? { title: tail } : {},
          })
        } catch (err) {
          const msg = err instanceof Error ? err.message : String(err)
          setSlashError(`Failed to dispatch /${cmd.prefix}: ${msg}`)
        }
      })()
      return true
    },
    [dispatchChat],
  )

  // ------------------------------------------------------------------
  // Enter handler — calls render_ui and swaps detail atomically.
  // Old content stays visible while new load is in flight (no flash).
  // ------------------------------------------------------------------
  const handleEnter = useCallback(() => {
    const callId = DEBUG_CHAT ? Math.random().toString(36).slice(2, 8) : ''
    if (DEBUG_CHAT) {
      console.debug('[dust] handleEnter START', {
        callId,
        query,
        isChatActive,
        slash: parseSlash(query)?.prefix ?? null,
        t: performance.now(),
      })
    }

    // Slash-command first: if the query parses as a command, route it and
    // short-circuit the result-list behaviour.
    const cmd = parseSlash(query)
    if (cmd) {
      if (DEBUG_CHAT) console.debug('[dust] handleEnter DISPATCH', { callId, path: 'slash' })
      dispatchSlash(cmd)
      return
    }
    setSlashError(null)

    // Sticky chat dispatch — non-slash text Enter inside an active thread
    // routes to chat. Empty query intentionally falls through (Case 2 in
    // DESIGN-CHAT-UX.md §a — let the user clear input to scroll history
    // without dispatching nothing).
    if (isChatActive && query.trim() !== '') {
      if (DEBUG_CHAT) console.debug('[dust] handleEnter DISPATCH', { callId, path: 'sticky' })
      dispatchChat(query)
      return
    }

    const selected = displayResults[selectedIndex]
    if (!selected) return

    if (selected.kind === 'ask_claude') {
      // Don't dispatch on an empty query — that's the sticky-mode "user
      // clearing input to scroll" affordance from §a Case 2.
      if (selected.query.trim() === '') return
      if (DEBUG_CHAT) console.debug('[dust] handleEnter DISPATCH', { callId, path: 'ask' })
      dispatchChat(selected.query)
      return
    }

    if (DEBUG_CHAT) console.debug('[dust] handleEnter DISPATCH', { callId, path: 'match' })
    const match = selected.match
    const loadId = ++detailLoadId.current

    Promise.all([
      invoke<PluginInfo>('get_plugin_info', { pluginId: match.plugin_id }),
      invoke<Component[]>('render_ui', {
        pluginId: match.plugin_id,
        capabilityId: match.capability.id,
        query,
      }).catch((): Component[] => []),
    ])
      .then(([info, components]) => {
        if (loadId !== detailLoadId.current) return
        setPluginInfo(info)
        setDetail(components.length > 0 ? components : null)
        // [bench] enter→detail: measured from enter-pressed log paired above
        console.debug(`[bench] detail-rendered ${performance.now().toFixed(3)}`)
      })
      .catch(console.error)
  }, [displayResults, selectedIndex, query, isChatActive, dispatchSlash, dispatchChat])

  // ------------------------------------------------------------------
  // Hide with animation — 120ms fade-out before OS hide
  // ------------------------------------------------------------------
  const hideWindow = useCallback(() => {
    if (hideTimerRef.current !== null) return
    setIsVisible(false)
    hideTimerRef.current = setTimeout(() => {
      getCurrentWindow().hide().catch(console.error)
      hideTimerRef.current = null
    }, 120)
  }, [])

  // ------------------------------------------------------------------
  // Window mode transitions — resize + reposition via Rust command
  // ------------------------------------------------------------------
  const transitionTo = useCallback((mode: WindowMode) => {
    windowModeRef.current = mode
    setWindowMode(mode)
    invoke('set_window_mode', { mode }).catch(console.error)
  }, [])

  // ------------------------------------------------------------------
  // Hide on blur, animate in on show, listen for Rust hide-request
  // ------------------------------------------------------------------
  useEffect(() => {
    const win = getCurrentWindow()
    const cleanups: (() => void)[] = []

    win
      .onFocusChanged(({ payload: focused }) => {
        if (!focused && windowModeRef.current !== 'collapsed') {
          hideWindow()
          invoke('chat_unsubscribe').catch(console.error)
        }
      })
      .then(f => cleanups.push(f))

    win
      .listen('tauri://focus', () => {
        // [bench] hotkey→pane: wall-clock ms when webview receives focus
        console.debug(`[bench] window-focus-received ${Date.now()}`)
        if (hideTimerRef.current !== null) {
          clearTimeout(hideTimerRef.current)
          hideTimerRef.current = null
        }
        setIsVisible(true)
        requestAnimationFrame(() => {
          // [bench] hotkey→pane: wall-clock ms at first RAF after visibility set
          console.debug(`[bench] first-paint-raf ${Date.now()}`)
          inputRef.current?.focus()
          inputRef.current?.select()
        })
      })
      .then(f => cleanups.push(f))

    win
      .listen('dust://hide-request', () => {
        if (windowModeRef.current !== 'collapsed') hideWindow()
      })
      .then(f => cleanups.push(f))

    return () => cleanups.forEach(f => f())
  }, [hideWindow])

  // ------------------------------------------------------------------
  // Thread-rail helpers — shared by ⌘T, rail row click, Enter-in-rail, and
  // the rail's `+` button. Keeping `threads` and `threadsRef` in lockstep is
  // load-bearing: the chat-event listener's "unknown thread_id" check relies
  // on `threadsRef.current` to decide whether to schedule a refresh.
  // ------------------------------------------------------------------
  const applyThreads = useCallback((list: ThreadMeta[]) => {
    setThreads(list)
    threadsRef.current = list
  }, [])

  const fetchThreads = useCallback(async (): Promise<ThreadMeta[]> => {
    const res = await invoke<unknown>('dispatch_action', {
      pluginId: 'chat',
      capabilityId: 'ask',
      actionId: 'list_threads',
      params: {},
    })
    const data = unwrapData<unknown>(res)
    if (data == null) return []
    if (!Array.isArray(data)) {
      throw new Error('malformed response')
    }
    return data as ThreadMeta[]
  }, [])

  // Safe wrapper used by every fetchThreads call site. Surfaces failures via
  // the slashError banner and drives `threadsStatus` so the rail can render a
  // terminal error / empty state instead of a perpetual "Loading…".
  const fetchThreadsSafe = useCallback(async () => {
    setThreadsStatus('loading')
    try {
      const list = await fetchThreads()
      applyThreads(list)
      setThreadsStatus('ready')
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err)
      setSlashError(`Couldn't load threads: ${msg.slice(0, 140)}`)
      setThreadsStatus('error')
    }
  }, [fetchThreads, applyThreads])

  const loadThreadMessages = useCallback(async (threadId: string): Promise<void> => {
    // Ordering: clear messages first (no flash of prior), flip active-thread
    // for instant selection highlight, then re-subscribe *before* dispatching
    // the read — matches the handleEnter invariant ("never miss the first delta").
    setChatMessages([])
    setActiveThread(threadId)
    setDispatchStatus('idle')
    try {
      await invoke('chat_subscribe', { threadId })
      const res = await invoke<unknown>('dispatch_action', {
        pluginId: 'chat',
        capabilityId: 'ask',
        actionId: 'list_messages',
        params: { thread_id: threadId },
      })
      const data = unwrapData<unknown>(res)
      const msgs = data == null ? [] : data
      if (!Array.isArray(msgs)) {
        throw new Error('malformed response')
      }
      setChatMessages(
        (msgs as StoredMessage[]).map(m => ({
          type: 'agent_turn',
          role: m.role,
          content: m.content,
          streaming: false,
          timestamp: m.created_at,
        }) as Component),
      )
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err)
      setSlashError(`Couldn't open thread: ${msg.slice(0, 140)}`)
    }
  }, [setActiveThread])

  // Step-back one level from chat to the launcher search surface. Leaves the
  // rust-side chat_subscribe task running — the existing stale-thread guard
  // drops any late events whose thread_id !== subscribedThreadRef.current.
  // Persisted messages remain in the chat plugin's store; re-entering via
  // ⌘T replays the full history from disk.
  const exitChat = useCallback(() => {
    setChatMessages([])
    setActiveThread(null)
    setThreadsVisible(false)
    setThreadCursor(0)
    setDispatchStatus('idle')
    provisionalUserIdRef.current = null
    setQuery('')
    setSlashError(null)
    setSelectedIndex(0)
  }, [setActiveThread])

  const handleNewThread = useCallback(() => {
    setChatMessages([])
    setActiveThread(null)
    setDispatchStatus('idle')
    // Subscribe before dispatching — invariant shared with handleEnter.
    invoke('chat_subscribe', { threadId: null })
      .then(() =>
        invoke('dispatch_action', {
          pluginId: 'chat',
          capabilityId: 'ask',
          actionId: 'new_thread',
          params: {},
        }),
      )
      .catch((err: unknown) => {
        const msg = err instanceof Error ? err.message : String(err)
        setSlashError(`Couldn't start a new thread: ${msg.slice(0, 140)}`)
      })
  }, [setActiveThread])

  // ------------------------------------------------------------------
  // Chat event listener — single effect, no deps.  Handles data_updated
  // (replaces chatMessages wholesale) and error (closes last streaming turn
  // + appends terracotta error text).  Subscription lifecycle is managed
  // explicitly by handleEnter / ⌘N / blur — not by this effect.
  //
  // Also schedules a trailing-edge 500 ms `list_threads` refresh when an
  // unknown thread_id shows up in a data_updated event — covers the case
  // where ⌘N's server-assigned thread_id hasn't yet been merged into `threads`.
  // ------------------------------------------------------------------
  useEffect(() => {
    const win = getCurrentWindow()
    let unlisten: (() => void) | null = null
    win
      .listen<{ thread_id: string | null; event_type: string; data: unknown }>(
        'dust://chat-event',
        ({ payload }) => {
          // Stale-thread guard — drop deltas from a thread we no longer
          // subscribe to. Without this, a payload from thread A in flight
          // during a ⌘T → thread B switch would stomp B's chatMessages.
          // A null incoming thread_id is accepted only when we are also in
          // the null-thread state (first delta on a fresh new_thread).
          const subscribed = subscribedThreadRef.current
          if (
            payload.thread_id !== null &&
            subscribed !== null &&
            payload.thread_id !== subscribed
          ) {
            if (DEBUG_CHAT) {
              console.debug('[chat-event] dropped stale payload', {
                payload_thread: payload.thread_id,
                subscribed,
              })
            }
            return
          }

          // First delta carrying a thread_id pins the thread when we're in
          // the null-state (fresh new_thread waiting for server-assigned id).
          if (payload.thread_id && subscribed === null) {
            setActiveThread(payload.thread_id)
          }

          if (payload.event_type === 'data_updated') {
            setChatMessages(payload.data as Component[])
            setDispatchStatus('idle')
            // First real event evicts any provisional echo by wholesale
            // replacement; clear the id-bookkeeping so rollback is a no-op.
            if (provisionalUserIdRef.current !== null) {
              provisionalUserIdRef.current = null
            }
            const newTid = payload.thread_id
            if (newTid && !threadsRef.current.some(t => t.id === newTid)) {
              if (refreshTimerRef.current) clearTimeout(refreshTimerRef.current)
              refreshTimerRef.current = setTimeout(() => {
                fetchThreads()
                  .then(applyThreads)
                  .catch((err: unknown) => {
                    const msg = err instanceof Error ? err.message : String(err)
                    setSlashError(`Couldn't load threads: ${msg.slice(0, 140)}`)
                  })
                refreshTimerRef.current = null
              }, 500)
            }
          } else if (payload.event_type === 'error') {
            setDispatchStatus('idle')
            const errMsg =
              typeof payload.data === 'string'
                ? payload.data
                : ((payload.data as { message?: string })?.message ?? 'An error occurred')
            setChatMessages(prev => {
              const updated = prev.map((c, i) =>
                i === prev.length - 1 && c.type === 'agent_turn' && c.streaming
                  ? ({ ...c, streaming: false } as Component)
                  : c,
              )
              return [
                ...updated,
                {
                  type: 'text',
                  content: errMsg,
                  style: { color: { r: 218, g: 119, b: 87 } },
                } as Component,
              ]
            })
          }
        },
      )
      .then(f => { unlisten = f })
    return () => {
      unlisten?.()
      if (refreshTimerRef.current) {
        clearTimeout(refreshTimerRef.current)
        refreshTimerRef.current = null
      }
      invoke('chat_unsubscribe').catch(console.error)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  // ------------------------------------------------------------------
  // Auto-submit investigation instrumentation (DEBUG_CHAT only).
  // Logs three exotic event paths that could double-fire Enter:
  //   - <input type="search"> native `search` event
  //   - IME composition start/end
  //   - blur / focus loss
  // The block is a single conditional effect; when DEBUG is off, only the
  // early `return` runs.
  // ------------------------------------------------------------------
  useEffect(() => {
    if (!DEBUG_CHAT) return
    const el = inputRef.current
    if (!el) return
    const onSearch = (e: Event) =>
      console.debug('[dust] input.search', {
        value: (e.target as HTMLInputElement).value,
        t: performance.now(),
      })
    const onCompStart = (e: CompositionEvent) =>
      console.debug('[dust] compositionstart', { data: e.data })
    const onCompEnd = (e: CompositionEvent) =>
      console.debug('[dust] compositionend', { data: e.data })
    const onBlur = (e: FocusEvent) =>
      console.debug('[dust] blur', { relatedTarget: e.relatedTarget })
    el.addEventListener('search', onSearch)
    el.addEventListener('compositionstart', onCompStart)
    el.addEventListener('compositionend', onCompEnd)
    el.addEventListener('blur', onBlur)
    return () => {
      el.removeEventListener('search', onSearch)
      el.removeEventListener('compositionstart', onCompStart)
      el.removeEventListener('compositionend', onCompEnd)
      el.removeEventListener('blur', onBlur)
    }
  }, [])

  // ------------------------------------------------------------------
  // Global keyboard shortcuts — ⌘E (expand toggle), ⌘K (collapse),
  // Esc (return to default from expanded/collapsed)
  // These fire regardless of which element has focus, so CollapsedBar
  // also responds to keyboard.
  // ------------------------------------------------------------------
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey && e.key === 'n') {
        e.preventDefault()
        handleNewThread()
        return
      }
      if (e.metaKey && e.key === 't') {
        e.preventDefault()
        // No-op when chat pane isn't active — flashing an empty rail is confusing.
        if (!isAskClaudeRef.current) return
        setThreadsVisible(v => {
          const next = !v
          // Re-fetch on first open OR when the previous attempt failed —
          // otherwise the rail would stay stuck on "Couldn't load threads."
          // even after the chat plugin recovers.
          if (next && (threadsRef.current.length === 0 || threadsStatus === 'error')) {
            fetchThreadsSafe()
          }
          return next
        })
        return
      }
      if (e.metaKey && e.key === 'e') {
        e.preventDefault()
        transitionTo(windowModeRef.current === 'expanded' ? 'default' : 'expanded')
        return
      }
      // ⌘+K collapsed-bar mode is parked — kept in the state machine
      // for future re-enable, but the keyboard entry point is disabled
      // so the sizing cascade doesn't trip users during MVP.
      // if (e.metaKey && e.key === 'k') {
      //   e.preventDefault()
      //   if (windowModeRef.current !== 'collapsed') transitionTo('collapsed')
      //   return
      // }
      if (e.key === 'Escape') {
        if (windowModeRef.current === 'expanded' || windowModeRef.current === 'collapsed') {
          e.preventDefault()
          transitionTo('default')
          return
        }
        // Default-mode Esc step-back (palette → detail → exit chat → clear+hide)
        // is owned by SearchBar's handleKeyDown. Running a parallel document-level
        // branch here collapses multiple step-back levels on one press, because the
        // synthetic handler and this listener fire on the same event tick.
      }
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [transitionTo, handleNewThread, fetchThreadsSafe, threadsStatus])

  // ------------------------------------------------------------------
  // Keyboard navigation
  // ------------------------------------------------------------------
  const railActive = isAskClaude && threadsVisible
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      // IME-composition guard — never dispatch on Enter that's actually an
      // IME candidate-commit. `keyCode === 229` covers the legacy code path
      // where `isComposing` is false but the OS still reports the synthetic
      // "process" key. Cost-free defence for CJK users; see DESIGN-CHAT-UX.md §f.
      const native = e.nativeEvent as KeyboardEvent
      if (native.isComposing || native.keyCode === 229) return

      if (DEBUG_CHAT) {
        console.debug('[dust] keydown', {
          key: e.key,
          code: e.code,
          isComposing: native.isComposing,
          repeat: e.repeat,
          t: performance.now(),
        })
      }

      if (e.ctrlKey && e.key === 'k') {
        e.preventDefault()
        setShowActionPalette(v => !v)
        return
      }

      // When the rail is visible, arrows / Enter / Esc drive the rail instead
      // of the results list (ResultsList isn't even rendered in chat mode).
      if (railActive) {
        switch (e.key) {
          case 'ArrowDown':
            e.preventDefault()
            setThreadCursor(c => Math.min(c + 1, Math.max(0, threads.length - 1)))
            return
          case 'ArrowUp':
            e.preventDefault()
            setThreadCursor(c => Math.max(c - 1, 0))
            return
          case 'Enter': {
            const t = threads[threadCursor]
            if (t) {
              e.preventDefault()
              setThreadsVisible(false)
              loadThreadMessages(t.id).catch(() => {
                // loadThreadMessages already surfaces failures via slashError.
              })
            }
            return
          }
          case 'Escape':
            e.preventDefault()
            setThreadsVisible(false)
            return
        }
      }

      switch (e.key) {
        case 'ArrowDown':
          e.preventDefault()
          setShowActionPalette(false)
          setSelectedIndex(i => Math.min(i + 1, displayResults.length - 1))
          break
        case 'ArrowUp':
          e.preventDefault()
          setShowActionPalette(false)
          setSelectedIndex(i => Math.max(i - 1, 0))
          break
        case 'Enter':
          e.preventDefault()
          setShowActionPalette(false)
          console.debug(`[bench] enter-pressed ${performance.now().toFixed(3)}`)
          handleEnter()
          break
        case 'Escape':
          // Expanded/collapsed Esc is handled by the global keydown listener
          if (windowModeRef.current !== 'default') return
          e.preventDefault()
          // Step-back order: action palette → loaded detail → exit chat → clear query+hide
          if (showActionPalette) {
            setShowActionPalette(false)
          } else if (detail !== null) {
            // Close the loaded component tree, keep the search + results
            setDetail(null)
            setPluginInfo(null)
            detailLoadId.current++
          } else if (isChatActive) {
            exitChat()
          } else {
            setQuery('')
            hideWindow()
          }
          break
      }
    },
    [
      displayResults.length,
      showActionPalette,
      handleEnter,
      hideWindow,
      detail,
      railActive,
      threads,
      threadCursor,
      loadThreadMessages,
      isChatActive,
      exitChat,
    ],
  )

  // ------------------------------------------------------------------
  // File open — invoked by FileRef chip (⌘⇧E)
  // ------------------------------------------------------------------
  const handleOpenFile: OpenFileHandler = useCallback((path, basename, line) => {
    setEditingFile({ path, basename, line })
  }, [])

  const handleCloseEditor = useCallback(() => {
    setEditingFile(null)
    requestAnimationFrame(() => inputRef.current?.focus())
  }, [])

  // ------------------------------------------------------------------
  // Action dispatch — forwarded to plugin via IPC
  // ------------------------------------------------------------------
  const handleAction: ActionHandler = useCallback(
    async (action, id) => {
      const selected = displayResults[selectedIndex]
      if (!selected || selected.kind !== 'match') return
      await invoke('dispatch_action', {
        pluginId: selected.match.plugin_id,
        capabilityId: selected.match.capability.id,
        actionId: action,
        params: id ? { id } : {},
      })
    },
    [displayResults, selectedIndex],
  )

  // CollapsedBar — shown when windowMode === 'collapsed'
  if (windowMode === 'collapsed') {
    const sel = displayResults[selectedIndex]
    const activeTask = sel?.kind === 'match' ? sel.match.capability.name : sel?.kind === 'ask_claude' ? 'Ask Claude' : null
    return (
      <div className="dust-wrapper visible">
        <CollapsedBar
          activeTask={activeTask}
          onExpand={() => transitionTo('default')}
        />
      </div>
    )
  }

  return (
    <div className={`dust-wrapper${isVisible ? ' visible' : ''}`}>
      <div
        className="flex h-screen w-screen flex-col overflow-hidden rounded-xl"
        style={{
          background: 'var(--bg)',
          border: '1px solid var(--border)',
          boxShadow: '0 32px 64px rgba(0,0,0,0.85)',
        }}
      >
        {/* ── Search bar ───────────────────────────────────────────── */}
        <SearchBar
          ref={inputRef}
          value={query}
          onChange={v => {
            if (DEBUG_CHAT) {
              console.debug('[dust] onChange', {
                value: v,
                len: v.length,
                slash: parseSlash(v)?.prefix ?? null,
                t: performance.now(),
              })
            }
            setQuery(v)
            if (slashError) setSlashError(null)
          }}
          onKeyDown={handleKeyDown}
          mode={isChatActive ? 'chat' : 'search'}
        />

        {/* ── Slash-command error banner ─────────────────────────── */}
        {slashError && (
          <div
            role="alert"
            className="px-4 py-1 text-[11px] shrink-0"
            style={{
              background: 'var(--bg-elevated)',
              borderTop: '1px solid var(--border)',
              color: TERRACOTTA,
            }}
          >
            {slashError}
          </div>
        )}

        {/* ── Body ─────────────────────────────────────────────────── */}
        <div
          className={`flex flex-1 overflow-hidden${windowMode === 'expanded' ? ' layout-expanded' : ''}`}
          style={{ borderTop: '1px solid var(--border)' }}
        >
          {!isAskClaude && (
            <ResultsList
              results={displayResults}
              selectedIndex={selectedIndex}
              onSelect={setSelectedIndex}
            />
          )}
          {railActive && (
            <ThreadRail
              threads={threads}
              status={threadsStatus}
              activeThreadId={activeThreadId}
              cursor={threadCursor}
              onSelect={(i, id) => {
                setThreadCursor(i)
                setThreadsVisible(false)
                loadThreadMessages(id).catch(() => {
                  // loadThreadMessages already surfaces via slashError; this
                  // catch only exists because TS marks the promise unhandled.
                })
              }}
              onNewThread={() => {
                handleNewThread()
                setThreadCursor(0)
              }}
            />
          )}
          {isAskClaude ? (
            <ChatPane messages={chatMessages} dispatchStatus={dispatchStatus} />
          ) : (
            <DetailPane
              pluginInfo={pluginInfo}
              detail={detail}
              onAction={handleAction}
              onOpenFile={handleOpenFile}
              editingFile={editingFile}
              onCloseEditor={handleCloseEditor}
              hideComponents={windowMode === 'expanded'}
            />
          )}
          {/* Third column — visible only in expanded mode (CSS width transition) */}
          <div className="dust-third-col" aria-hidden={windowMode !== 'expanded'}>
            <div className="overflow-y-auto p-4 h-full">
              {detail && detail.length > 0 ? (
                <ComponentRenderer components={detail} onAction={handleAction} />
              ) : (
                <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
                  Component output will appear here.
                </p>
              )}
            </div>
          </div>
        </div>

        {/* ── Action bar (with optional palette) ───────────────────── */}
        <div className="relative shrink-0">
          {showActionPalette && (
            <ActionPalette
              pluginInfo={pluginInfo}
              onClose={() => setShowActionPalette(false)}
            />
          )}
          <ActionBar
            hasSelection={displayResults[selectedIndex] != null}
            showPalette={showActionPalette}
            onTogglePalette={() => setShowActionPalette(v => !v)}
            windowMode={windowMode}
            onTransition={transitionTo}
            isChatActive={isChatActive}
          />
        </div>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// SearchBar
// ---------------------------------------------------------------------------

type SearchBarProps = {
  value: string
  onChange: (v: string) => void
  onKeyDown: (e: React.KeyboardEvent) => void
  mode: 'search' | 'chat'
}

const SearchBar = forwardRef<HTMLInputElement, SearchBarProps>(
  ({ value, onChange, onKeyDown, mode }, ref) => (
    <div className="flex h-[52px] shrink-0 items-center gap-3 px-4">
      {/* Leading icon — magnifier in search mode, chat bubble in chat mode */}
      <svg
        aria-hidden="true"
        width="14"
        height="14"
        viewBox="0 0 16 16"
        fill="none"
        className="shrink-0"
        style={{ color: 'var(--text-secondary)' }}
      >
        {mode === 'chat' ? (
          <path
            d="M8 2a6 6 0 0 0-5.2 9l-.8 3 3-.8A6 6 0 1 0 8 2z"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinejoin="round"
          />
        ) : (
          <>
            <circle cx="6.5" cy="6.5" r="5" stroke="currentColor" strokeWidth="1.5" />
            <line x1="10.5" y1="10.5" x2="14.5" y2="14.5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          </>
        )}
      </svg>

      <input
        ref={ref}
        type="search"
        autoFocus
        autoComplete="off"
        autoCorrect="off"
        autoCapitalize="off"
        spellCheck={false}
        value={value}
        placeholder={mode === 'chat' ? 'Message Claude…' : 'Search capabilities…'}
        onChange={e => onChange(e.target.value)}
        onKeyDown={onKeyDown}
        className="flex-1 bg-transparent text-sm outline-none"
        style={{
          color: 'var(--text-primary)',
          fontFamily: 'inherit',
          caretColor: 'var(--color-accent)',
        }}
      />

      <kbd
        className="hidden shrink-0 items-center gap-0.5 rounded px-1.5 py-0.5 text-[10px] sm:flex"
        style={{
          background: 'var(--mic-bg)',
          border: '1px solid var(--pill-border)',
          color: 'var(--text-secondary)',
        }}
      >
        ⌃K
      </kbd>
    </div>
  ),
)
SearchBar.displayName = 'SearchBar'

// ---------------------------------------------------------------------------
// ResultsList
// ---------------------------------------------------------------------------

type ResultsListProps = {
  results: DisplayResult[]
  selectedIndex: number
  onSelect: (i: number) => void
}

function ResultsList({ results, selectedIndex, onSelect }: ResultsListProps) {
  const listRef = useRef<HTMLDivElement>(null)

  // Scroll selected row into view
  useEffect(() => {
    const container = listRef.current
    if (!container) return
    const row = container.children[selectedIndex] as HTMLElement | undefined
    row?.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
  }, [selectedIndex])

  return (
    <div
      ref={listRef}
      role="listbox"
      aria-label="Plugin capabilities"
      className="w-[280px] shrink-0 overflow-y-auto py-1.5"
      style={{ borderRight: '1px solid var(--border)' }}
    >
      {results.length === 0 ? (
        <p
          className="px-4 py-8 text-center text-xs"
          style={{ color: 'var(--text-secondary)' }}
        >
          No capabilities found
        </p>
      ) : (
        results.map((r, i) => {
          if (r.kind === 'ask_claude') {
            return (
              <AskClaudeItem
                key="ask-claude"
                query={r.query}
                selected={i === selectedIndex}
                onClick={() => onSelect(i)}
              />
            )
          }
          return (
            <ResultItem
              key={`${r.match.plugin_id}/${r.match.capability.id}`}
              match={r.match}
              selected={i === selectedIndex}
              onClick={() => onSelect(i)}
            />
          )
        })
      )}
    </div>
  )
}

type ResultItemProps = {
  match: CapabilityMatch
  selected: boolean
  onClick: () => void
}

function ResultItem({ match, selected, onClick }: ResultItemProps) {
  const glyph = GLYPH_CHAR[glyphFor(match)]
  return (
    <button
      type="button"
      role="option"
      aria-selected={selected}
      onClick={onClick}
      className="flex w-full items-center gap-2 px-3 py-2 text-left transition-colors"
      style={{
        background: selected ? 'var(--selected-bg)' : 'transparent',
      }}
    >
      {/* Type glyph */}
      <span
        aria-hidden="true"
        className="w-4 shrink-0 text-center text-[11px] font-mono"
        style={{ color: selected ? 'var(--color-accent)' : 'var(--text-secondary)' }}
      >
        {glyph}
      </span>

      {/* Title + snippet on a single line */}
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-1.5 overflow-hidden">
          <span
            className="shrink-0 text-xs font-semibold"
            style={{ color: 'var(--text-primary)' }}
          >
            {match.capability.name}
          </span>
          <span
            className="truncate text-[11px]"
            style={{ color: 'var(--text-secondary)' }}
          >
            {match.capability.description}
          </span>
        </div>
      </div>
    </button>
  )
}

// ---------------------------------------------------------------------------
// AskClaudeItem — synthetic fallback entry for unmatched queries
// ---------------------------------------------------------------------------

type AskClaudeItemProps = {
  query: string
  selected: boolean
  onClick: () => void
}

function AskClaudeItem({ query, selected, onClick }: AskClaudeItemProps) {
  return (
    <button
      type="button"
      role="option"
      aria-selected={selected}
      onClick={onClick}
      className="flex w-full items-center gap-2 px-3 py-2 text-left transition-colors"
      style={{
        background: selected ? 'var(--selected-bg)' : 'transparent',
      }}
    >
      {/* Terracotta chat icon */}
      <span
        aria-hidden="true"
        className="w-4 shrink-0 text-center text-[11px] font-mono"
        style={{ color: TERRACOTTA }}
      >
        ≡
      </span>

      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-1.5 overflow-hidden">
          <span
            className="shrink-0 text-xs font-semibold"
            style={{ color: 'var(--text-primary)' }}
          >
            Ask Claude
          </span>
          <span
            className="truncate text-[11px]"
            style={{ color: 'var(--text-secondary)' }}
          >
            {query}
          </span>
        </div>
      </div>
    </button>
  )
}

// ---------------------------------------------------------------------------
// DetailPane — PluginInfo + ComponentRenderer output
// ---------------------------------------------------------------------------

type DetailPaneProps = {
  pluginInfo: PluginInfo | null
  detail: Component[] | null
  onAction: ActionHandler
  onOpenFile: OpenFileHandler
  editingFile: { path: string; basename: string; line?: number } | null
  onCloseEditor: () => void
  hideComponents?: boolean
}

function DetailPane({ pluginInfo, detail, onAction, onOpenFile, editingFile, onCloseEditor, hideComponents }: DetailPaneProps) {
  if (editingFile) {
    return (
      <div className="flex flex-1 overflow-hidden">
        <QuickEditor
          path={editingFile.path}
          basename={editingFile.basename}
          line={editingFile.line}
          onClose={onCloseEditor}
        />
      </div>
    )
  }

  if (!pluginInfo) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
          Select a capability and press ↵ to open
        </p>
      </div>
    )
  }

  return (
    <div className="flex flex-1 flex-col overflow-hidden">
      <PluginInfoPanel info={pluginInfo} />

      <div style={{ height: 1, background: 'var(--border)', flexShrink: 0 }} />

      {!hideComponents && (
        <div className="flex-1 overflow-y-auto p-4">
          {detail && detail.length > 0 ? (
            <ComponentRenderer components={detail} onAction={onAction} onOpenFile={onOpenFile} />
          ) : (
            <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
              Plugin output will appear here.
            </p>
          )}
        </div>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// ThreadRail — 180 px left column shown in chat mode when toggled with ⌘T.
// Mutually exclusive with ResultsList (the layout contract in App enforces the
// invariant `!(showResultsColumn && showThreadRail)`).
// ---------------------------------------------------------------------------

type ThreadRailProps = {
  threads: ThreadMeta[]
  status: ThreadsStatus
  activeThreadId: string | null
  cursor: number
  onSelect: (index: number, id: string) => void
  onNewThread: () => void
}

function ThreadRail({ threads, status, activeThreadId, cursor, onSelect, onNewThread }: ThreadRailProps) {
  const listRef = useRef<HTMLDivElement>(null)

  // Keep the keyboard-selected row in view as cursor moves.
  useEffect(() => {
    const container = listRef.current
    if (!container) return
    const row = container.children[cursor] as HTMLElement | undefined
    row?.scrollIntoView({ block: 'nearest' })
  }, [cursor])

  return (
    <div
      className="flex w-[180px] shrink-0 flex-col"
      style={{ borderRight: '1px solid var(--border)' }}
    >
      {/* Header */}
      <div
        className="flex h-[32px] shrink-0 items-center justify-between px-2.5"
        style={{
          background: 'var(--bg-elevated)',
          borderBottom: '1px solid var(--border)',
        }}
      >
        <span
          className="text-[10px] font-semibold uppercase tracking-widest"
          style={{ color: 'var(--text-secondary)' }}
        >
          Threads
        </span>
        <button
          type="button"
          onClick={onNewThread}
          aria-label="New thread (⌘N)"
          title="New thread (⌘N)"
          className="flex h-[18px] w-[18px] items-center justify-center rounded text-[14px] leading-none transition-colors"
          style={{ color: 'var(--text-secondary)' }}
          onMouseEnter={e => { (e.currentTarget as HTMLElement).style.color = 'var(--text-primary)' }}
          onMouseLeave={e => { (e.currentTarget as HTMLElement).style.color = 'var(--text-secondary)' }}
        >
          +
        </button>
      </div>

      {/* Thread list */}
      <div ref={listRef} className="flex-1 overflow-y-auto">
        {threads.length === 0 ? (
          <p
            className="px-2.5 py-3 text-[10px]"
            style={{ color: 'var(--text-secondary)' }}
          >
            {status === 'error'
              ? "Couldn't load threads."
              : status === 'ready'
                ? 'No threads yet — press ⌘N to start one.'
                : 'Loading…'}
          </p>
        ) : (
          threads.map((t, i) => {
            const isSelected = i === cursor
            const isActive = t.id === activeThreadId
            const isLast = i === threads.length - 1
            const untitled = t.title === 'New Conversation'
            return (
              <button
                key={t.id}
                type="button"
                onClick={() => onSelect(i, t.id)}
                className="flex w-full flex-col justify-center gap-[2px] px-2.5 py-1.5 text-left transition-colors"
                style={{
                  height: 40,
                  background: isSelected ? 'var(--selected-bg)' : 'transparent',
                  borderBottom: isLast ? 'none' : '1px solid var(--border)',
                  borderLeft: isActive
                    ? `2px solid ${TERRACOTTA}`
                    : '2px solid transparent',
                }}
                onMouseEnter={e => {
                  if (!isSelected) {
                    ;(e.currentTarget as HTMLElement).style.background = 'var(--hover-bg)'
                  }
                }}
                onMouseLeave={e => {
                  if (!isSelected) {
                    ;(e.currentTarget as HTMLElement).style.background = 'transparent'
                  }
                }}
              >
                <span
                  className="overflow-hidden text-ellipsis whitespace-nowrap text-[12px]"
                  style={{
                    color: 'var(--text-primary)',
                    fontWeight: isActive ? 600 : 500,
                    fontStyle: untitled ? 'italic' : 'normal',
                  }}
                >
                  {t.title || 'Untitled'}
                </span>
                <span
                  className="text-[10px]"
                  style={{
                    color: 'var(--text-secondary)',
                    fontVariantNumeric: 'tabular-nums',
                  }}
                >
                  {agoLabel(t.updated_at)}
                </span>
              </button>
            )
          })
        )}
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// ChatPane — right pane shown when the active result is the Ask-Claude synthetic.
// chatMessages is replaced wholesale by each data_updated event; the chat plugin
// is the single source of truth — this component never appends tokens itself.
// ---------------------------------------------------------------------------

type ChatPaneProps = {
  messages: Component[]
  dispatchStatus: DispatchStatus
}

function ChatPane({ messages, dispatchStatus }: ChatPaneProps) {
  const bottomRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages])

  return (
    <div className="flex flex-1 flex-col overflow-hidden">
      <div
        className="px-4 py-2 text-[10px] font-semibold uppercase tracking-widest shrink-0"
        style={{
          background: 'var(--bg-elevated)',
          color: 'var(--text-secondary)',
          borderBottom: '1px solid var(--border)',
        }}
      >
        Ask Claude
      </div>
      <div className="flex-1 overflow-y-auto p-4">
        {messages.length === 0 ? (
          <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
            {dispatchStatus === 'in_flight'
              ? 'Waiting for response…'
              : 'Type a message and press Enter.'}
          </p>
        ) : (
          <ComponentRenderer components={messages} onAction={async () => {}} />
        )}
        <div ref={bottomRef} />
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// PluginInfoPanel — name, version, capability chips, status dot
// ---------------------------------------------------------------------------

function PluginInfoPanel({ info }: { info: PluginInfo }) {
  return (
    <div
      className="flex shrink-0 flex-col gap-2 p-4"
      style={{ background: 'var(--bg-elevated)' }}
    >
      {/* Header row */}
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h2
            className="truncate text-sm font-semibold"
            style={{ color: 'var(--text-primary)' }}
          >
            {info.manifest.name}
          </h2>
          <p
            className="text-[11px] font-mono"
            style={{ color: 'var(--text-secondary)' }}
          >
            v{info.manifest.version}
            {info.manifest.id !== info.manifest.name && (
              <span style={{ opacity: 0.6 }}> · {info.manifest.id}</span>
            )}
          </p>
        </div>

        {/* Status dot */}
        <div className="flex shrink-0 items-center gap-1.5 pt-0.5">
          <span
            aria-hidden="true"
            className="h-2 w-2 rounded-full"
            style={{
              background: info.healthy ? 'var(--status-green)' : 'var(--status-red)',
              boxShadow: info.healthy
                ? '0 0 6px var(--status-green)'
                : '0 0 6px var(--status-red)',
            }}
          />
          <span
            className="text-[10px] font-mono"
            style={{ color: 'var(--text-secondary)' }}
          >
            {info.healthy ? 'running' : 'offline'}
          </span>
        </div>
      </div>

      {/* Capability chips */}
      {info.manifest.capabilities.length > 0 && (
        <div className="flex flex-wrap gap-1" role="list" aria-label="Capabilities">
          {info.manifest.capabilities.map(cap => (
            <span
              key={cap.id}
              role="listitem"
              className="rounded px-1.5 py-0.5 text-[10px] font-mono"
              style={{
                background: 'var(--mic-bg)',
                border: '1px solid var(--pill-border)',
                color: 'var(--text-secondary)',
              }}
            >
              {cap.name}
            </span>
          ))}
        </div>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// ActionPalette — Ctrl+K overlay listing plugin capabilities as actions
// ---------------------------------------------------------------------------

type ActionPaletteProps = {
  pluginInfo: PluginInfo | null
  onClose: () => void
}

function ActionPalette({ pluginInfo, onClose }: ActionPaletteProps) {
  const capabilities = pluginInfo?.manifest.capabilities ?? []

  return (
    <div
      role="dialog"
      aria-label="Action palette"
      className="absolute bottom-full right-0 z-10 mb-px w-64 overflow-hidden rounded-t-lg"
      style={{
        background: 'var(--bg-elevated)',
        border: '1px solid var(--border)',
        borderBottom: 'none',
        boxShadow: '0 -8px 24px rgba(0,0,0,0.5)',
      }}
    >
      <div
        className="px-3 py-1.5 text-[10px] font-semibold uppercase tracking-widest"
        style={{ color: 'var(--text-secondary)', borderBottom: '1px solid var(--border)' }}
      >
        Actions
      </div>
      {capabilities.length === 0 ? (
        <p className="px-3 py-3 text-[11px]" style={{ color: 'var(--text-secondary)' }}>
          No actions available
        </p>
      ) : (
        capabilities.map(cap => (
          <button
            key={cap.id}
            type="button"
            onClick={onClose}
            className="flex w-full flex-col gap-0.5 px-3 py-2 text-left transition-colors"
            style={{ color: 'var(--text-primary)' }}
            onMouseEnter={e => {
              ;(e.currentTarget as HTMLElement).style.background = 'var(--hover-bg)'
            }}
            onMouseLeave={e => {
              ;(e.currentTarget as HTMLElement).style.background = 'transparent'
            }}
          >
            <span className="text-xs font-medium">{cap.name}</span>
            <span className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
              {cap.description}
            </span>
          </button>
        ))
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// ActionBar — keyboard shortcut hints + palette toggle
// ---------------------------------------------------------------------------

type ActionBarProps = {
  hasSelection: boolean
  showPalette: boolean
  onTogglePalette: () => void
  windowMode: WindowMode
  onTransition: (mode: WindowMode) => void
  isChatActive: boolean
}

function ActionBar({ hasSelection, showPalette, onTogglePalette, windowMode, onTransition: _onTransition, isChatActive }: ActionBarProps) {
  return (
    <div
      className="flex h-[28px] shrink-0 items-center justify-between px-3"
      style={{
        background: 'var(--bg-elevated)',
        borderTop: '1px solid var(--border)',
        color: 'var(--text-secondary)',
      }}
    >
      {/* Left: nav hints */}
      <div className="flex items-center gap-3">
        <ShortcutHint keys={['↑', '↓']} label="navigate" />
        {hasSelection && <ShortcutHint keys={['↵']} label="open" />}
        {isChatActive ? (
          <ShortcutHint keys={['esc']} label="leave chat" />
        ) : (
          <ShortcutHint keys={['esc']} label="hide" />
        )}
        <ShortcutHint keys={['⌘K']} label="collapse" />
        <ShortcutHint keys={['⌘E']} label={windowMode === 'expanded' ? 'shrink' : 'expand'} />
      </div>

      {/* Right: action palette toggle */}
      <button
        type="button"
        onClick={onTogglePalette}
        aria-pressed={showPalette}
        aria-label="Toggle action palette (Ctrl+K)"
        className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] transition-colors"
        style={{
          background: showPalette ? 'var(--selected-bg)' : 'transparent',
          color: showPalette ? 'var(--text-primary)' : 'var(--text-secondary)',
          fontFamily: 'inherit',
        }}
      >
        <kbd style={{ fontFamily: 'inherit' }}>⌃K</kbd>
        <span>actions</span>
      </button>
    </div>
  )
}

function ShortcutHint({ keys, label }: { keys: string[]; label: string }) {
  return (
    <span className="flex items-center gap-1 text-[10px]">
      {keys.map(k => (
        <kbd
          key={k}
          className="rounded px-1 text-[9px]"
          style={{
            background: 'var(--mic-bg)',
            border: '1px solid var(--pill-border)',
            color: 'var(--text-secondary)',
            fontFamily: 'inherit',
          }}
        >
          {k}
        </kbd>
      ))}
      <span style={{ opacity: 0.55 }}>{label}</span>
    </span>
  )
}

// ---------------------------------------------------------------------------
// CollapsedBar — full-width 52px bar shown when windowMode === 'collapsed'
// ---------------------------------------------------------------------------

type CollapsedBarProps = {
  activeTask: string | null
  onExpand: () => void
}

function CollapsedBar({ activeTask, onExpand }: CollapsedBarProps) {
  return (
    <div
      className="flex h-[52px] w-full items-center justify-between px-4"
      style={{
        background: 'var(--bg)',
        border: '1px solid var(--border)',
        boxShadow: '0 2px 12px rgba(0,0,0,0.7)',
      }}
    >
      {/* Active task badge */}
      <div className="flex items-center gap-2">
        <span
          aria-hidden="true"
          className="h-1.5 w-1.5 rounded-full"
          style={{ background: activeTask ? 'var(--status-green)' : 'var(--text-secondary)', flexShrink: 0 }}
        />
        <span
          className="text-xs font-mono truncate max-w-[400px]"
          style={{ color: activeTask ? 'var(--text-primary)' : 'var(--text-secondary)' }}
        >
          {activeTask ?? 'No active task'}
        </span>
      </div>

      {/* Shortcut hints + expand caret */}
      <div className="flex items-center gap-3 shrink-0">
        <span className="text-[10px]" style={{ color: 'var(--text-secondary)', opacity: 0.6 }}>
          esc to restore · ⌘E to expand
        </span>
        <button
          type="button"
          onClick={onExpand}
          aria-label="Expand dust (Esc)"
          className="flex items-center justify-center h-6 w-6 rounded transition-colors"
          style={{ color: 'var(--text-secondary)' }}
          onMouseEnter={e => { (e.currentTarget as HTMLElement).style.background = 'var(--hover-bg)' }}
          onMouseLeave={e => { (e.currentTarget as HTMLElement).style.background = 'transparent' }}
        >
          ˄
        </button>
      </div>
    </div>
  )
}
