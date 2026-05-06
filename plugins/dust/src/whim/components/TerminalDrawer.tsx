"use client"
import { useState, useEffect } from 'react'
import { MOCK_TERMINAL_LINES, MOCK_TERMINAL_PROMPT, type TerminalLine, type TerminalPrompt } from '../mocks/terminal'
import { Icon } from '../icons/Icon'

// ─── Types ────────────────────────────────────────────────────────────────────

interface TerminalDrawerProps {
  defaultOpen?: boolean
  onClose?:     () => void
  /** Lines to render. Defaults to MOCK_TERMINAL_LINES so existing mock scenes are unaffected. */
  lines?:       TerminalLine[]
  /** Prompt line. Defaults to MOCK_TERMINAL_PROMPT. */
  prompt?:      TerminalPrompt
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

function lineColor(kind: 'cmd' | 'stdout' | 'stderr' | 'info'): string {
  switch (kind) {
    case 'cmd':    return 'var(--accent-2)'
    case 'stdout': return 'var(--text)'
    case 'stderr': return 'var(--red)'
    case 'info':   return 'var(--muted)'
  }
}

function linePrefix(kind: 'cmd' | 'stdout' | 'stderr' | 'info'): string {
  switch (kind) {
    case 'cmd':    return '$ '
    case 'stderr': return '! '
    default:       return '  '
  }
}

// ─── TerminalDrawer ───────────────────────────────────────────────────────────

export function TerminalDrawer({
  defaultOpen = false,
  onClose,
  lines  = MOCK_TERMINAL_LINES,
  prompt = MOCK_TERMINAL_PROMPT,
}: TerminalDrawerProps) {
  const [open, setOpen] = useState(defaultOpen)
  const [timestamp, setTimestamp] = useState(() => {
    const d = new Date()
    return `[${String(d.getHours()).padStart(2,'0')}:${String(d.getMinutes()).padStart(2,'0')}:${String(d.getSeconds()).padStart(2,'0')}]`
  })

  useEffect(() => {
    const id = setInterval(() => {
      const d = new Date()
      setTimestamp(`[${String(d.getHours()).padStart(2,'0')}:${String(d.getMinutes()).padStart(2,'0')}:${String(d.getSeconds()).padStart(2,'0')}]`)
    }, 1000)
    return () => clearInterval(id)
  }, [])

  if (!open) {
    return (
      <div style={{
        display:        'flex',
        alignItems:     'center',
        gap:            '10px',
        padding:        '5px 14px',
        background:     'var(--s1)',
        borderTop:      '0.5px solid var(--border)',
        fontFamily:     'var(--mono)',
        fontSize:       '11px',
        color:          'var(--faint)',
      }}>
        <button
          onClick={() => setOpen(true)}
          style={{
            background:   'none',
            border:       'none',
            cursor:       'pointer',
            fontFamily:   'var(--mono)',
            fontSize:     '11px',
            color:        'var(--muted)',
            padding:      '2px 6px',
            borderRadius: '4px',
          }}
        >
          Terminal
        </button>
        <span style={{ color: 'var(--ghost)' }}>·</span>
        {/* ⌘J to toggle terminal */}
        <span style={{ color: 'var(--ghost)' }}>⌘J to open</span>
      </div>
    )
  }

  const p = prompt

  return (
    <div style={{
      display:       'flex',
      flexDirection: 'column',
      background:    'var(--s0)',
      borderTop:     '0.5px solid var(--border)',
      height:        '260px',
      flexShrink:    0,
    }}>
      {/* Header */}
      <div style={{
        display:        'flex',
        alignItems:     'center',
        justifyContent: 'space-between',
        padding:        '0 12px',
        height:         '34px',
        borderBottom:   '0.5px solid var(--border)',
        background:     'var(--s1)',
        flexShrink:     0,
      }}>
        {/* Title + ⌘J hint */}
        <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
          <span style={{
            fontFamily:    'var(--mono)',
            fontSize:      '11px',
            fontWeight:    600,
            color:         'var(--text)',
            letterSpacing: '0.04em',
          }}>
            Terminal
          </span>
          <span style={{
            fontFamily:    'var(--mono)',
            fontSize:      '10px',
            color:         'var(--ghost)',
            letterSpacing: '0.06em',
          }}>
            ⌘J
          </span>
        </div>

        {/* Tab-management glyphs */}
        <div style={{ display: 'flex', alignItems: 'center', gap: '2px' }}>
          <HeaderGlyph title="Split terminal"><Icon name="SplitWindow" size={13} /></HeaderGlyph>
          <HeaderGlyph title="New tab"><Icon name="NewTab" size={13} /></HeaderGlyph>
          <HeaderGlyph title="Close all tabs"><Icon name="Trash" size={13} /></HeaderGlyph>
          <div style={{ width: '1px', height: '14px', background: 'var(--border)', margin: '0 4px' }} />
          <HeaderGlyph
            title="Close terminal"
            onClick={() => {
              setOpen(false)
              onClose?.()
            }}
          >
            <Icon name="Close" size={13} />
          </HeaderGlyph>
        </div>
      </div>

      {/* Body */}
      <div style={{
        flex:       1,
        overflowY:  'auto',
        padding:    '10px 14px',
        fontFamily: 'var(--mono)',
        fontSize:   '12px',
        lineHeight: '1.7',
      }}>
        {lines.map((line, i) => (
          <div key={i} style={{ color: lineColor(line.kind), whiteSpace: 'pre' }}>
            <span style={{ color: 'var(--ghost)', userSelect: 'none' }}>{linePrefix(line.kind)}</span>
            {line.text}
          </div>
        ))}

        {/* Live prompt line */}
        <div style={{
          display:        'flex',
          alignItems:     'baseline',
          justifyContent: 'space-between',
          marginTop:      '4px',
          gap:            '8px',
        }}>
          <div style={{ display: 'flex', alignItems: 'baseline', gap: '0', whiteSpace: 'pre', flexWrap: 'nowrap' }}>
            {/* user@host */}
            <span style={{ color: 'var(--green)' }}>{p.user}@{p.host}</span>
            <span style={{ color: 'var(--text)' }}> </span>
            {/* path */}
            <span style={{ color: 'var(--blue)' }}>{p.path}</span>
            <span style={{ color: 'var(--text)' }}> </span>
            {/* branch */}
            <span style={{ color: 'var(--accent)' }}>⎇ {p.branch}</span>
            <span style={{ color: 'var(--muted)' }}> : {p.gitStatus}</span>
            {/* prompt + cursor */}
            <span style={{ color: 'var(--text)' }}> &gt; </span>
            <span style={{
              display:         'inline-block',
              width:           '8px',
              height:          '14px',
              background:      'var(--text)',
              verticalAlign:   'text-bottom',
              animation:       'blink 1.1s step-start infinite',
            }} />
          </div>

          {/* Right-edge timestamp */}
          <span style={{
            color:              'var(--ghost)',
            fontSize:           '11px',
            fontVariantNumeric: 'tabular-nums',
            flexShrink:         0,
          }}>
            {timestamp}
          </span>
        </div>
      </div>
    </div>
  )
}

// ─── HeaderGlyph ──────────────────────────────────────────────────────────────

function HeaderGlyph({
  children,
  title,
  onClick,
}: {
  children: React.ReactNode
  title: string
  onClick?: () => void
}) {
  return (
    <button
      title={title}
      onClick={onClick}
      style={{
        display:      'flex',
        alignItems:   'center',
        justifyContent: 'center',
        background:   'none',
        border:       'none',
        cursor:       onClick ? 'pointer' : 'default',
        color:        'var(--faint)',
        padding:      '3px 6px',
        borderRadius: '4px',
        lineHeight:   1,
        transition:   'color 120ms, background 120ms',
      }}
      onMouseEnter={e => {
        if (onClick) {
          ;(e.currentTarget as HTMLButtonElement).style.color = 'var(--text)'
          ;(e.currentTarget as HTMLButtonElement).style.background = 'var(--s3)'
        }
      }}
      onMouseLeave={e => {
        ;(e.currentTarget as HTMLButtonElement).style.color = 'var(--faint)'
        ;(e.currentTarget as HTMLButtonElement).style.background = 'none'
      }}
    >
      {children}
    </button>
  )
}
