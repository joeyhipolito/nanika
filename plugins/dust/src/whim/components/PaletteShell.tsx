import type { CSSProperties, ReactNode } from 'react'
import { Icon } from '../icons/Icon'

// ─── Public types ──────────────────────────────────────────────────────────────

export type ShellScale = 'pill' | 'palette' | 'detail' | 'canvas'
export type PillMode   = 'idle' | 'hover' | 'type' | 'voice'

export interface ResultItem {
  id: string
  icon?: ReactNode
  name: ReactNode
  meta?: string
  selected?: boolean
}

export interface ResultSection {
  label: string
  items: ResultItem[]
}

export interface RunLogRow {
  id: string
  done: boolean
  live?: boolean
  persona: string
  phase: string
  duration?: string
}

export interface RailItem {
  id: string
  label: string
  meta?: string
  working?: boolean
  active?: boolean
}

export interface HintEntry {
  keys: string[]
  label: string
}

export interface PaletteShellProps {
  scale: ShellScale
  warmBleed?: boolean
  // L0 pill
  pillMode?: PillMode
  recording?: boolean
  convCount?: number
  // Input row
  query?: string
  placeholder?: string
  // Results (L1, L2)
  sections?: ResultSection[]
  // Detail pane (L2)
  detailContent?: ReactNode
  breadcrumb?: string
  // Canvas (L3)
  railItems?: RailItem[]
  mainContent?: ReactNode
  runLog?: RunLogRow[]
  composerPlaceholder?: string
  // Footer hints
  hints?: HintEntry[]
  // Callbacks
  onQueryChange?: (value: string) => void
  onResultSelect?: (id: string) => void
  onSubmit?: () => void
  onEsc?: () => void
}

// ─── Shared style constants ────────────────────────────────────────────────────

const GLASS: CSSProperties = {
  background:              'var(--glass)',
  backdropFilter:          'blur(40px)',
  WebkitBackdropFilter:    'blur(40px)',
  border:                  '0.5px solid var(--rim)',
  overflow:                'hidden',
  position:                'relative',
  width:                   '820px',
}

const PILL_DIMENSIONS: Record<PillMode, { width: number; height: number; borderRadius: number; padding: string }> = {
  idle:  { width: 63,  height: 9,  borderRadius: 5,  padding: '0' },
  hover: { width: 126, height: 36, borderRadius: 18, padding: '0 8px' },
  voice: { width: 126, height: 36, borderRadius: 18, padding: '0 8px' },
  type:  { width: 540, height: 40, borderRadius: 20, padding: '0 14px' },
}

const SHADOW_FLOAT = '0 40px 80px rgba(0,0,0,0.5), 0 12px 32px rgba(0,0,0,0.45), inset 0 0 0 0.5px rgba(255,255,255,0.04)'
const SHADOW_PILL  = '0 18px 40px rgba(0,0,0,0.45), inset 0 0 0 0.5px rgba(255,255,255,0.04)'

// ─── Breadcrumb separator (not in icon registry — inline only) ────────────────

function ChevronIcon() {
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden="true">
      <path d="M3.5 2L6.5 5L3.5 8" stroke="currentColor" strokeWidth="1.1"
        strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  )
}

// ─── Voice overlay (9-bin waveform) ───────────────────────────────────────────

function VoiceOverlay({ bins = 9 }: { bins?: number }) {
  return (
    <div
      aria-label="Voice recording in progress"
      style={{
        display:     'flex',
        alignItems:  'center',
        gap:         '4px',
        flex:        1,
        height:      '24px',
        paddingLeft: '2px',
      }}
    >
      {Array.from({ length: bins }).map((_, i) => (
        <span
          key={i}
          aria-hidden="true"
          style={{
            display:         'block',
            width:           '3px',
            height:          '24px',
            borderRadius:    '2px',
            background:      'var(--accent)',
            animation:       'wave-bar 0.55s ease-in-out infinite',
            animationDelay:  `${i * 0.06}s`,
            transformOrigin: 'center',
          }}
        />
      ))}
    </div>
  )
}

// ─── Decorative overlays ───────────────────────────────────────────────────────

function TopGlow() {
  return (
    <div aria-hidden="true" style={{
      position:      'absolute',
      inset:         0,
      pointerEvents: 'none',
      background:    'linear-gradient(180deg, rgba(255,255,255,0.03) 0%, transparent 30%)',
      zIndex:        0,
    }} />
  )
}

function WarmBleedLine() {
  return (
    <div aria-hidden="true" style={{
      position:      'absolute',
      left:          0,
      right:         0,
      top:           0,
      height:        '2px',
      background:    'linear-gradient(90deg, transparent, var(--accent) 40%, var(--accent-2) 60%, transparent)',
      opacity:       0.55,
      pointerEvents: 'none',
      zIndex:        1,
    }} />
  )
}

// ─── Shared building blocks ────────────────────────────────────────────────────

function PaletteInput({
  query,
  placeholder = 'Search Whim…',
  recording,
  onQueryChange,
  onSubmit,
  onEsc,
}: {
  query?: string
  placeholder?: string
  recording?: boolean
  onQueryChange?: (v: string) => void
  onSubmit?: () => void
  onEsc?: () => void
}) {
  const interactive = !!onQueryChange

  return (
    <div
      data-tour-anchor="palette-input"
      style={{
        display:      'flex',
        alignItems:   'center',
        gap:          '14px',
        padding:      '17px 22px',
        borderBottom: '0.5px solid var(--border-soft)',
      }}
    >
      {recording ? (
        <>
          <span aria-hidden="true" style={{
            width:        '8px',
            height:       '8px',
            borderRadius: '50%',
            background:   'var(--accent)',
            boxShadow:    '0 0 6px var(--accent)',
            flexShrink:   0,
            animation:    'pulse 1s ease-in-out infinite',
          }} />
          <VoiceOverlay bins={9} />
          <span style={{
            fontFamily:    'var(--mono)',
            fontSize:      '11px',
            color:         'var(--accent)',
            letterSpacing: '0.18em',
            textTransform: 'uppercase',
            flexShrink:    0,
          }}>
            Listening
          </span>
        </>
      ) : (
        <>
          <Icon name="Search" size={18} style={{ color: 'var(--faint)', flexShrink: 0 }} />
          {interactive ? (
            <input
              className="palette-input"
              autoFocus
              value={query ?? ''}
              onChange={e => onQueryChange!(e.target.value)}
              placeholder={placeholder}
              onKeyDown={e => {
                if (e.key === 'Enter') { e.preventDefault(); onSubmit?.() }
                if (e.key === 'Escape') { e.preventDefault(); onEsc?.() }
              }}
            />
          ) : (
            <span style={{
              flex:          1,
              fontSize:      '16px',
              color:         query ? 'var(--text)' : 'var(--faint)',
              letterSpacing: '-0.005em',
              userSelect:    'none',
            }}>
              {query || placeholder}
              {query && (
                <span aria-hidden="true" style={{
                  display:         'inline-block',
                  width:           '1.5px',
                  height:          '17px',
                  background:      'var(--accent)',
                  verticalAlign:   '-3px',
                  marginLeft:      '2px',
                  animation:       'blink 1.1s steps(1) infinite',
                }} />
              )}
            </span>
          )}
          <button
            type="button"
            aria-label="Voice input"
            onClick={() => onQueryChange?.('')}
            style={{
              width:          '28px',
              height:         '28px',
              borderRadius:   '50%',
              border:         '0.5px solid var(--border)',
              background:     'transparent',
              cursor:         'pointer',
              display:        'flex',
              alignItems:     'center',
              justifyContent: 'center',
              color:          'var(--muted)',
              padding:        0,
              flexShrink:     0,
            }}
          >
            <Icon name="Mic" size={14} variant={recording ? 'duotone' : 'line'} />
          </button>
        </>
      )}
    </div>
  )
}

function ResultsList({
  sections,
  onResultSelect,
}: {
  sections?: ResultSection[]
  onResultSelect?: (id: string) => void
}) {
  if (!sections?.length) {
    return (
      <div style={{ padding: '24px 22px', color: 'var(--faint)', fontSize: '13px', fontFamily: 'var(--mono)', letterSpacing: '0.04em' }}>
        Type to search…
      </div>
    )
  }

  return (
    <div data-tour-anchor="palette-results" role="listbox" style={{ padding: '6px 0' }}>
      {sections.map(sec => (
        <div key={sec.label}>
          <div style={{
            fontFamily:    'var(--mono)',
            fontSize:      '10.5px',
            letterSpacing: '0.2em',
            color:         'var(--faint)',
            textTransform: 'uppercase',
            padding:       '10px 22px 6px',
          }}>
            {sec.label}
          </div>
          {sec.items.map(item => (
            <div
              key={item.id}
              role="option"
              aria-selected={item.selected ?? false}
              tabIndex={0}
              onClick={() => onResultSelect?.(item.id)}
              onKeyDown={e => e.key === 'Enter' && onResultSelect?.(item.id)}
              style={{
                display:             'grid',
                gridTemplateColumns: '28px 1fr auto',
                alignItems:          'center',
                gap:                 '14px',
                padding:             item.selected ? '10px 22px 10px 20px' : '10px 22px',
                fontSize:            '14.5px',
                cursor:              'pointer',
                background:          item.selected
                  ? 'linear-gradient(90deg, var(--s2), rgba(28,29,36,0.4))'
                  : 'transparent',
                borderLeft: item.selected ? '2px solid var(--accent)' : '2px solid transparent',
              }}
            >
              <span style={{
                width:          '22px',
                height:         '22px',
                borderRadius:   '5px',
                background:     'var(--s2)',
                border:         '0.5px solid var(--border)',
                display:        'grid',
                placeItems:     'center',
                color:          'var(--muted)',
                fontSize:       '10px',
                flexShrink:     0,
              }}>
                {item.icon ?? '⚡'}
              </span>
              <span style={{ color: 'var(--text)', fontWeight: 450 }}>{item.name}</span>
              {item.meta && (
                <span style={{ fontFamily: 'var(--mono)', fontSize: '11.5px', color: 'var(--faint)', letterSpacing: '0.04em' }}>
                  {item.meta}
                </span>
              )}
            </div>
          ))}
        </div>
      ))}
    </div>
  )
}

const DEFAULT_HINTS: HintEntry[] = [
  { keys: ['↑', '↓'], label: 'Navigate' },
  { keys: ['↵'],      label: 'Select' },
  { keys: ['Esc'],    label: 'Close' },
]

function HintBar({ hints }: { hints?: HintEntry[] }) {
  const rows = hints ?? DEFAULT_HINTS
  return (
    <div style={{
      display:    'flex',
      alignItems: 'center',
      gap:        '18px',
      padding:    '10px 20px',
      borderTop:  '0.5px solid var(--border-soft)',
      fontSize:   '12.5px',
    }}>
      {rows.map((h, i) => (
        <span key={i} style={{ display: 'inline-flex', alignItems: 'center', gap: '4px' }}>
          {h.keys.map((k, j) => (
            <span key={j} className="K nav">{k}</span>
          ))}
          <span style={{ color: 'var(--faint)', marginLeft: '3px' }}>{h.label}</span>
        </span>
      ))}
    </div>
  )
}

function LeftRail({ items }: { items?: RailItem[] }) {
  return (
    <nav aria-label="Mission list" style={{
      width:         '180px',
      minWidth:      '180px',
      background:    'rgba(16,17,22,0.8)',
      borderRight:   '0.5px solid var(--border)',
      padding:       '12px 10px',
      display:       'flex',
      flexDirection: 'column',
      gap:           '2px',
      overflowY:     'auto',
    }}>
      <div style={{
        fontFamily:    'var(--mono)',
        fontSize:      '10.5px',
        letterSpacing: '0.18em',
        color:         'var(--faint)',
        textTransform: 'uppercase',
        padding:       '8px 8px 4px',
      }}>
        Missions
      </div>
      {items?.map(item => (
        <div
          key={item.id}
          style={{
            padding:      '7px 10px',
            borderRadius: '6px',
            fontSize:     '12.5px',
            color:        item.active ? 'var(--text)' : 'var(--muted)',
            background:   item.active ? 'var(--s2)' : 'transparent',
            display:      'flex',
            alignItems:   'center',
            gap:          '10px',
            cursor:       'pointer',
          }}
        >
          <span style={{
            width:        '6px',
            height:       '6px',
            borderRadius: '50%',
            flexShrink:   0,
            background:   item.working ? 'var(--green)' : item.active ? 'var(--muted)' : 'var(--ghost)',
            boxShadow:    item.working ? '0 0 6px var(--green)' : 'none',
            animation:    item.working ? 'pulse 1.6s ease-in-out infinite' : 'none',
          }} />
          <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
            {item.label}
          </span>
          {item.meta && (
            <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--faint)' }}>
              {item.meta}
            </span>
          )}
        </div>
      ))}
    </nav>
  )
}

function RunLog({ rows }: { rows: RunLogRow[] }) {
  return (
    <div style={{ fontFamily: 'var(--mono)', fontSize: '13px', lineHeight: 1.9 }}>
      {rows.map(row => (
        <div key={row.id} style={{
          display:             'grid',
          gridTemplateColumns: '18px 150px 1fr auto',
          gap:                 '12px',
          alignItems:          'baseline',
        }}>
          <span style={{ color: row.done ? 'var(--green)' : row.live ? 'var(--accent)' : 'var(--muted)' }}>
            {row.done ? '✓' : '○'}
          </span>
          <span style={{ color: 'var(--text)' }}>{row.persona}</span>
          <span style={{ color: 'var(--muted)' }}>{row.phase}</span>
          {row.duration && (
            <span style={{ color: 'var(--faint)', fontVariantNumeric: 'tabular-nums' }}>
              {row.duration}
            </span>
          )}
        </div>
      ))}
    </div>
  )
}

// ─── Scale renderers ────────────────────────────────────────────────────────────

function PillShell({ pillMode = 'idle', recording, convCount, query, placeholder = 'Search Whim…' }: PaletteShellProps) {
  const dims   = PILL_DIMENSIONS[pillMode]
  const isIdle = pillMode === 'idle'
  const active = pillMode === 'hover' || pillMode === 'type'
  const voice  = pillMode === 'voice'

  return (
    <div
      data-tour-anchor="pill-capsule"
      role="search"
      aria-label="Whim"
      style={{
        background:           GLASS.background,
        backdropFilter:       GLASS.backdropFilter,
        WebkitBackdropFilter: GLASS.WebkitBackdropFilter,
        border:               GLASS.border,
        overflow:             GLASS.overflow,
        position:             GLASS.position,
        width:                dims.width,
        height:               dims.height,
        borderRadius:         dims.borderRadius,
        padding:              dims.padding,
        boxSizing:            'border-box',
        boxShadow:            SHADOW_PILL,
        transition:           'width 0.25s ease, height 0.25s ease, border-radius 0.25s ease',
        display:              'flex',
        alignItems:           'center',
      }}
    >
      {!isIdle && <TopGlow />}
      <div
        style={{
          position:   'relative',
          zIndex:     2,
          width:      '100%',
          height:     '100%',
          display:    'flex',
          alignItems: 'center',
          opacity:    isIdle ? 0 : 1,
          transition: 'opacity 0.15s ease 0.18s',
        }}
      >
        {!isIdle && (
          <div data-tour-anchor="pill-hover-row" style={{ display: 'flex', alignItems: 'center', gap: '8px', width: '100%' }}>
            {voice ? (
              <>
                <div data-tour-anchor="pill-voice-waveform" style={{ display: 'flex', alignItems: 'center', gap: '3px', height: '22px', flex: 1, justifyContent: 'center' }}>
                  {[0, 1, 2, 3, 4].map(i => (
                    <span key={i} aria-hidden="true" style={{
                      display:         'block',
                      width:           '2px',
                      height:          '22px',
                      background:      'var(--text)',
                      borderRadius:    '2px',
                      animation:       'wave-bar 0.8s ease-in-out infinite',
                      animationDelay:  `${i * 0.12}s`,
                      transformOrigin: 'center',
                    }} />
                  ))}
                </div>
                <span style={{
                  fontFamily:    'var(--mono)',
                  fontSize:      '10px',
                  color:         recording ? 'var(--accent)' : 'var(--faint)',
                  letterSpacing: '0.16em',
                  textTransform: 'uppercase',
                  flexShrink:    0,
                }}>
                  {recording ? 'Listening' : 'Standby'}
                </span>
              </>
            ) : active ? (
              <>
                <Icon name="Search" size={16} style={{ color: 'var(--faint)', flexShrink: 0 }} />
                <span style={{
                  flex:          1,
                  fontSize:      '13px',
                  color:         query ? 'var(--text)' : 'var(--faint)',
                  letterSpacing: '-0.005em',
                  userSelect:    'none',
                  overflow:      'hidden',
                  whiteSpace:    'nowrap',
                  textOverflow:  'ellipsis',
                }}>
                  {query || placeholder}
                  {pillMode === 'type' && (
                    <span aria-hidden="true" style={{
                      display:       'inline-block',
                      width:         '1.5px',
                      height:        '14px',
                      background:    'var(--accent)',
                      verticalAlign: '-2px',
                      marginLeft:    '2px',
                      animation:     'blink 1.1s steps(1) infinite',
                    }} />
                  )}
                </span>
                {convCount !== undefined && convCount > 0 && (
                  <span style={{
                    width:        '18px',
                    height:       '18px',
                    borderRadius: '50%',
                    background:   'var(--s2)',
                    border:       '0.5px solid var(--border)',
                    display:      'grid',
                    placeItems:   'center',
                    fontSize:     '10px',
                    fontFamily:   'var(--mono)',
                    color:        'var(--faint)',
                    flexShrink:   0,
                  }}>
                    {convCount}
                  </span>
                )}
                <button
                  type="button"
                  aria-label="Voice input"
                  style={{
                    width:          '22px',
                    height:         '22px',
                    borderRadius:   '50%',
                    border:         '0.5px solid var(--border)',
                    background:     'transparent',
                    cursor:         'pointer',
                    display:        'flex',
                    alignItems:     'center',
                    justifyContent: 'center',
                    color:          'var(--muted)',
                    padding:        0,
                    flexShrink:     0,
                  }}
                >
                  <Icon name="Mic" size={12} />
                </button>
              </>
            ) : null}
          </div>
        )}
      </div>
    </div>
  )
}

function PaletteShellL1({
  query, placeholder, sections, hints, warmBleed = true,
  recording, onQueryChange, onResultSelect, onSubmit, onEsc,
}: PaletteShellProps) {
  return (
    <div data-tour-anchor="shell-frame" style={{ ...GLASS, borderRadius: '14px', boxShadow: SHADOW_FLOAT }} role="dialog" aria-label="Command palette">
      {warmBleed && <WarmBleedLine />}
      <TopGlow />
      <div style={{ position: 'relative', zIndex: 2 }}>
        <PaletteInput
          query={query}
          placeholder={placeholder}
          recording={recording}
          onQueryChange={onQueryChange}
          onSubmit={onSubmit}
          onEsc={onEsc}
        />
        <div style={{ maxHeight: '400px', overflowY: 'auto' }}>
          <ResultsList sections={sections} onResultSelect={onResultSelect} />
        </div>
        <HintBar hints={hints} />
      </div>
    </div>
  )
}

function PaletteShellL2({
  query, placeholder, sections, hints, detailContent, breadcrumb, warmBleed = true,
  recording, onQueryChange, onResultSelect, onSubmit, onEsc,
}: PaletteShellProps) {
  return (
    <div style={{ ...GLASS, borderRadius: '14px', boxShadow: SHADOW_FLOAT }} role="dialog" aria-label="Command palette">
      {warmBleed && <WarmBleedLine />}
      <TopGlow />
      <div style={{ position: 'relative', zIndex: 2 }}>
        <PaletteInput
          query={query}
          placeholder={placeholder}
          recording={recording}
          onQueryChange={onQueryChange}
          onSubmit={onSubmit}
          onEsc={onEsc}
        />
        <div style={{ display: 'flex', maxHeight: '400px' }}>
          <div style={{ width: '280px', minWidth: '280px', borderRight: '0.5px solid var(--border-soft)', overflowY: 'auto' }}>
            <ResultsList sections={sections} onResultSelect={onResultSelect} />
          </div>
          <div data-tour-anchor="palette-detail-pane" style={{ flex: 1, overflowY: 'auto', padding: '16px 20px' }}>
            {breadcrumb && (
              <div style={{
                display:       'flex',
                alignItems:    'center',
                gap:           '5px',
                fontFamily:    'var(--mono)',
                fontSize:      '11px',
                color:         'var(--faint)',
                letterSpacing: '0.1em',
                textTransform: 'uppercase',
                marginBottom:  '14px',
              }}>
                {breadcrumb.split(' › ').map((seg, i, arr) => (
                  <span key={i} style={{ display: 'inline-flex', alignItems: 'center', gap: '5px' }}>
                    <span style={{ color: i === arr.length - 1 ? 'var(--muted)' : 'var(--faint)' }}>{seg}</span>
                    {i < arr.length - 1 && <ChevronIcon />}
                  </span>
                ))}
              </div>
            )}
            {detailContent ?? (
              <span style={{ color: 'var(--faint)', fontSize: '13px', fontFamily: 'var(--mono)' }}>
                Select an item to preview
              </span>
            )}
          </div>
        </div>
        <HintBar hints={hints} />
      </div>
    </div>
  )
}

function PaletteShellL3({
  breadcrumb = 'whim-palette · phase-3',
  railItems,
  mainContent,
  runLog,
  composerPlaceholder = 'Ask anything, @tag files, or use / for commands…',
}: PaletteShellProps) {
  return (
    <div style={{ ...GLASS, borderRadius: '14px', boxShadow: SHADOW_FLOAT }} role="main" aria-label="Mission canvas">
      <WarmBleedLine />
      <TopGlow />
      <div style={{ position: 'relative', zIndex: 2, display: 'flex', flexDirection: 'column' }}>
        <div style={{
          display:      'flex',
          alignItems:   'center',
          gap:          '14px',
          padding:      '12px 16px',
          borderBottom: '0.5px solid var(--border-soft)',
          minHeight:    '40px',
        }}>
          <div style={{ display: 'flex', gap: '7px' }} aria-hidden="true">
            {[0, 1, 2].map(i => (
              <span key={i} style={{ width: '12px', height: '12px', borderRadius: '50%', background: 'var(--s2)', border: '0.5px solid var(--border)' }} />
            ))}
          </div>
          <span style={{ fontFamily: 'var(--mono)', fontSize: '12px', color: 'var(--muted)', letterSpacing: '0.04em' }}>
            {breadcrumb}
          </span>
        </div>

        <div style={{ display: 'flex', height: '360px' }}>
          <LeftRail items={railItems} />
          <div style={{ flex: 1, overflowY: 'auto', padding: '20px 24px' }}>
            {mainContent ?? (runLog ? <RunLog rows={runLog} /> : (
              <span style={{ color: 'var(--faint)', fontSize: '13px', fontFamily: 'var(--mono)' }}>
                No active mission
              </span>
            ))}
          </div>
        </div>

        <div style={{
          borderTop:  '0.5px solid var(--border-soft)',
          padding:    '12px 16px',
          display:    'flex',
          alignItems: 'center',
          gap:        '10px',
        }}>
          <span style={{ flex: 1, fontSize: '14px', color: 'var(--faint)', userSelect: 'none' }}>
            {composerPlaceholder}
          </span>
          <button
            type="button"
            aria-label="Send message"
            style={{
              width:          '28px',
              height:         '28px',
              borderRadius:   '6px',
              background:     'var(--blue)',
              border:         'none',
              cursor:         'pointer',
              display:        'flex',
              alignItems:     'center',
              justifyContent: 'center',
              color:          '#fff',
              padding:        0,
              flexShrink:     0,
            }}
          >
            <Icon name="Send" size={14} />
          </button>
        </div>

        <div style={{
          borderTop:      '0.5px solid var(--border-soft)',
          padding:        '6px 16px',
          display:        'flex',
          justifyContent: 'space-between',
          alignItems:     'center',
          fontFamily:     'var(--mono)',
          fontSize:       '11px',
          color:          'var(--faint)',
          letterSpacing:  '0.06em',
        }}>
          <span style={{ display: 'flex', alignItems: 'center', gap: '6px', color: 'var(--green)' }}>
            <span style={{
              width:        '6px',
              height:       '6px',
              borderRadius: '50%',
              background:   'var(--green)',
              boxShadow:    '0 0 5px var(--green)',
              animation:    'pulse 1.6s ease-in-out infinite',
              display:      'inline-block',
            }} />
            Working · 2m 14s
          </span>
          <div style={{ display: 'flex', alignItems: 'center', gap: '10px' }}>
            <span>feature/whim-palette</span>
            <span style={{
              padding:      '1px 7px',
              borderRadius: '3px',
              border:       '0.5px solid rgba(34,197,94,0.35)',
              color:        'var(--green)',
              fontSize:     '10px',
            }}>
              healthy
            </span>
          </div>
        </div>
      </div>
    </div>
  )
}

// ─── Main export ───────────────────────────────────────────────────────────────

export function PaletteShell(props: PaletteShellProps) {
  switch (props.scale) {
    case 'pill':    return <PillShell      {...props} />
    case 'palette': return <PaletteShellL1 {...props} />
    case 'detail':  return <PaletteShellL2 {...props} />
    case 'canvas':  return <PaletteShellL3 {...props} />
  }
}
