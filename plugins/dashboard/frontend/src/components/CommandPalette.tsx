import {
  useCallback,
  useEffect,
  useRef,
  useState,
} from 'react'
import { createPortal } from 'react-dom'
import { Command } from 'cmdk'
import { listModules } from '../modules/registry'
import { setInteractiveBounds, setFullClickThrough, wailsRuntime, getChannelStatus } from '../lib/wails'
import type { Module, ExtensionItem } from '../modules/registry'
import type { ChannelStatus } from '../types'
import {
  CommandPaletteContext,
  type ConversationBridge,
  type PaletteMode,
  useCommandPalette,
} from './commandPaletteContext'
import { useVoiceInput } from '../hooks/useVoiceInput'
import { sampleBins } from '../utils'
import './CommandPalette.css'

// ---------------------------------------------------------------------------
// ActionBridge — passed from App so the palette can invoke orchestrator actions
// ---------------------------------------------------------------------------

export interface ActionBridge {
  runMission: (task: string) => Promise<{ request_id: string; status: string; task: string } | null>
  triggerScan: () => Promise<{ output: string; error?: string }>
  triggerCleanup: () => Promise<{ output: string; error?: string }>
  reloadPersonas: () => Promise<boolean>
  addToast: (text: string, type?: 'info' | 'success' | 'warning' | 'error') => void
}

// ---------------------------------------------------------------------------
// Module categorization
// ---------------------------------------------------------------------------

const MODULE_CATEGORIES: Record<string, string> = {
  missions: 'Operations',
  events: 'Operations',
  metrics: 'Intelligence',
  personas: 'Intelligence',
  findings: 'Intelligence',
  settings: 'System',
}

function groupByCategory(modules: Module[]): Record<string, Module[]> {
  const groups: Record<string, Module[]> = {}
  for (const mod of modules) {
    const cat = MODULE_CATEGORIES[mod.id] ?? 'Other'
    if (!groups[cat]) groups[cat] = []
    groups[cat].push(mod)
  }
  return groups
}

function normalizeWhitespace(text: string): string {
  return text.replace(/\s+/g, ' ').trim()
}

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

// ---------------------------------------------------------------------------
// Fuzzy search — Raycast-style scorer
// ---------------------------------------------------------------------------

interface FuzzyResult {
  score: number
  indices: number[]
}

function fuzzyMatch(query: string, target: string): FuzzyResult {
  if (!query) return { score: 1, indices: [] }

  const q = query.toLowerCase()
  const t = target.toLowerCase()

  // Exact
  if (t === q) return { score: 1.0, indices: Array.from({ length: t.length }, (_, i) => i) }

  // Prefix
  if (t.startsWith(q)) {
    return { score: 0.85, indices: Array.from({ length: q.length }, (_, i) => i) }
  }

  // Word prefix (first char of any word)
  const words = t.split(/[\s_-]+/)
  let wordOffset = 0
  for (const word of words) {
    if (word.startsWith(q)) {
      return { score: 0.75, indices: Array.from({ length: q.length }, (_, i) => wordOffset + i) }
    }
    wordOffset += word.length + 1
  }

  // Contains (substring)
  const ci = t.indexOf(q)
  if (ci !== -1) {
    return { score: 0.65 - ci * 0.005, indices: Array.from({ length: q.length }, (_, i) => ci + i) }
  }

  // Fuzzy (char-by-char with gaps allowed)
  const indices: number[] = []
  let ti = 0
  for (let qi = 0; qi < q.length; qi++) {
    let found = false
    while (ti < t.length) {
      if (t[ti] === q[qi]) {
        indices.push(ti)
        ti++
        found = true
        break
      }
      ti++
    }
    if (!found) return { score: 0, indices: [] }
  }
  const span = indices[indices.length - 1] - indices[0] + 1
  const tightness = q.length / span
  return { score: 0.1 + tightness * 0.4, indices }
}

/** cmdk custom filter — searches name (value) + keywords */
function cmdFilter(value: string, search: string, keywords?: string[]): number {
  if (!search.trim()) return 1
  const q = search.toLowerCase()

  const nameScore = fuzzyMatch(q, value.toLowerCase()).score

  let kwScore = 0
  if (keywords?.length) {
    for (const kw of keywords) {
      const s = fuzzyMatch(q, kw.toLowerCase()).score
      if (s > kwScore) kwScore = s
    }
    kwScore *= 0.85
  }

  return Math.max(nameScore, kwScore)
}

function HighlightedName({ name, query }: { name: string; query: string }) {
  if (!query.trim()) return <>{name}</>
  const { indices } = fuzzyMatch(query.toLowerCase(), name.toLowerCase())
  if (!indices.length) return <>{name}</>

  const indexSet = new Set(indices)
  const segments: { text: string; highlight: boolean }[] = []
  let i = 0
  while (i < name.length) {
    const isHighlighted = indexSet.has(i)
    let j = i + 1
    while (j < name.length && indexSet.has(j) === isHighlighted) j++
    segments.push({ text: name.slice(i, j), highlight: isHighlighted })
    i = j
  }
  return (
    <>
      {segments.map((seg, idx) =>
        seg.highlight
          ? <mark key={idx} className="cmd-highlight">{seg.text}</mark>
          : <span key={idx}>{seg.text}</span>,
      )}
    </>
  )
}

function moduleVoiceForms(module: Module): string[] {
  return Array.from(new Set([
    module.id.toLowerCase(),
    module.name.toLowerCase(),
  ]))
}

function normalizeConversationVoice(text: string, modules: Module[]): string {
  let next = text
  for (const mod of modules) {
    for (const form of moduleVoiceForms(mod)) {
      const pattern = new RegExp(`\\b(?:at|mention|app|module)\\s+${escapeRegExp(form)}\\b`, 'gi')
      next = next.replace(pattern, `@${mod.id} `)
    }
  }
  return normalizeWhitespace(next)
}

function resolveVoiceModuleTarget(text: string, modules: Module[]): Module | null {
  const normalized = normalizeWhitespace(text.toLowerCase())
  const stripped = normalized
    .replace(/^(?:open|show|go to|switch to|take me to|bring up|launch)\s+/, '')
    .replace(/^the\s+/, '')
    .replace(/\s+(?:panel|module|app)$/, '')
  for (const mod of modules) {
    if (moduleVoiceForms(mod).includes(stripped)) return mod
  }
  return null
}

function mergeVoiceTranscript(base: string, transcript: string): string {
  return normalizeWhitespace([base, transcript].filter(Boolean).join(' '))
}

function CommandsModeIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
      <rect x="4" y="4" width="6" height="6" rx="1.25" />
      <rect x="14" y="4" width="6" height="6" rx="1.25" />
      <rect x="4" y="14" width="6" height="6" rx="1.25" />
      <rect x="14" y="14" width="6" height="6" rx="1.25" />
    </svg>
  )
}

function ConversationModeIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path
        d="M12 22C17.5228 22 22 17.5228 22 12C22 6.47715 17.5228 2 12 2C6.47715 2 2 6.47715 2 12C2 13.5997 2.37562 15.1116 3.04346 16.4525C3.22094 16.8088 3.28001 17.2161 3.17712 17.6006L2.58151 19.8267C2.32295 20.793 3.20701 21.677 4.17335 21.4185L6.39939 20.8229C6.78393 20.72 7.19121 20.7791 7.54753 20.9565C8.88837 21.6244 10.4003 22 12 22Z"
        stroke="currentColor"
        strokeWidth="1.5"
      />
    </svg>
  )
}

interface VoiceButtonProps {
  active: boolean
  supported: boolean
  bars: number[]
  title: string
  onClick: () => void
}

function VoiceButton({ active, supported, bars, title, onClick }: VoiceButtonProps) {
  return (
    <button
      type="button"
      className={`cmd-voice-btn${active ? ' cmd-voice-btn--active' : ''}`}
      onClick={onClick}
      aria-label={active ? 'Stop voice input' : 'Start voice input'}
      title={title}
      disabled={!supported}
    >
      {active ? (
        <span className="cmd-voice-bars" aria-hidden="true">
          {bars.map((bar, index) => (
            <span
              key={index}
              className="cmd-voice-bar"
              style={{ transform: `scaleY(${0.35 + bar * 0.85})` }}
            />
          ))}
        </span>
      ) : (
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <path
            d="M12 15.5A3.5 3.5 0 0 0 15.5 12V7a3.5 3.5 0 1 0-7 0v5a3.5 3.5 0 0 0 3.5 3.5Z"
            stroke="currentColor"
            strokeWidth="1.6"
          />
          <path d="M18 11.5a6 6 0 1 1-12 0" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
          <path d="M12 17.5V21" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
        </svg>
      )}
    </button>
  )
}

interface ModeSwitcherProps {
  mode: PaletteMode
  onSelect: (mode: PaletteMode) => void
}

function ModeSwitcher({ mode, onSelect }: ModeSwitcherProps) {
  return (
    <div className="cmd-mode-switcher" aria-label="Palette mode">
      <button
        type="button"
        className={`cmd-mode-btn${mode === 'command' ? ' cmd-mode-btn--active' : ''}`}
        aria-pressed={mode === 'command'}
        aria-label="Commands"
        title="Commands"
        onClick={() => onSelect('command')}
      >
        <CommandsModeIcon />
      </button>
      <button
        type="button"
        className={`cmd-mode-btn${mode === 'conversation' ? ' cmd-mode-btn--active' : ''}`}
        aria-pressed={mode === 'conversation'}
        aria-label="Conversation"
        title="Conversation"
        onClick={() => onSelect('conversation')}
      >
        <ConversationModeIcon />
      </button>
    </div>
  )
}

function DiscordIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
      <path d="M20.317 4.37a19.791 19.791 0 0 0-4.885-1.515.074.074 0 0 0-.079.037c-.21.375-.444.864-.608 1.25a18.27 18.27 0 0 0-5.487 0 12.64 12.64 0 0 0-.617-1.25.077.077 0 0 0-.079-.037A19.736 19.736 0 0 0 3.677 4.37a.07.07 0 0 0-.032.027C.533 9.046-.32 13.58.099 18.057c.002.022.015.042.033.053a19.901 19.901 0 0 0 5.993 3.03.078.078 0 0 0 .084-.028c.462-.63.874-1.295 1.226-1.994a.076.076 0 0 0-.041-.106 13.107 13.107 0 0 1-1.872-.892.077.077 0 0 1-.008-.128 10.2 10.2 0 0 0 .372-.292.074.074 0 0 1 .077-.01c3.928 1.793 8.18 1.793 12.062 0a.074.074 0 0 1 .078.01c.12.098.246.198.373.292a.077.077 0 0 1-.006.127 12.299 12.299 0 0 1-1.873.892.077.077 0 0 0-.041.107c.36.698.772 1.362 1.225 1.993a.076.076 0 0 0 .084.028 19.839 19.839 0 0 0 6.002-3.03.077.077 0 0 0 .032-.054c.5-5.177-.838-9.674-3.549-13.66a.061.061 0 0 0-.031-.03zM8.02 15.33c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.956-2.419 2.157-2.419 1.21 0 2.176 1.096 2.157 2.42 0 1.333-.956 2.418-2.157 2.418zm7.975 0c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.955-2.419 2.157-2.419 1.21 0 2.176 1.096 2.157 2.42 0 1.333-.946 2.418-2.157 2.418z" />
    </svg>
  )
}

function TelegramIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
      <path d="M11.944 0A12 12 0 0 0 0 12a12 12 0 0 0 12 12 12 12 0 0 0 12-12A12 12 0 0 0 12 0a12 12 0 0 0-.056 0zm4.962 7.224c.1-.002.321.023.465.14a.506.506 0 0 1 .171.325c.016.093.036.306.02.472-.18 1.898-.962 6.502-1.36 8.627-.168.9-.499 1.201-.82 1.23-.696.065-1.225-.46-1.9-.902-1.056-.693-1.653-1.124-2.678-1.8-1.185-.78-.417-1.21.258-1.91.177-.184 3.247-2.977 3.307-3.23.007-.032.014-.15-.056-.212s-.174-.041-.249-.024c-.106.024-1.793 1.14-5.061 3.345-.48.33-.913.49-1.302.48-.428-.008-1.252-.241-1.865-.44-.752-.245-1.349-.374-1.297-.789.027-.216.325-.437.893-.663 3.498-1.524 5.83-2.529 6.998-3.014 3.332-1.386 4.025-1.627 4.476-1.635z" />
    </svg>
  )
}

function ChannelStatusIcons({ channels }: { channels: ChannelStatus[] | null }) {
  const discord = channels?.find(c => c.platform === 'discord')
  const telegram = channels?.find(c => c.platform === 'telegram')

  const connected = (ch: ChannelStatus | undefined) =>
    !!(ch?.configured && ch?.active && ch?.error_count === 0)

  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
      <span
        title={`Discord: ${connected(discord) ? 'connected' : 'disconnected'}`}
        style={{ color: '#5865F2', opacity: connected(discord) ? 1 : 0.3, display: 'flex' }}
      >
        <DiscordIcon />
      </span>
      <span
        title={`Telegram: ${connected(telegram) ? 'connected' : 'disconnected'}`}
        style={{ color: '#26A5E4', opacity: connected(telegram) ? 1 : 0.3, display: 'flex' }}
      >
        <TelegramIcon />
      </span>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Overlay (internal — mounted/unmounted by CommandPalettePortal)
// ---------------------------------------------------------------------------

interface CommandPaletteOverlayProps {
  initialQuery: string
  initialMode: PaletteMode
  onClose: () => void
  onOpenModule: (id: string) => void
  conversationBridge: ConversationBridge | null
  actionBridge: ActionBridge | null
}

function CommandPaletteOverlay({
  initialQuery,
  initialMode,
  onClose,
  onOpenModule,
  conversationBridge,
  actionBridge,
}: CommandPaletteOverlayProps) {
  // Seeded from initialQuery on mount; cleared on mode toggle
  const [query, setQuery] = useState(initialQuery)
  const [paletteMode, setPaletteMode] = useState<PaletteMode>(initialMode)
  const [channelStatus, setChannelStatus] = useState<ChannelStatus[] | null>(null)

  useEffect(() => {
    let cancelled = false
    getChannelStatus()
      .then(data => { if (!cancelled) setChannelStatus(data) })
      .catch(() => { if (!cancelled) setChannelStatus(null) })
    return () => { cancelled = true }
  }, [])
  const commandRef = useRef<HTMLDivElement>(null)
  const convEndRef = useRef<HTMLDivElement>(null)
  const dialogRef = useRef<HTMLDivElement>(null)

  // Report palette bounds to the Go overlay so its region becomes interactive.
  useEffect(() => {
    const el = dialogRef.current
    if (!el) return
    requestAnimationFrame(() => {
      const rect = el.getBoundingClientRect()
      const screenH = window.innerHeight
      setInteractiveBounds(
        rect.left,
        screenH - (rect.top + rect.height),
        rect.width,
        rect.height,
      )
    })
  }, [])

  // @ extension state
  const [atMenuOpen, setAtMenuOpen] = useState(false)
  const [atQuery, setAtQuery] = useState('')
  const [activeExtension, setActiveExtension] = useState<{
    moduleId: string
    label: string
    items: ExtensionItem[]
  } | null>(null)
  const atCmdRef = useRef<HTMLDivElement>(null)
  const voiceBaseRef = useRef('')

  // Run-mission inline input state
  const [pendingRunMission, setPendingRunMission] = useState(false)
  const [runMissionInput, setRunMissionInput] = useState('')

  const paletteMessages = conversationBridge?.messages ?? []
  const hasMessages = paletteMessages.length > 0

  const resetConversationChrome = useCallback((clearPreview: boolean) => {
    setAtMenuOpen(false)
    setAtQuery('')
    if (clearPreview) setActiveExtension(null)
  }, [])

  // Auto-scroll conversation to bottom when messages update
  useEffect(() => {
    convEndRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [paletteMessages.length])

  const setPaletteModeAndReset = useCallback((nextMode: PaletteMode) => {
    voiceBaseRef.current = ''
    setPaletteMode(nextMode)
    setQuery('')
    setPendingRunMission(false)
    setRunMissionInput('')
    resetConversationChrome(true)
  }, [resetConversationChrome])

  const handleConversationSubmit = useCallback(() => {
    if (!query.trim() || !conversationBridge) return
    conversationBridge.submit(query.trim())
    setQuery('')
    resetConversationChrome(true)
  }, [query, conversationBridge, resetConversationChrome])

  const handleSelect = useCallback(
    (id: string) => {
      onOpenModule(id)
      onClose()
    },
    [onOpenModule, onClose],
  )

  const modules = listModules()
  const moduleIndexMap = new Map(modules.map((m, i) => [m, i]))
  const grouped = groupByCategory(modules)
  const {
    supported: voiceSupported,
    listening: voiceListening,
    error: voiceError,
    frequencyData: voiceFrequencyData,
    toggle: toggleVoice,
    stop: stopVoice,
  } = useVoiceInput({
    onInterimTranscript(text) {
      const draft = mergeVoiceTranscript(voiceBaseRef.current, text)
      if (paletteMode === 'conversation') {
        applyConversationQuery(normalizeConversationVoice(draft, modules))
      } else {
        setQuery(draft)
      }
    },
    onFinalTranscript(text) {
      const draft = mergeVoiceTranscript(voiceBaseRef.current, text)
      voiceBaseRef.current = ''
      if (paletteMode === 'conversation') {
        applyConversationQuery(normalizeConversationVoice(draft, modules))
        return
      }
      const target = resolveVoiceModuleTarget(draft, modules)
      if (target) {
        handleSelect(target.id)
        return
      }
      setQuery(draft)
    },
  })
  const voiceBars = sampleBins(voiceFrequencyData, 5)
  const selectPaletteMode = useCallback((nextMode: PaletteMode) => {
    voiceBaseRef.current = ''
    stopVoice()
    setPaletteModeAndReset(nextMode)
  }, [setPaletteModeAndReset, stopVoice])

  // @ extension helpers
  const modulesWithExtensions = modules.filter(m => m.extension)
  const filteredAtModules = atQuery
    ? modulesWithExtensions.filter(
        m =>
          m.id.toLowerCase().includes(atQuery.toLowerCase()) ||
          m.name.toLowerCase().includes(atQuery.toLowerCase()),
      )
    : modulesWithExtensions

  function applyConversationQuery(val: string) {
    setQuery(val)

    const mentionIds = Array.from(val.matchAll(/@([a-z0-9_-]+)/gi), match => match[1].toLowerCase())
    if (activeExtension && !mentionIds.includes(activeExtension.moduleId.toLowerCase())) {
      setActiveExtension(null)
    }

    const atIdx = val.lastIndexOf('@')
    if (atIdx !== -1) {
      const after = val.slice(atIdx + 1)
      if (!after.includes(' ')) {
        setAtMenuOpen(true)
        setAtQuery(after)
        setActiveExtension(null)
        return
      }
    }
    if (atMenuOpen) {
      resetConversationChrome(false)
    }
  }

  function handleDialogKeyDownCapture(e: React.KeyboardEvent<HTMLDivElement>) {
    if (e.shiftKey && e.key === 'Tab') {
      e.preventDefault()
      e.stopPropagation()
      selectPaletteMode(paletteMode === 'command' ? 'conversation' : 'command')
      return
    }
    if (e.key !== 'Escape') return
    e.preventDefault()
    e.stopPropagation()
    if (voiceListening) {
      voiceBaseRef.current = ''
      stopVoice()
      return
    }
    if (pendingRunMission) {
      setPendingRunMission(false)
      setRunMissionInput('')
      return
    }
    if (paletteMode === 'conversation' && atMenuOpen) {
      resetConversationChrome(false)
      return
    }
    onClose()
  }

  function handleConvInputChange(e: React.ChangeEvent<HTMLInputElement>) {
    applyConversationQuery(e.target.value)
  }

  async function handleExtensionSelect(mod: Module) {
    const atIdx = query.lastIndexOf('@')
    const newQuery = query.slice(0, atIdx) + '@' + mod.id + ' '
    setQuery(newQuery)
    setAtMenuOpen(false)
    setAtQuery('')
    if (mod.extension) {
      // Set an empty preview immediately so the panel appears while fetching.
      setActiveExtension({ moduleId: mod.id, label: mod.extension.label, items: [] })
      const items = await mod.extension.fetchItems()
      setActiveExtension({ moduleId: mod.id, label: mod.extension.label, items })
    }
  }

  // ---------------------------------------------------------------------------
  // Action command handlers
  // ---------------------------------------------------------------------------

  const handleScanNen = useCallback(async () => {
    if (!actionBridge) return
    onClose()
    const result = await actionBridge.triggerScan()
    if (result.error) {
      actionBridge.addToast(`Scan failed: ${result.error}`, 'error')
    } else {
      const summary = result.output?.split('\n')[0]?.slice(0, 60).trim() || 'complete'
      actionBridge.addToast(`Scan complete — ${summary}`, 'success')
    }
  }, [actionBridge, onClose])

  const handleCleanupWorktrees = useCallback(async () => {
    if (!actionBridge) return
    onClose()
    const result = await actionBridge.triggerCleanup()
    if (result.error) {
      actionBridge.addToast(`Cleanup failed: ${result.error}`, 'error')
    } else {
      const summary = result.output?.split('\n')[0]?.slice(0, 60).trim() || 'complete'
      actionBridge.addToast(`Cleanup complete — ${summary}`, 'success')
    }
  }, [actionBridge, onClose])

  const handleReloadPersonas = useCallback(async () => {
    if (!actionBridge) return
    onClose()
    const ok = await actionBridge.reloadPersonas()
    actionBridge.addToast(ok ? 'Personas reloaded' : 'Failed to reload personas', ok ? 'success' : 'error')
  }, [actionBridge, onClose])

  const handleRunMissionSubmit = useCallback(async () => {
    if (!actionBridge || !runMissionInput.trim()) return
    onClose()
    const result = await actionBridge.runMission(runMissionInput.trim())
    if (result) {
      actionBridge.addToast(`Mission queued — ${result.request_id}`, 'success')
    } else {
      actionBridge.addToast('Failed to launch mission', 'error')
    }
  }, [actionBridge, runMissionInput, onClose])

  // Handles keyboard for the cmdk Command container (command mode)
  function handleCommandKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (!e.metaKey && !e.ctrlKey && !e.shiftKey && !e.altKey && query === '') {
      // j/k vim navigation
      if (e.key === 'j' || e.key === 'k') {
        e.preventDefault()
        const arrowKey = e.key === 'j' ? 'ArrowDown' : 'ArrowUp'
        const synth = new KeyboardEvent('keydown', { key: arrowKey, bubbles: true, cancelable: true })
        commandRef.current?.dispatchEvent(synth)
        return
      }
    }
    // Cmd+1-9 — open module by registry position
    if (e.metaKey && !e.shiftKey && !e.altKey && e.key >= '1' && e.key <= '9') {
      e.preventDefault()
      const idx = parseInt(e.key, 10) - 1
      if (idx < modules.length) {
        handleSelect(modules[idx].id)
      }
      return
    }
  }

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Command palette"
      className="cmd-overlay"
      onClick={onClose}
    >
      <div
        ref={dialogRef}
        className={`cmd-dialog cmd-dialog--${paletteMode}`}
        onClick={e => e.stopPropagation()}
        onKeyDownCapture={handleDialogKeyDownCapture}
      >
        {/* Command mode — remounts on every switch, giving autoFocus to Command.Input */}
        {paletteMode === 'command' && (
          <div className="cmd-mode-content">
            {pendingRunMission ? (
              /* Inline mission input sub-view */
              <>
                <div className="cmd-input-row">
                  <svg
                    className="cmd-search-icon"
                    width="14"
                    height="14"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    aria-hidden="true"
                  >
                    <path d="M5 12h14M12 5l7 7-7 7" />
                  </svg>
                  <input
                    autoFocus
                    className="cmd-input"
                    value={runMissionInput}
                    onChange={e => setRunMissionInput(e.target.value)}
                    onKeyDown={e => {
                      if (e.key === 'Enter') {
                        e.preventDefault()
                        void handleRunMissionSubmit()
                      }
                    }}
                    placeholder="Describe the mission…"
                    autoComplete="off"
                    spellCheck={false}
                  />
                  <kbd className="cmd-kbd">esc</kbd>
                </div>
                <div className="cmd-action-context">
                  <span className="cmd-action-context-label">Run mission</span>
                  <span className="cmd-action-context-hint">Enter a task description and press ↵ to launch</span>
                </div>
                <div className="cmd-mode-bar" onClick={e => e.stopPropagation()}>
                  <ModeSwitcher mode={paletteMode} onSelect={selectPaletteMode} />
                  <ChannelStatusIcons channels={channelStatus} />
                  <div className="cmd-mode-hints">
                    <span className="cmd-footer-hint">
                      <kbd className="cmd-kbd">↵</kbd>launch
                    </span>
                    <span className="cmd-footer-hint">
                      <kbd className="cmd-kbd">esc</kbd>back
                    </span>
                  </div>
                </div>
              </>
            ) : (
              /* Normal command mode with cmdk */
              <Command
                ref={commandRef}
                label="Command palette"
                filter={cmdFilter}
                loop
                onKeyDown={handleCommandKeyDown}
              >
                <div className="cmd-input-row">
                  <svg
                    className="cmd-search-icon"
                    width="14"
                    height="14"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    aria-hidden="true"
                  >
                    <circle cx="11" cy="11" r="8" />
                    <path d="m21 21-4.35-4.35" />
                  </svg>
                  <Command.Input
                    autoFocus
                    value={query}
                    onValueChange={setQuery}
                    placeholder="Go to module or run action…"
                    className={`cmd-input${voiceListening ? ' cmd-input--listening' : ''}`}
                  />
                  <VoiceButton
                    active={voiceListening}
                    supported={voiceSupported}
                    bars={voiceBars}
                    onClick={() => {
                      if (!voiceListening) voiceBaseRef.current = query
                      else voiceBaseRef.current = ''
                      toggleVoice()
                    }}
                    title={voiceError ?? (voiceListening ? 'Stop voice input' : 'Start voice input')}
                  />
                  <kbd className="cmd-kbd">esc</kbd>
                </div>

                <Command.List className="cmd-list">
                  <Command.Empty className="cmd-empty">No results found.</Command.Empty>
                  {Object.entries(grouped).map(([category, mods]) => (
                    <Command.Group
                      key={category}
                      heading={category}
                      className="cmd-group"
                    >
                      {mods.map(mod => {
                        const Icon = mod.icon
                        const modIdx = moduleIndexMap.get(mod) ?? -1
                        const shortcutNum = modIdx >= 0 && modIdx < 9 ? `⌘${modIdx + 1}` : undefined
                        const desc = mod.description
                          ? mod.description.length > 60
                            ? mod.description.slice(0, 57) + '…'
                            : mod.description
                          : undefined
                        return (
                          <Command.Item
                            key={mod.id}
                            value={mod.name}
                            keywords={mod.keywords}
                            onSelect={() => handleSelect(mod.id)}
                            className="cmd-item"
                          >
                            <span className="cmd-item-icon" aria-hidden="true">
                              <Icon size={14} />
                            </span>
                            <span className="cmd-item-label">
                              <HighlightedName name={mod.name} query={query} />
                            </span>
                            {desc && (
                              <span className="cmd-item-desc">{desc}</span>
                            )}
                            {shortcutNum && (
                              <span className="cmd-shortcut-badge" title={`Press ${shortcutNum} to open`}>{shortcutNum}</span>
                            )}
                          </Command.Item>
                        )
                      })}
                    </Command.Group>
                  ))}

                  <Command.Group heading="Actions" className="cmd-group">
                    <Command.Item
                      value="Run mission"
                      keywords={['run', 'mission', 'launch', 'task', 'orchestrator']}
                      onSelect={() => setPendingRunMission(true)}
                      className="cmd-item"
                    >
                      <span className="cmd-item-icon" aria-hidden="true">
                        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                          <path d="M5 12h14M12 5l7 7-7 7" />
                        </svg>
                      </span>
                      <span className="cmd-item-label">
                        <HighlightedName name="Run mission" query={query} />
                      </span>
                      <span className="cmd-item-desc">Launch a new orchestrator task</span>
                    </Command.Item>
                    <Command.Item
                      value="Scan (Nen)"
                      keywords={['scan', 'nen', 'security', 'findings', 'scanner']}
                      onSelect={() => void handleScanNen()}
                      className="cmd-item"
                    >
                      <span className="cmd-item-icon" aria-hidden="true">
                        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                          <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
                        </svg>
                      </span>
                      <span className="cmd-item-label">
                        <HighlightedName name="Scan (Nen)" query={query} />
                      </span>
                      <span className="cmd-item-desc">Run a Nen security scan now</span>
                    </Command.Item>
                    <Command.Item
                      value="Cleanup worktrees"
                      keywords={['cleanup', 'worktrees', 'orphaned', 'prune']}
                      onSelect={() => void handleCleanupWorktrees()}
                      className="cmd-item"
                    >
                      <span className="cmd-item-icon" aria-hidden="true">
                        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                          <polyline points="3 6 5 6 21 6" />
                          <path d="M19 6l-1 14H6L5 6" />
                          <path d="M10 11v6M14 11v6" />
                          <path d="M9 6V4h6v2" />
                        </svg>
                      </span>
                      <span className="cmd-item-label">
                        <HighlightedName name="Cleanup worktrees" query={query} />
                      </span>
                      <span className="cmd-item-desc">Prune orphaned git worktrees</span>
                    </Command.Item>
                    <Command.Item
                      value="Reload personas"
                      keywords={['reload', 'personas', 'refresh', 'agents']}
                      onSelect={() => void handleReloadPersonas()}
                      className="cmd-item"
                    >
                      <span className="cmd-item-icon" aria-hidden="true">
                        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                          <polyline points="23 4 23 10 17 10" />
                          <polyline points="1 20 1 14 7 14" />
                          <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
                        </svg>
                      </span>
                      <span className="cmd-item-label">
                        <HighlightedName name="Reload personas" query={query} />
                      </span>
                      <span className="cmd-item-desc">Refresh persona definitions from disk</span>
                    </Command.Item>
                  </Command.Group>
                </Command.List>

                <div className="cmd-mode-bar" onClick={e => e.stopPropagation()}>
                  <ModeSwitcher mode={paletteMode} onSelect={selectPaletteMode} />
                  <ChannelStatusIcons channels={channelStatus} />
                  <div className="cmd-mode-hints">
                    <span className="cmd-footer-hint">
                      <kbd className="cmd-kbd">↵</kbd>open
                    </span>
                    <span className="cmd-footer-hint">
                      <kbd className="cmd-kbd">↑↓ jk</kbd>navigate
                    </span>
                    <span className="cmd-footer-hint">
                      <kbd className="cmd-kbd">⇧Tab</kbd>switch
                    </span>
                    <span className="cmd-footer-hint">
                      <kbd className="cmd-kbd">esc</kbd>close
                    </span>
                  </div>
                </div>
              </Command>
            )}
          </div>
        )}

        {/* Conversation mode — remounts on every switch, giving autoFocus to plain input */}
        {paletteMode === 'conversation' && (
          <div className="cmd-mode-content">
            <div className="cmd-input-row">
              <input
                autoFocus
                value={query}
                onChange={handleConvInputChange}
                onKeyDown={e => {
                  if (atMenuOpen) {
                    if (e.key === 'ArrowDown' || e.key === 'ArrowUp' || e.key === 'Enter') {
                      e.preventDefault()
                      atCmdRef.current?.dispatchEvent(
                        new KeyboardEvent('keydown', { key: e.key, bubbles: true, cancelable: true }),
                      )
                      return
                    }
                  }
                  if (e.key === 'Enter' && !e.metaKey && !e.shiftKey) {
                    e.preventDefault()
                    handleConversationSubmit()
                  }
                }}
                placeholder={hasMessages ? 'Ask follow-up…' : 'Ask AI anything…'}
                className={`cmd-input${voiceListening ? ' cmd-input--listening' : ''}`}
                autoComplete="off"
                spellCheck={false}
              />
              <VoiceButton
                active={voiceListening}
                supported={voiceSupported}
                bars={voiceBars}
                onClick={() => {
                  if (!voiceListening) voiceBaseRef.current = query
                  else voiceBaseRef.current = ''
                  toggleVoice()
                }}
                title={voiceError ?? (voiceListening ? 'Stop voice input' : 'Start voice input')}
              />
              <kbd className="cmd-kbd">esc</kbd>
            </div>

            {/* @ extension dropdown — floats below input row */}
            {atMenuOpen && filteredAtModules.length > 0 && (
              <Command
                ref={atCmdRef}
                shouldFilter={false}
                className="cmd-at-dropdown"
                aria-label="Mention a module"
              >
                <Command.List>
                  <Command.Group heading="Mention module" className="cmd-group">
                    {filteredAtModules.map(mod => {
                      const Icon = mod.icon
                      return (
                        <Command.Item
                          key={mod.id}
                          value={mod.id}
                          onSelect={() => handleExtensionSelect(mod)}
                          className="cmd-item"
                        >
                          <span className="cmd-item-icon" aria-hidden="true">
                            <Icon size={12} />
                          </span>
                          <span className="cmd-item-label cmd-at-tag">@{mod.id}</span>
                          <span className="cmd-item-meta">{mod.extension?.label}</span>
                        </Command.Item>
                      )
                    })}
                  </Command.Group>
                </Command.List>
              </Command>
            )}

            {/* Extension preview — shown after selecting @module */}
            {activeExtension && (
              <div className="cmd-ext-preview" role="region" aria-label={activeExtension.label}>
                <div className="cmd-ext-preview-header">
                  <span className="cmd-ext-preview-label">{activeExtension.label}</span>
                  <button
                    type="button"
                    className="cmd-ext-preview-close"
                    onClick={() => setActiveExtension(null)}
                    aria-label="Dismiss preview"
                  >
                    ✕
                  </button>
                </div>
                {activeExtension.items.map(item => (
                  <div key={item.id} className="cmd-ext-preview-item">
                    <span className="cmd-ext-preview-title">{item.title}</span>
                    {item.subtitle && (
                      <span className={`cmd-ext-preview-sub cmd-ext-preview-sub--${item.subtitle.split(' ')[0]}`}>
                        {item.subtitle}
                      </span>
                    )}
                  </div>
                ))}
              </div>
            )}

            <div
              className="cmd-conversation"
              aria-live="polite"
              aria-label="Conversation"
            >
              {paletteMessages.length === 0 ? (
                <div className="cmd-conv-empty">
                  Start a conversation — results appear here.
                </div>
              ) : (
                paletteMessages.map((msg, i) => (
                  <div key={i} className={`cmd-conv-msg cmd-conv-msg--${msg.role}`}>
                    <span className="cmd-conv-role">
                      {msg.role === 'user' ? 'You' : 'AI'}
                    </span>
                    <span className="cmd-conv-text">
                      {msg.text || (
                        <span className="cmd-conv-cursor" aria-hidden="true">▌</span>
                      )}
                    </span>
                  </div>
                ))
              )}
              <div ref={convEndRef} />
            </div>

            <div className="cmd-mode-bar" onClick={e => e.stopPropagation()}>
              <ModeSwitcher mode={paletteMode} onSelect={selectPaletteMode} />
              <ChannelStatusIcons channels={channelStatus} />
              <div className="cmd-mode-hints">
                <span className="cmd-footer-hint">
                  <kbd className="cmd-kbd">@</kbd>mention
                </span>
                <span className="cmd-footer-hint">
                  <kbd className="cmd-kbd">↵</kbd>send
                </span>
                <span className="cmd-footer-hint">
                  <kbd className="cmd-kbd">⇧Tab</kbd>switch
                </span>
                <span className="cmd-footer-hint">
                  <kbd className="cmd-kbd">esc</kbd>close
                </span>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Portal — rendered from AppInner so it has direct access to conv data
// ---------------------------------------------------------------------------

interface CommandPalettePortalProps {
  onOpenModule: (id: string) => void
  conversationBridge: ConversationBridge | null
  actionBridge: ActionBridge | null
}

export function CommandPalettePortal({ onOpenModule, conversationBridge, actionBridge }: CommandPalettePortalProps) {
  const { isOpen, initialQuery, initialMode, close } = useCommandPalette()

  if (!isOpen) return null

  return createPortal(
    <CommandPaletteOverlay
      initialQuery={initialQuery}
      initialMode={initialMode}
      onClose={close}
      onOpenModule={onOpenModule}
      conversationBridge={conversationBridge}
      actionBridge={actionBridge}
    />,
    document.body,
  )
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

interface CommandPaletteProviderProps {
  children: React.ReactNode
}

export function CommandPaletteProvider({ children }: CommandPaletteProviderProps) {
  const [isOpen, setIsOpen] = useState(false)
  const [initialQuery, setInitialQuery] = useState('')
  const [initialMode, setInitialMode] = useState<PaletteMode>('command')

  const open = useCallback(() => {
    setInitialQuery('')
    setInitialMode('command')
    setIsOpen(true)
  }, [])
  const openConversation = useCallback((query = '') => {
    setInitialQuery(query)
    setInitialMode('conversation')
    setIsOpen(true)
  }, [])
  const close = useCallback((_skipHide?: boolean) => {
    setIsOpen(false)
    if (!_skipHide) setFullClickThrough()
  }, [])
  const toggle = useCallback(() => {
    setInitialQuery('')
    setInitialMode('command')
    setIsOpen(prev => !prev)
  }, [])
  const openWithQuery = useCallback((q: string) => {
    setInitialQuery(q)
    setInitialMode('command')
    setIsOpen(true)
  }, [])

  // Ensure click-through is fully disabled whenever the palette closes.
  // Handles the hotkey-toggle-close path where toggle() doesn't call setFullClickThrough().
  useEffect(() => {
    if (!isOpen) setFullClickThrough()
  }, [isOpen])

  useEffect(() => {
    function handler(e: KeyboardEvent) {
      if (e.key === ' ' && e.altKey) {
        e.preventDefault()
        toggle()
      }
    }
    document.addEventListener('keydown', handler)
    return () => document.removeEventListener('keydown', handler)
  }, [openConversation, toggle])

  // Listen for toggle event from Go (double-tap Option / tray).
  useEffect(() => {
    const runtime = wailsRuntime()
    if (!runtime) return
    runtime.EventsOn('nanika:toggle-palette', toggle)
    return () => runtime.EventsOff('nanika:toggle-palette')
  }, [toggle])

  // Listen for dismiss event from Go (click-outside via global mouse monitor).
  // Uses a dedicated event rather than toggle so clicking outside a closed
  // palette doesn't accidentally re-open it.
  useEffect(() => {
    const runtime = wailsRuntime()
    if (!runtime) return
    runtime.EventsOn('nanika:dismiss-palette', close)
    return () => runtime.EventsOff('nanika:dismiss-palette')
  }, [close])

  return (
    <CommandPaletteContext.Provider
      value={{ isOpen, initialQuery, initialMode, open, openConversation, close, toggle, openWithQuery }}
    >
      {children}
    </CommandPaletteContext.Provider>
  )
}
