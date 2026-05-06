import { useState, useEffect } from 'react'

// ─── Types ─────────────────────────────────────────────────────────────────────

export interface FanThread {
  id: string
  title: string
  preview?: string
}

export interface FanViewProps {
  open: boolean
  threads: FanThread[]
  anchorRect: DOMRect | null
  onSelect: (thread: FanThread) => void
  onDismiss: () => void
}

// ─── Constants ─────────────────────────────────────────────────────────────────

const RADIUS       = 80
const CHIP_D       = 36
const ARC_DEG      = 60
const MAX_VISIBLE  = 7
const TRANSITION   = '0.22s ease'

// ─── Component ─────────────────────────────────────────────────────────────────

export function FanView({ open, threads, anchorRect, onSelect, onDismiss }: FanViewProps) {
  const [selected, setSelected] = useState(0)
  const [offset,   setOffset]   = useState(0)

  useEffect(() => {
    if (open) { setSelected(0); setOffset(0) }
  }, [open])

  useEffect(() => {
    if (!open) return
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'j') {
        e.preventDefault()
        setSelected(s => {
          const next = Math.min(s + 1, threads.length - 1)
          setOffset(o => {
            const visible = threads.length > MAX_VISIBLE ? MAX_VISIBLE : threads.length
            if (next - o >= visible) return Math.min(o + 1, threads.length - visible)
            return o
          })
          return next
        })
      } else if (e.key === 'k') {
        e.preventDefault()
        setSelected(s => {
          const prev = Math.max(s - 1, 0)
          setOffset(o => {
            if (prev < o) return Math.max(o - 1, 0)
            return o
          })
          return prev
        })
      } else if (e.key === 'Enter') {
        onSelect(threads[selected])
      } else if (e.key === 'Escape') {
        onDismiss()
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [open, selected, threads, onSelect, onDismiss])

  if (!open) return null

  const cx = anchorRect ? anchorRect.left + anchorRect.width  / 2 : window.innerWidth  / 2
  const cy = anchorRect ? anchorRect.top                          : window.innerHeight  / 2

  const visible = threads.slice(offset, offset + MAX_VISIBLE)
  const count   = visible.length
  const startAngle = -90 - ARC_DEG / 2
  const angleStep  = count > 1 ? ARC_DEG / (count - 1) : 0

  return (
    <div
      onClick={onDismiss}
      style={{
        position: 'fixed',
        inset:    0,
        zIndex:   7000,
      }}
    >
      {/* Fan origin anchored to pill center-top */}
      <div
        onClick={e => e.stopPropagation()}
        style={{
          position: 'absolute',
          left:     cx,
          top:      cy,
          width:    0,
          height:   0,
        }}
      >
        {visible.map((thread, i) => {
          const globalIdx   = offset + i
          const isSelected  = globalIdx === selected
          const angleDeg    = startAngle + i * angleStep
          const angleRad    = (angleDeg * Math.PI) / 180
          const tx          = Math.cos(angleRad) * RADIUS
          const ty          = Math.sin(angleRad) * RADIUS

          return (
            <button
              key={thread.id}
              title={thread.title}
              onClick={() => onSelect(thread)}
              onMouseEnter={() => setSelected(globalIdx)}
              style={{
                position:     'absolute',
                left:         0,
                top:          0,
                width:        CHIP_D,
                height:       CHIP_D,
                borderRadius: '50%',
                background:   isSelected ? 'var(--accent)'     : 'var(--s2)',
                border:       `0.5px solid ${isSelected ? 'var(--accent-rim)' : 'var(--border)'}`,
                cursor:       'pointer',
                display:      'flex',
                alignItems:   'center',
                justifyContent: 'center',
                fontFamily:   'var(--mono)',
                fontSize:     '9px',
                fontWeight:   isSelected ? 700 : 400,
                color:        isSelected ? 'var(--s0)' : 'var(--muted)',
                transform:    `translate(${tx - CHIP_D / 2}px, ${ty - CHIP_D / 2}px)`,
                transition:   `background ${TRANSITION}, border-color ${TRANSITION}, color ${TRANSITION}, transform ${TRANSITION}`,
                boxShadow:    isSelected ? '0 4px 16px rgba(0,0,0,0.5)' : '0 2px 8px rgba(0,0,0,0.3)',
                userSelect:   'none',
              }}
            >
              {thread.title.slice(0, 2).toUpperCase()}
            </button>
          )
        })}
      </div>

      {/* Scroll hint when more than MAX_VISIBLE */}
      {threads.length > MAX_VISIBLE && (
        <div style={{
          position:   'fixed',
          left:       cx + RADIUS + 12,
          top:        cy - 16,
          fontFamily: 'var(--mono)',
          fontSize:   '10px',
          color:      'var(--faint)',
          pointerEvents: 'none',
        }}>
          {offset + MAX_VISIBLE < threads.length ? `+${threads.length - offset - MAX_VISIBLE} more ↓` : ''}
        </div>
      )}
    </div>
  )
}
