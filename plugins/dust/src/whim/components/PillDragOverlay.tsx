// ─── Types ─────────────────────────────────────────────────────────────────────

export interface PillDragOverlayProps {
  active: boolean
  anchorPercents: number[]
}

// ─── Component ─────────────────────────────────────────────────────────────────

export function PillDragOverlay({ active, anchorPercents }: PillDragOverlayProps) {
  if (!active) return null

  return (
    <>
      {/* Invisible anchor target for SpotlightRing */}
      <div data-tour-anchor="pill-drag-overlay" style={{
        position:      'absolute',
        inset:         0,
        pointerEvents: 'none',
        zIndex:        0,
      }} />
      {anchorPercents.map(pct => (
        <div
          key={pct}
          data-snap-line={pct}
          style={{
            position:      'absolute',
            left:          `${pct}%`,
            top:           0,
            bottom:        0,
            width:         '1px',
            background:    'var(--accent)',
            opacity:       0.65,
            pointerEvents: 'none',
            zIndex:        1,
            transition:    'opacity 0.15s ease',
          }}
        />
      ))}
    </>
  )
}
