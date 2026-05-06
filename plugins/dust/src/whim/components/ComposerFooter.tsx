"use client"
import { useState, useRef, useEffect } from 'react'
import { Icon } from '../icons/Icon'

// ─── Types ────────────────────────────────────────────────────────────────────

interface FooterPopoverProps {
  items: string[]
  onClose: () => void
}

interface ComposerFooterProps {
  className?: string
}

// ─── Popover ──────────────────────────────────────────────────────────────────

function FooterPopover({ items, onClose }: FooterPopoverProps) {
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
        bottom:       'calc(100% + 6px)',
        right:        0,
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
            padding:      '8px 14px',
            background:   'none',
            border:       'none',
            borderBottom: i < items.length - 1 ? '0.5px solid var(--border-soft)' : 'none',
            cursor:       'pointer',
            fontFamily:   'var(--mono)',
            fontSize:     '11.5px',
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

// ─── Mini chip (right-side model+reasoning display) ───────────────────────────

function MiniChip({
  children,
  items,
}: {
  children: React.ReactNode
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
          padding:      '2px 8px',
          background:   'var(--s1)',
          border:       '0.5px solid var(--border)',
          borderRadius: '5px',
          cursor:       hasPopover ? 'pointer' : 'default',
          fontFamily:   'var(--mono)',
          fontSize:     '11px',
          color:        'var(--faint)',
          whiteSpace:   'nowrap',
          transition:   'background 100ms, color 100ms',
          userSelect:   'none',
          outline:      'none',
        }}
        onMouseEnter={e => {
          if (hasPopover) {
            (e.currentTarget as HTMLButtonElement).style.background = 'var(--s2)'
            ;(e.currentTarget as HTMLButtonElement).style.color = 'var(--muted)'
          }
        }}
        onMouseLeave={e => {
          if (hasPopover) {
            (e.currentTarget as HTMLButtonElement).style.background = 'var(--s1)'
            ;(e.currentTarget as HTMLButtonElement).style.color = 'var(--faint)'
          }
        }}
      >
        {children}
      </button>

      {open && items && (
        <FooterPopover items={items} onClose={() => setOpen(false)} />
      )}
    </div>
  )
}

// ─── GlyphButton ──────────────────────────────────────────────────────────────

function GlyphButton({
  children,
  label,
}: {
  children: React.ReactNode
  label: string
}) {
  return (
    <button
      aria-label={label}
      style={{
        display:        'flex',
        alignItems:     'center',
        justifyContent: 'center',
        width:          '26px',
        height:         '26px',
        background:     'none',
        border:         'none',
        borderRadius:   '5px',
        cursor:         'pointer',
        color:          'var(--muted)',
        transition:     'background 80ms, color 80ms',
        padding:        0,
        flexShrink:     0,
      }}
      onMouseEnter={e => {
        (e.currentTarget as HTMLButtonElement).style.background = 'var(--s2)'
        ;(e.currentTarget as HTMLButtonElement).style.color = 'var(--text)'
      }}
      onMouseLeave={e => {
        (e.currentTarget as HTMLButtonElement).style.background = 'none'
        ;(e.currentTarget as HTMLButtonElement).style.color = 'var(--muted)'
      }}
    >
      {children}
    </button>
  )
}

// ─── ComposerFooter ───────────────────────────────────────────────────────────

export function ComposerFooter({ className }: ComposerFooterProps) {
  return (
    <div
      className={className}
      style={{
        display:        'flex',
        alignItems:     'center',
        justifyContent: 'space-between',
        padding:        '5px 12px',
        background:     'var(--s1)',
        borderTop:      '0.5px solid var(--border-soft)',
        gap:            '8px',
        minHeight:      '36px',
      }}
    >
      {/* Left — Bypass permissions + attach + mic */}
      <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
        <button
          style={{
            display:      'flex',
            alignItems:   'center',
            gap:          '6px',
            padding:      '2px 10px',
            background:   'none',
            border:       'none',
            cursor:       'pointer',
            fontFamily:   'var(--mono)',
            fontSize:     '11.5px',
            color:        'var(--accent)',
            whiteSpace:   'nowrap',
            borderRadius: '5px',
            transition:   'background 80ms',
            userSelect:   'none',
          }}
          onMouseEnter={e => { (e.currentTarget as HTMLButtonElement).style.background = 'var(--accent-soft)' }}
          onMouseLeave={e => { (e.currentTarget as HTMLButtonElement).style.background = 'none' }}
        >
          <Icon name="ShieldWarning" variant="duotone" size={14} />
          Bypass permissions
        </button>

        <GlyphButton label="Attach file">
          <Icon name="Attach" size={14} />
        </GlyphButton>
        <GlyphButton label="Voice input">
          <Icon name="Mic" size={14} />
        </GlyphButton>
      </div>

      {/* Right — model · reasoning mini chip (display only, click opens popover) */}
      <MiniChip items={['Opus 4.7 1M · Extra high', 'Sonnet 4.6 · High', 'Haiku 4.5 · Low']}>
        <Icon name="Sparkles" size={11} />
        Opus 4.7 1M · Extra high
      </MiniChip>
    </div>
  )
}
