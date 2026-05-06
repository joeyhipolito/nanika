"use client"
import { useState, useRef, useEffect } from 'react'
import { Icon } from '../icons/Icon'

// ─── Types ────────────────────────────────────────────────────────────────────

interface BreadcrumbChip {
  label: string
  href?: string
}

interface TopActionBarProps {
  breadcrumbs?: BreadcrumbChip[]
}

// ─── TopActionBar ─────────────────────────────────────────────────────────────

export function TopActionBar({ breadcrumbs = [] }: TopActionBarProps) {
  const [openMenu, setOpenMenu] = useState<'open' | 'commit' | null>(null)

  return (
    <div style={{
      display:        'flex',
      alignItems:     'center',
      justifyContent: 'space-between',
      padding:        '0 12px',
      height:         '40px',
      background:     'var(--s1)',
      borderBottom:   '0.5px solid var(--border)',
      position:       'relative',
      flexShrink:     0,
    }}>
      {/* Left — breadcrumb chips */}
      <div style={{ display: 'flex', alignItems: 'center', gap: '4px' }}>
        {breadcrumbs.map((chip, i) => (
          <span key={i} style={{ display: 'flex', alignItems: 'center', gap: '4px' }}>
            {i > 0 && (
              <Icon name="ChevronRight" size={12} style={{ color: 'var(--faint)' }} />
            )}
            <span style={{
              fontFamily:    'var(--mono)',
              fontSize:      '11.5px',
              color:         i === breadcrumbs.length - 1 ? 'var(--text)' : 'var(--muted)',
              padding:       '2px 8px',
              borderRadius:  '5px',
              background:    i === breadcrumbs.length - 1 ? 'var(--s3)' : 'transparent',
              border:        i === breadcrumbs.length - 1 ? '0.5px solid var(--border)' : 'none',
              cursor:        chip.href ? 'pointer' : 'default',
            }}>
              {chip.label}
            </span>
          </span>
        ))}
      </div>

      {/* Right — actions */}
      <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
        {/* + Add action button */}
        <button style={{
          display:      'flex',
          alignItems:   'center',
          gap:          '4px',
          padding:      '4px 11px',
          background:   'var(--accent)',
          border:       'none',
          borderRadius: '6px',
          cursor:       'pointer',
          fontFamily:   'var(--mono)',
          fontSize:     '11.5px',
          fontWeight:   600,
          color:        '#fff',
          letterSpacing: '0.02em',
          whiteSpace:   'nowrap',
        }}>
          <Icon name="PlusAccent" size={14} /> Add action
        </button>

        {/* Open ▾ */}
        <div style={{ position: 'relative' }}>
          <ChipButton
            label="Open"
            active={openMenu === 'open'}
            onClick={() => setOpenMenu(m => m === 'open' ? null : 'open')}
          />
          {openMenu === 'open' && (
            <Popover onClose={() => setOpenMenu(null)} items={[
              'Open in editor',
              'Open in new tab',
            ]} />
          )}
        </div>

        {/* Commit & push ▾ */}
        <div style={{ position: 'relative' }}>
          <ChipButton
            label="Commit & push"
            active={openMenu === 'commit'}
            onClick={() => setOpenMenu(m => m === 'commit' ? null : 'commit')}
          />
          {openMenu === 'commit' && (
            <Popover onClose={() => setOpenMenu(null)} items={[
              'Commit only',
              'Commit & push',
              'Amend last commit',
            ]} />
          )}
        </div>

        {/* Divider */}
        <div style={{ width: '1px', height: '16px', background: 'var(--border)', margin: '0 2px' }} />

        {/* Expand glyph */}
        <IconButton title="Expand panel"><Icon name="ExpandWindow" size={14} /></IconButton>

        {/* + new-tab glyph */}
        <IconButton title="New tab"><Icon name="NewTab" size={14} /></IconButton>
      </div>
    </div>
  )
}

// ─── ChipButton ───────────────────────────────────────────────────────────────

function ChipButton({
  label,
  active,
  onClick,
}: {
  label: string
  active: boolean
  onClick: () => void
}) {
  return (
    <button
      onClick={onClick}
      aria-expanded={active}
      style={{
        display:      'flex',
        alignItems:   'center',
        gap:          '4px',
        padding:      '4px 10px',
        background:   active ? 'var(--s3)' : 'var(--s2)',
        border:       `0.5px solid ${active ? 'var(--accent-rim)' : 'var(--border)'}`,
        borderRadius: '6px',
        cursor:       'pointer',
        fontFamily:   'var(--mono)',
        fontSize:     '11.5px',
        color:        active ? 'var(--text)' : 'var(--muted)',
        whiteSpace:   'nowrap',
        outline:      active ? '1px solid var(--accent-rim)' : 'none',
        outlineOffset: '-1px',
        transition:   'background 120ms, color 120ms',
      }}
    >
      {label}
      <Icon name="CaretDown" size={11} />
    </button>
  )
}

// ─── Popover ──────────────────────────────────────────────────────────────────

function Popover({ items, onClose }: { items: string[]; onClose: () => void }) {
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
      style={{
        position:     'absolute',
        top:          'calc(100% + 6px)',
        right:        0,
        background:   'var(--s2)',
        border:       '0.5px solid var(--border)',
        borderRadius: '8px',
        boxShadow:    '0 8px 24px rgba(0,0,0,0.5)',
        minWidth:     '160px',
        zIndex:       1000,
        overflow:     'hidden',
      }}
    >
      {items.map((item, i) => (
        <button
          key={i}
          onClick={onClose}
          style={{
            display:    'block',
            width:      '100%',
            padding:    '9px 14px',
            background: 'none',
            border:     'none',
            borderBottom: i < items.length - 1 ? '0.5px solid var(--border-soft)' : 'none',
            cursor:     'pointer',
            fontFamily: 'var(--mono)',
            fontSize:   '12px',
            color:      'var(--text)',
            textAlign:  'left',
            transition: 'background 80ms',
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

// ─── IconButton ───────────────────────────────────────────────────────────────

function IconButton({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <button
      title={title}
      style={{
        display:      'flex',
        alignItems:   'center',
        justifyContent: 'center',
        width:        '28px',
        height:       '28px',
        background:   'none',
        border:       'none',
        borderRadius: '5px',
        cursor:       'pointer',
        fontFamily:   'var(--mono)',
        fontSize:     '14px',
        color:        'var(--faint)',
        transition:   'color 120ms, background 120ms',
      }}
      onMouseEnter={e => {
        ;(e.currentTarget as HTMLButtonElement).style.color = 'var(--text)'
        ;(e.currentTarget as HTMLButtonElement).style.background = 'var(--s3)'
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
