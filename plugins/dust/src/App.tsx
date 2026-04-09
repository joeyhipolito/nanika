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
  useRef,
  useState,
} from 'react'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { ComponentRenderer } from './ComponentRenderer'
import type { ActionHandler } from './ComponentRenderer'
import type { Component } from './types'

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
// App — root shell
// ---------------------------------------------------------------------------

export function App() {
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<CapabilityMatch[]>([])
  const [selectedIndex, setSelectedIndex] = useState(0)
  const [pluginInfo, setPluginInfo] = useState<PluginInfo | null>(null)
  const [detail, setDetail] = useState<Component | null>(null)
  const [isLoadingDetail, setIsLoadingDetail] = useState(false)
  const [showActionPalette, setShowActionPalette] = useState(false)
  const inputRef = useRef<HTMLInputElement>(null)

  // ------------------------------------------------------------------
  // Search — debounced 80 ms to avoid hammering IPC on every keystroke
  // ------------------------------------------------------------------
  useEffect(() => {
    const t = setTimeout(() => {
      invoke<CapabilityMatch[]>('search_capabilities', { query })
        .then(r => {
          setResults(r)
          setSelectedIndex(0)
        })
        .catch(console.error)
    }, 80)
    return () => clearTimeout(t)
  }, [query])

  // ------------------------------------------------------------------
  // Detail pane — reload when selected result changes
  // ------------------------------------------------------------------
  useEffect(() => {
    const selected = results[selectedIndex]
    if (!selected) {
      setPluginInfo(null)
      setDetail(null)
      return
    }

    let cancelled = false
    setIsLoadingDetail(true)

    Promise.all([
      invoke<PluginInfo>('get_plugin_info', { pluginId: selected.plugin_id }),
      invoke<Component>('render_ui', {
        pluginId: selected.plugin_id,
        capabilityId: selected.capability.id,
        query,
      }).catch(() => null),
    ])
      .then(([info, comp]) => {
        if (cancelled) return
        setPluginInfo(info)
        setDetail(comp)
      })
      .catch(() => {
        if (cancelled) return
        setPluginInfo(null)
        setDetail(null)
      })
      .finally(() => {
        if (!cancelled) setIsLoadingDetail(false)
      })

    return () => {
      cancelled = true
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedIndex, results])

  // ------------------------------------------------------------------
  // Hide on blur, re-focus input on show
  // ------------------------------------------------------------------
  useEffect(() => {
    const win = getCurrentWindow()
    let unlistenBlur: (() => void) | null = null
    let unlistenFocus: (() => void) | null = null

    win
      .onFocusChanged(({ payload: focused }) => {
        if (!focused) win.hide().catch(console.error)
      })
      .then(f => {
        unlistenBlur = f
      })

    win
      .listen('tauri://focus', () => {
        inputRef.current?.focus()
        inputRef.current?.select()
      })
      .then(f => {
        unlistenFocus = f
      })

    return () => {
      unlistenBlur?.()
      unlistenFocus?.()
    }
  }, [])

  // ------------------------------------------------------------------
  // Keyboard navigation
  // ------------------------------------------------------------------
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.ctrlKey && e.key === 'k') {
        e.preventDefault()
        setShowActionPalette(v => !v)
        return
      }

      switch (e.key) {
        case 'ArrowDown':
          e.preventDefault()
          setShowActionPalette(false)
          setSelectedIndex(i => Math.min(i + 1, results.length - 1))
          break
        case 'ArrowUp':
          e.preventDefault()
          setShowActionPalette(false)
          setSelectedIndex(i => Math.max(i - 1, 0))
          break
        case 'Enter':
          e.preventDefault()
          // Enter confirms the selection; detail already loaded — nothing extra to do.
          // Close palette if open.
          setShowActionPalette(false)
          break
        case 'Escape':
          e.preventDefault()
          if (showActionPalette) {
            setShowActionPalette(false)
          } else {
            setQuery('')
            getCurrentWindow().hide().catch(console.error)
          }
          break
      }
    },
    [results.length, showActionPalette],
  )

  // ------------------------------------------------------------------
  // Action dispatch — forwarded to plugin via IPC
  // ------------------------------------------------------------------
  const handleAction: ActionHandler = useCallback(
    async (action, id) => {
      const selected = results[selectedIndex]
      if (!selected) return
      await invoke('dispatch_action', {
        pluginId: selected.plugin_id,
        capabilityId: selected.capability.id,
        actionId: action,
        params: id ? { id } : {},
      })
    },
    [results, selectedIndex],
  )

  return (
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
        onChange={setQuery}
        onKeyDown={handleKeyDown}
      />

      {/* ── Body ─────────────────────────────────────────────────── */}
      <div
        className="flex flex-1 overflow-hidden"
        style={{ borderTop: '1px solid var(--border)' }}
      >
        <ResultsList
          results={results}
          selectedIndex={selectedIndex}
          onSelect={setSelectedIndex}
        />
        <DetailPane
          pluginInfo={pluginInfo}
          detail={detail}
          isLoading={isLoadingDetail}
          onAction={handleAction}
        />
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
          hasSelection={results[selectedIndex] != null}
          showPalette={showActionPalette}
          onTogglePalette={() => setShowActionPalette(v => !v)}
        />
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
}

const SearchBar = forwardRef<HTMLInputElement, SearchBarProps>(
  ({ value, onChange, onKeyDown }, ref) => (
    <div className="flex h-[52px] shrink-0 items-center gap-3 px-4">
      {/* Search icon */}
      <svg
        aria-hidden="true"
        width="14"
        height="14"
        viewBox="0 0 16 16"
        fill="none"
        className="shrink-0"
        style={{ color: 'var(--text-secondary)' }}
      >
        <circle cx="6.5" cy="6.5" r="5" stroke="currentColor" strokeWidth="1.5" />
        <line x1="10.5" y1="10.5" x2="14.5" y2="14.5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
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
        placeholder="Search capabilities…"
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
  results: CapabilityMatch[]
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
        results.map((r, i) => (
          <ResultItem
            key={`${r.plugin_id}/${r.capability.id}`}
            match={r}
            selected={i === selectedIndex}
            onClick={() => onSelect(i)}
          />
        ))
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
  return (
    <button
      type="button"
      role="option"
      aria-selected={selected}
      onClick={onClick}
      className="flex w-full flex-col gap-0.5 px-3 py-2 text-left transition-colors"
      style={{
        background: selected ? 'var(--selected-bg)' : 'transparent',
      }}
    >
      <div className="flex items-center gap-2">
        <span
          aria-hidden="true"
          className="h-1.5 w-1.5 shrink-0 rounded-full"
          style={{
            background: selected ? 'var(--color-accent)' : 'var(--text-secondary)',
          }}
        />
        <span
          className="truncate text-xs font-semibold"
          style={{ color: 'var(--text-primary)' }}
        >
          {match.capability.name}
        </span>
      </div>
      <span
        className="truncate pl-3.5 text-[11px]"
        style={{ color: 'var(--text-secondary)' }}
      >
        {match.plugin_name}
      </span>
    </button>
  )
}

// ---------------------------------------------------------------------------
// DetailPane — PluginInfo + ComponentRenderer output
// ---------------------------------------------------------------------------

type DetailPaneProps = {
  pluginInfo: PluginInfo | null
  detail: Component | null
  isLoading: boolean
  onAction: ActionHandler
}

function DetailPane({ pluginInfo, detail, isLoading, onAction }: DetailPaneProps) {
  if (!pluginInfo) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
          Select a capability to preview
        </p>
      </div>
    )
  }

  return (
    <div className="flex flex-1 flex-col overflow-hidden">
      <PluginInfoPanel info={pluginInfo} />

      <div style={{ height: 1, background: 'var(--border)', flexShrink: 0 }} />

      <div className="flex-1 overflow-y-auto p-4">
        {isLoading ? (
          <p
            className="animate-pulse text-xs"
            style={{ color: 'var(--text-secondary)' }}
          >
            Loading…
          </p>
        ) : detail ? (
          <ComponentRenderer component={detail} onAction={onAction} />
        ) : (
          <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
            Plugin output will appear here.
          </p>
        )}
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
}

function ActionBar({ hasSelection, showPalette, onTogglePalette }: ActionBarProps) {
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
        <ShortcutHint keys={['esc']} label="hide" />
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
