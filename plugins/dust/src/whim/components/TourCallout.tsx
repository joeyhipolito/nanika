import type { TourStep } from '../tour/types'

export interface TourCalloutProps {
  step: TourStep
  totalSteps: number
  index: number
  onNext(): void
  onBack(): void
  onExit(): void
  preferredPlacement?: 'top-right' | 'top-left' | 'bottom-right' | 'bottom-left'
  spotlightRect: DOMRect | null
  reduceMotion?: boolean
}

const CALLOUT_W   = 360
const CALLOUT_H   = 200  // conservative estimate; actual height may differ
const MARGIN      = 24
const SLACK       = 12

type Corner = 'top-right' | 'top-left' | 'bottom-right' | 'bottom-left'

function cornerBox(
  corner: Corner,
  vw: number,
  vh: number,
): { top: number; left: number } {
  switch (corner) {
    case 'top-right':    return { top: MARGIN,              left: vw - CALLOUT_W - MARGIN }
    case 'top-left':     return { top: MARGIN,              left: MARGIN }
    case 'bottom-right': return { top: vh - CALLOUT_H - MARGIN, left: vw - CALLOUT_W - MARGIN }
    case 'bottom-left':  return { top: vh - CALLOUT_H - MARGIN, left: MARGIN }
  }
}

function rectsIntersect(
  aTop: number, aLeft: number, aW: number, aH: number,
  bTop: number, bLeft: number, bW: number, bH: number,
  slack: number,
): boolean {
  return !(
    aLeft + aW + slack  < bLeft ||
    bLeft + bW  + slack < aLeft ||
    aTop  + aH  + slack < bTop  ||
    bTop  + bH  + slack < aTop
  )
}

function placeCallout(
  spotlightRect: DOMRect | null,
  preferred: Corner,
): { top: number; left: number } {
  const vw = window.innerWidth
  const vh = window.innerHeight

  const order: Corner[] = [preferred]
  const all: Corner[]   = ['top-right', 'top-left', 'bottom-right', 'bottom-left']
  for (const c of all) { if (c !== preferred) order.push(c) }

  for (const corner of order) {
    const pos = cornerBox(corner, vw, vh)
    // Viewport clip check
    if (pos.left < 0 || pos.top < 0 || pos.left + CALLOUT_W > vw || pos.top + CALLOUT_H > vh) continue
    // Spotlight collision check
    if (spotlightRect && rectsIntersect(
      pos.top, pos.left, CALLOUT_W, CALLOUT_H,
      spotlightRect.top, spotlightRect.left, spotlightRect.width, spotlightRect.height,
      SLACK,
    )) continue
    return pos
  }

  // Fallback: place in corner most distant from spotlight centroid
  if (spotlightRect) {
    const cx = spotlightRect.left + spotlightRect.width  / 2
    const cy = spotlightRect.top  + spotlightRect.height / 2
    const opposite: Corner =
      cx > vw / 2
        ? cy > vh / 2 ? 'top-left'     : 'bottom-left'
        : cy > vh / 2 ? 'top-right'    : 'bottom-right'
    return cornerBox(opposite, vw, vh)
  }

  return cornerBox(preferred, vw, vh)
}

const CHAPTER_DISPLAY: Record<string, string> = {
  'at-rest':       'AT REST',
  'summon':        'SUMMON',
  'compose-voice': 'COMPOSE-VOICE',
  'canvas':        'CANVAS',
  'plugins':       'PLUGINS',
  'ambient-error': 'AMBIENT-ERROR',
  'wrap':          'WRAP',
}

export function TourCallout({
  step,
  totalSteps,
  index,
  onNext,
  onBack,
  onExit,
  preferredPlacement = 'top-right',
  spotlightRect,
  reduceMotion = false,
}: TourCalloutProps) {
  const pos = placeCallout(spotlightRect, preferredPlacement)

  const isFirst = index === 0
  const isLast  = index === totalSteps - 1

  return (
    <div
      role="dialog"
      aria-label={`Tour step ${index + 1} of ${totalSteps}: ${step.title}`}
      style={{
        position:     'fixed',
        top:          `${pos.top}px`,
        left:         `${pos.left}px`,
        width:        `${CALLOUT_W}px`,
        background:   'var(--s1)',
        border:       '0.5px solid var(--border)',
        borderRadius: '14px',
        boxShadow:    '0 40px 80px rgba(0,0,0,0.5), 0 12px 32px rgba(0,0,0,0.45)',
        padding:      '16px 20px 14px',
        zIndex:       8700,
        animation:    reduceMotion ? 'none' : 'callout-in 220ms cubic-bezier(0.2, 0.8, 0.2, 1) forwards',
      }}
    >
      {/* Caption line */}
      <div style={{
        fontFamily:    'var(--mono)',
        fontSize:      '10px',
        color:         'var(--faint)',
        letterSpacing: '0.1em',
        marginBottom:  '10px',
        display:       'flex',
        alignItems:    'center',
        gap:           '6px',
      }}>
        <span>{index + 1} / {totalSteps}</span>
        <span>·</span>
        <span>CHAPTER {Object.keys(CHAPTER_DISPLAY).indexOf(step.chapter) + 1} — {CHAPTER_DISPLAY[step.chapter] ?? step.chapter.toUpperCase()}</span>
      </div>

      {/* Title */}
      <div style={{
        fontSize:      '13.5px',
        fontWeight:    600,
        color:         'var(--text)',
        letterSpacing: '-0.01em',
        marginBottom:  '8px',
        lineHeight:    1.3,
      }}>
        {step.title}
      </div>

      {/* Body */}
      <div style={{
        fontSize:   '12.5px',
        color:      'var(--muted)',
        lineHeight: 1.55,
        marginBottom: '14px',
      }}>
        {step.body}
      </div>

      {/* Keycap row */}
      {step.keys.length > 0 && (
        <div style={{
          display:      'flex',
          alignItems:   'center',
          gap:          '10px',
          flexWrap:     'wrap',
          marginBottom: '14px',
          paddingTop:   '8px',
          borderTop:    '0.5px solid var(--border-soft)',
        }}>
          {step.keys.map((k, i) => (
            <span key={i} style={{ display: 'inline-flex', alignItems: 'center', gap: '4px' }}>
              <span
                className="K"
                style={k.owned ? {} : { color: 'var(--faint)', borderColor: 'var(--border-soft)', background: 'transparent' }}
              >
                {k.key}
              </span>
              <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--faint)', letterSpacing: '0.06em' }}>
                {k.label}
              </span>
            </span>
          ))}
        </div>
      )}

      {/* Navigation row */}
      <div style={{
        display:        'flex',
        alignItems:     'center',
        justifyContent: 'space-between',
        gap:            '8px',
      }}>
        <button
          type="button"
          onClick={onExit}
          style={{
            fontFamily:    'var(--mono)',
            fontSize:      '10px',
            color:         'var(--faint)',
            background:    'none',
            border:        'none',
            cursor:        'pointer',
            letterSpacing: '0.06em',
            padding:       '4px 0',
          }}
        >
          Esc · exit
        </button>

        <div style={{ display: 'flex', gap: '6px' }}>
          {!isFirst && (
            <button
              type="button"
              onClick={onBack}
              aria-label="Previous step"
              style={{
                fontFamily:    'var(--mono)',
                fontSize:      '11px',
                color:         'var(--muted)',
                background:    'transparent',
                border:        '0.5px solid var(--border)',
                borderRadius:  '6px',
                cursor:        'pointer',
                padding:       '5px 12px',
                letterSpacing: '0.04em',
              }}
            >
              ← back
            </button>
          )}
          <button
            type="button"
            onClick={isLast ? onExit : onNext}
            aria-label={isLast ? 'Finish tour' : 'Next step'}
            style={{
              fontFamily:    'var(--mono)',
              fontSize:      '11px',
              color:         isLast ? 'var(--s0)' : 'var(--accent)',
              background:    isLast ? 'var(--accent)' : 'var(--accent-soft)',
              border:        isLast ? 'none' : '0.5px solid var(--accent-rim)',
              borderRadius:  '6px',
              cursor:        'pointer',
              padding:       '5px 14px',
              letterSpacing: '0.04em',
              fontWeight:    isLast ? 600 : 400,
            }}
          >
            {isLast ? 'Finish' : '→ next'}
          </button>
        </div>
      </div>
    </div>
  )
}
