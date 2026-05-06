"use client"
import { useState, useRef, useEffect } from 'react'
import { Icon } from '../icons/Icon'

// ─── Types ────────────────────────────────────────────────────────────────────

interface ChipPopoverProps {
  items: string[]
  onClose: () => void
}

interface ComposerChipsProps {
  className?: string
}

// ─── Popover ──────────────────────────────────────────────────────────────────

function ChipPopover({ items, onClose }: ChipPopoverProps) {
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        onClose()
      }
    }
    document.addEventListener('mousedown', handler)
    return () => document.removeEventListener('mousedown', handler)
  }, [onClose])

  return (
    <div
      ref={ref}
      role="menu"
      style={{
        position:     'absolute',
        top:          'calc(100% + 6px)',
        left:         0,
        background:   'var(--s2)',
        border:       '0.5px solid var(--border)',
        borderRadius: '8px',
        boxShadow:    '0 8px 24px rgba(0,0,0,0.55)',
        minWidth:     '176px',
        zIndex:       1000,
        overflow:     'hidden',
      }}
    >
      {items.map((item, i) => (
        <button
          key={i}
          role="menuitem"
          onClick={onClose}
          style={{
            display:      'block',
            width:        '100%',
            padding:      '9px 14px',
            background:   'none',
            border:       'none',
            borderBottom: i < items.length - 1 ? '0.5px solid var(--border-soft)' : 'none',
            cursor:       'pointer',
            fontFamily:   'var(--mono)',
            fontSize:     '12px',
            color:        'var(--text)',
            textAlign:    'left',
            transition:   'background 80ms',
          }}
          onMouseEnter={e => { (e.currentTarget as HTMLButtonElement).style.background = 'var(--s3)' }}
          onMouseLeave={e => { (e.currentTarget as HTMLButtonElement).style.background = 'none' }}
        >
          {item}
        </button>
      ))}
    </div>
  )
}

// ─── Chip ─────────────────────────────────────────────────────────────────────

function Chip({
  children,
  caret,
  muted,
  items,
}: {
  children: React.ReactNode
  caret?: boolean
  muted?: boolean
  items?: string[]
}) {
  const [open, setOpen] = useState(false)
  const hasPopover = items && items.length > 0

  return (
    <div style={{ position: 'relative' }}>
      <button
        onClick={hasPopover ? () => setOpen(v => !v) : undefined}
        aria-expanded={hasPopover ? open : undefined}
        aria-haspopup={hasPopover ? 'menu' : undefined}
        style={{
          display:      'flex',
          alignItems:   'center',
          gap:          '4px',
          padding:      '3px 9px',
          background:   'var(--s1)',
          border:       `0.5px solid var(--border)`,
          borderRadius: '6px',
          cursor:       hasPopover ? 'pointer' : 'default',
          fontFamily:   'var(--mono)',
          fontSize:     '11.5px',
          color:        muted ? 'var(--faint)' : 'var(--muted)',
          whiteSpace:   'nowrap',
          transition:   'background 100ms, color 100ms',
          userSelect:   'none',
          outline:      'none',
        }}
        onMouseEnter={e => {
          if (hasPopover) {
            (e.currentTarget as HTMLButtonElement).style.background = 'var(--s2)'
            ;(e.currentTarget as HTMLButtonElement).style.color = 'var(--text)'
          }
        }}
        onMouseLeave={e => {
          if (hasPopover) {
            (e.currentTarget as HTMLButtonElement).style.background = 'var(--s1)'
            ;(e.currentTarget as HTMLButtonElement).style.color = muted ? 'var(--faint)' : 'var(--muted)'
          }
        }}
      >
        {children}
        {caret && (
          <span style={{ marginLeft: '1px', display: 'flex', color: 'var(--ghost)' }}>
            <Icon name="CaretDown" size={9} />
          </span>
        )}
      </button>

      {open && items && (
        <ChipPopover items={items} onClose={() => setOpen(false)} />
      )}
    </div>
  )
}

// ─── ComposerChips ────────────────────────────────────────────────────────────

export function ComposerChips({ className }: ComposerChipsProps) {
  return (
    <div
      className={className}
      style={{
        display:    'flex',
        alignItems: 'center',
        gap:        '6px',
        padding:    '6px 14px',
        background: 'var(--s1)',
        flexWrap:   'wrap',
      }}
    >
      {/* Model chip */}
      <Chip
        caret
        items={['Claude Opus 4.6', 'Claude Sonnet 4.6', 'Claude Haiku 4.5']}
      >
        <Icon name="ModelLeading" size={12} />
        Claude Opus 4.6
      </Chip>

      {/* Reasoning chip */}
      <Chip
        caret
        items={['High · Normal · 200k', 'High · Extended · 200k', 'Low · Normal · 200k']}
      >
        High · Normal · 200k
      </Chip>

      {/* Mode chip */}
      <Chip
        caret
        items={['Build', 'Plan', 'Review', 'Chat']}
      >
        Build
      </Chip>

      {/* Permissions chip */}
      <Chip
        caret
        items={['Full access', 'Restricted', 'Read-only']}
      >
        <Icon name="Lock" size={13} />
        Full access
      </Chip>

      {/* Token counter — plain muted, no caret, no popover */}
      <Chip muted>
        26
      </Chip>
    </div>
  )
}
