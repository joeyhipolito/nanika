import { useState, useEffect } from 'react'
import { Keycap } from './Keycap'

// ─── Types ─────────────────────────────────────────────────────────────────────

export interface ActionPaletteItem {
  id: string
  title: string
  subtitle?: string
}

export interface ActionPaletteAction {
  id: string
  verb: string
  key: string
}

export interface ActionPaletteProps {
  open: boolean
  item: ActionPaletteItem
  actions: ActionPaletteAction[]
  onAction: (actionId: string) => void
  onDismiss: () => void
}

// ─── Component ─────────────────────────────────────────────────────────────────

export function ActionPalette({ open, item, actions, onAction, onDismiss }: ActionPaletteProps) {
  const [selected, setSelected] = useState(0)

  useEffect(() => {
    if (open) setSelected(0)
  }, [open])

  useEffect(() => {
    if (!open) return
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'j') {
        e.preventDefault()
        setSelected(s => Math.min(s + 1, actions.length - 1))
      } else if (e.key === 'k') {
        e.preventDefault()
        setSelected(s => Math.max(s - 1, 0))
      } else if (e.key === 'Enter') {
        onAction(actions[selected].id)
      } else if (e.key === 'Escape') {
        onDismiss()
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [open, selected, actions, onAction, onDismiss])

  if (!open) return null

  return (
    <div
      onClick={onDismiss}
      style={{
        position:       'fixed',
        inset:          0,
        background:     'rgba(12,13,16,0.6)',
        backdropFilter: 'blur(8px)',
        display:        'flex',
        alignItems:     'center',
        justifyContent: 'center',
        zIndex:         8000,
      }}
    >
      <div
        onClick={e => e.stopPropagation()}
        style={{
          background:   'var(--s1)',
          border:       '0.5px solid var(--border)',
          borderRadius: '12px',
          width:        '320px',
          overflow:     'hidden',
          boxShadow:    '0 24px 64px rgba(0,0,0,0.6)',
        }}
      >
        {/* Item header */}
        <div style={{
          padding:      '16px 20px 12px',
          borderBottom: '0.5px solid var(--border-soft)',
        }}>
          <div style={{
            fontFamily: 'var(--sans)',
            fontSize:   '13px',
            fontWeight: 600,
            color:      'var(--text)',
            marginBottom: item.subtitle ? '3px' : 0,
          }}>
            {item.title}
          </div>
          {item.subtitle && (
            <div style={{
              fontFamily:    'var(--mono)',
              fontSize:      '10px',
              color:         'var(--muted)',
              letterSpacing: '0.06em',
            }}>
              {item.subtitle}
            </div>
          )}
        </div>

        {/* Verb list */}
        <div style={{ padding: '6px 0' }}>
          {actions.map((action, i) => (
            <button
              key={action.id}
              onClick={() => onAction(action.id)}
              onMouseEnter={() => setSelected(i)}
              style={{
                display:         'flex',
                alignItems:      'center',
                justifyContent:  'space-between',
                width:           '100%',
                padding:         '9px 20px',
                background:      selected === i ? 'var(--s2)' : 'transparent',
                border:          'none',
                cursor:          'pointer',
                transition:      'background 0.08s ease',
                outline:         selected === i ? '1px solid var(--accent-rim)' : 'none',
                outlineOffset:   '-1px',
              }}
            >
              <span style={{
                fontFamily: 'var(--sans)',
                fontSize:   '13px',
                color:      selected === i ? 'var(--text)' : 'var(--muted)',
                fontWeight: selected === i ? 500 : 400,
              }}>
                {action.verb}
              </span>
              <Keycap variant="nav">{action.key}</Keycap>
            </button>
          ))}
        </div>

        {/* Footer hint */}
        <div style={{
          padding:       '8px 20px 12px',
          borderTop:     '0.5px solid var(--border-soft)',
          display:       'flex',
          gap:           '8px',
          fontFamily:    'var(--mono)',
          fontSize:      '10px',
          color:         'var(--faint)',
          letterSpacing: '0.06em',
        }}>
          <span>j/k navigate</span>
          <span>·</span>
          <span>Enter confirm</span>
          <span>·</span>
          <span>Esc dismiss</span>
        </div>
      </div>
    </div>
  )
}
