import { useEffect, useRef, useState } from 'react'

export interface TourStepperSegment {
  id: string
  chapter: string
  title: string
}

export interface TourStepperProps {
  segments: TourStepperSegment[]
  active: number
  onJump(index: number): void
  reduceMotion?: boolean
}

const CHAPTER_LABELS: Record<string, string> = {
  'at-rest':       'AT REST',
  'summon':        'SUMMON',
  'compose-voice': 'COMPOSE-VOICE',
  'canvas':        'CANVAS',
  'plugins':       'PLUGINS',
  'ambient-error': 'AMBIENT-ERROR',
  'wrap':          'WRAP',
}

export function TourStepper({ segments, active, onJump, reduceMotion = false }: TourStepperProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const [width, setWidth] = useState(720)
  const [hoveredIndex, setHoveredIndex] = useState<number | null>(null)

  useEffect(() => {
    const measure = () => {
      const vw = window.innerWidth
      setWidth(Math.min(vw - 80, 720))
    }
    measure()
    window.addEventListener('resize', measure)
    return () => window.removeEventListener('resize', measure)
  }, [])

  // Build chapter groups
  const chapters: Array<{ name: string; start: number; end: number }> = []
  for (let i = 0; i < segments.length; i++) {
    const ch = segments[i].chapter
    if (!chapters.length || chapters[chapters.length - 1].name !== ch) {
      chapters.push({ name: ch, start: i, end: i })
    } else {
      chapters[chapters.length - 1].end = i
    }
  }

  const CHAPTER_GAP = 8
  const totalGaps = (chapters.length - 1) * CHAPTER_GAP
  const segW = (width - totalGaps) / segments.length

  // Map segment index → x position
  function segX(i: number): number {
    const chIdx = chapters.findIndex(ch => i >= ch.start && i <= ch.end)
    const gapsBefore = chIdx * CHAPTER_GAP
    return gapsBefore + i * segW
  }

  function segColor(i: number): string {
    if (i === active)  return 'var(--accent)'
    if (i < active)    return 'rgba(232,232,236,0.8)'
    return 'transparent'
  }

  function segBorder(i: number): string {
    if (i > active) return '0.5px solid rgba(94,95,104,0.5)'
    return 'none'
  }

  function segHeight(i: number): number {
    if (i === active)                   return 8
    if (hoveredIndex === i)             return 6
    return 4
  }

  function chapterColor(ch: { start: number; end: number }): string {
    if (active >= ch.start && active <= ch.end) return 'var(--accent)'
    if (active > ch.end) return 'var(--text)'
    return 'var(--faint)'
  }

  function chapterCenterX(ch: { name: string; start: number; end: number }): number {
    const startX = segX(ch.start)
    const endX   = segX(ch.end) + segW
    return startX + (endX - startX) / 2
  }

  return (
    <div
      ref={containerRef}
      style={{
        position:  'fixed',
        bottom:    '24px',
        left:      '50%',
        transform: 'translateX(-50%)',
        width:     `${width}px`,
        zIndex:    8600,
      }}
      aria-label="Tour progress"
    >
      {/* Chapter labels */}
      <div style={{ position: 'relative', height: '20px', marginBottom: '4px' }}>
        {chapters.map(ch => (
          <span
            key={ch.name}
            style={{
              position:      'absolute',
              top:           0,
              left:          `${chapterCenterX(ch)}px`,
              transform:     'translateX(-50%)',
              fontFamily:    'var(--mono)',
              fontSize:      '9px',
              letterSpacing: '0.12em',
              color:         chapterColor(ch),
              textTransform: 'uppercase',
              whiteSpace:    'nowrap',
              pointerEvents: 'none',
              userSelect:    'none',
            }}
          >
            {CHAPTER_LABELS[ch.name] ?? ch.name.toUpperCase()}
          </span>
        ))}
      </div>

      {/* Segment track */}
      <div style={{ position: 'relative', height: '16px' }}>
        {segments.map((seg, i) => {
          const x = segX(i)
          const h = segHeight(i)

          return (
            <button
              key={seg.id}
              type="button"
              title={seg.title}
              aria-label={`Jump to step ${i + 1}: ${seg.title}`}
              aria-pressed={i === active}
              onClick={() => onJump(i)}
              onMouseEnter={() => setHoveredIndex(i)}
              onMouseLeave={() => setHoveredIndex(null)}
              style={{
                position:        'absolute',
                left:            `${x}px`,
                top:             `${(16 - 16) / 2}px`,
                width:           `${segW}px`,
                height:          '16px',
                padding:         0,
                background:      'transparent',
                border:          'none',
                cursor:          'pointer',
                display:         'flex',
                alignItems:      'center',
                justifyContent:  'center',
              }}
            >
              <span style={{
                display:    'block',
                width:      `${Math.max(segW - 2, 2)}px`,
                height:     `${h}px`,
                background: segColor(i),
                border:     segBorder(i),
                borderRadius: '2px',
                transition:   reduceMotion ? 'none' : 'height 120ms ease, background 120ms ease',
              }} />
            </button>
          )
        })}
      </div>
    </div>
  )
}
