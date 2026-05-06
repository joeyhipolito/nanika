import { useState, useEffect, useRef, useCallback } from 'react'
import { PaletteShell, type ShellScale, type ResultSection, type RailItem, type RunLogRow } from './PaletteShell'
import { Icon } from '../icons/Icon'

// ─── Types ─────────────────────────────────────────────────────────────────────

export interface TransitionDemoProps {
  autoPlay?: boolean
  sections?: ResultSection[]
  railItems?: RailItem[]
  runLog?: RunLogRow[]
}

// ─── Constants ─────────────────────────────────────────────────────────────────

const SCALES: ShellScale[] = ['pill', 'palette', 'detail', 'canvas']
const AUTO_STEP_MS = 1000

const STAGE_LABELS: Record<ShellScale, string> = {
  pill:    'L0 · Pill',
  palette: 'L1 · Palette',
  detail:  'L2 · Detail',
  canvas:  'L3 · Canvas',
}

// Morphing element CSS — emitted into a <style> block so the literal
// `transition-duration: 200ms` survives in the source file (≤ 220 ms cap).
const MORPH_CSS = `
.transition-demo-morph {
  transition-property: transform, opacity, width, max-width;
  transition-duration: 200ms;
  transition-timing-function: cubic-bezier(0.2, 0.8, 0.2, 1);
  display: flex;
  justify-content: center;
  width: 100%;
}
.transition-demo-morph[data-scale="pill"]    { max-width: 360px; }
.transition-demo-morph[data-scale="palette"] { max-width: 820px; }
.transition-demo-morph[data-scale="detail"]  { max-width: 820px; }
.transition-demo-morph[data-scale="canvas"]  { max-width: 820px; }
`

// ─── Mock content (defaults) ───────────────────────────────────────────────────

const DEFAULT_SECTIONS: ResultSection[] = [
  {
    label: 'Actions',
    items: [
      { id: 'run-mission',   name: 'Run mission',     meta: '⌘↵', selected: true },
      { id: 'tracker-ready', name: 'Tracker ready items' },
      { id: 'new-thread',    name: 'New chat thread', meta: '⌘N' },
    ],
  },
  {
    label: 'Projects',
    items: [
      { id: 'proj-nanika', name: 'nanika', meta: '3 active' },
    ],
  },
]

const DEFAULT_RAIL: RailItem[] = [
  { id: 'whim-palette', label: 'whim-palette', active: true, meta: '2m', working: true },
  { id: 'trk-558',      label: 'TRK-558 parity' },
]

const DEFAULT_RUN_LOG: RunLogRow[] = [
  { id: 'r1', done: true,  persona: 'architect',           phase: 'read-ux-decisions',        duration: '12s' },
  { id: 'r2', done: false, live: true, persona: 'senior-frontend', phase: 'implement-shell',  duration: '2m 14s' },
  { id: 'r3', done: false, persona: 'staff-code-reviewer', phase: 'review' },
]

// ─── Component ─────────────────────────────────────────────────────────────────

export function TransitionDemo({
  autoPlay  = false,
  sections  = DEFAULT_SECTIONS,
  railItems = DEFAULT_RAIL,
  runLog    = DEFAULT_RUN_LOG,
}: TransitionDemoProps) {
  const [stepIndex, setStepIndex] = useState(0)
  const [mode, setMode]           = useState<'manual' | 'auto'>(autoPlay ? 'auto' : 'manual')
  const timerRef                  = useRef<ReturnType<typeof setTimeout> | null>(null)

  const scale = SCALES[stepIndex]

  const clearTimer = () => {
    if (timerRef.current) { clearTimeout(timerRef.current); timerRef.current = null }
  }

  const advance = useCallback(() => {
    setStepIndex(i => Math.min(i + 1, SCALES.length - 1))
  }, [])

  const reset = useCallback(() => {
    clearTimer()
    setStepIndex(0)
    setMode('manual')
  }, [])

  // Keyboard: Space advances, Esc returns to L0.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === ' ') {
        e.preventDefault()
        setMode('manual')
        clearTimer()
        advance()
      } else if (e.key === 'Escape') {
        reset()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [advance, reset])

  // Auto-play: ~1 s per scale.
  useEffect(() => {
    if (mode !== 'auto') { clearTimer(); return }
    if (stepIndex >= SCALES.length - 1) { clearTimer(); return }
    timerRef.current = setTimeout(() => advance(), AUTO_STEP_MS)
    return clearTimer
  }, [mode, stepIndex, advance])

  const atEnd = stepIndex >= SCALES.length - 1

  return (
    <div style={{
      display:        'flex',
      flexDirection:  'column',
      alignItems:     'center',
      gap:            '24px',
      width:          '100%',
    }}>
      <style>{MORPH_CSS}</style>

      {/* Stepper bar */}
      <div style={{
        display:        'flex',
        alignItems:     'center',
        gap:            '14px',
        padding:        '10px 16px',
        background:     'var(--s1)',
        border:         '0.5px solid var(--border)',
        borderRadius:   '999px',
        fontFamily:     'var(--mono)',
        fontSize:       '12px',
      }}>
        {/* Stage breadcrumbs */}
        <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
          {SCALES.map((s, i) => (
            <span key={s} style={{ display: 'inline-flex', alignItems: 'center', gap: '8px' }}>
              <span style={{
                color:         i <= stepIndex ? 'var(--accent)' : 'var(--ghost)',
                letterSpacing: '0.06em',
                fontWeight:    i === stepIndex ? 600 : 400,
              }}>
                {STAGE_LABELS[s]}
              </span>
              {i < SCALES.length - 1 && (
                <span style={{ color: 'var(--ghost)' }}>→</span>
              )}
            </span>
          ))}
        </div>

        <span style={{ color: 'var(--ghost)' }}>·</span>

        {/* Next button */}
        <button
          type="button"
          onClick={() => { setMode('manual'); advance() }}
          disabled={atEnd}
          aria-label="Advance to next stage"
          style={{
            display:        'inline-flex',
            alignItems:     'center',
            gap:            '6px',
            padding:        '5px 12px',
            borderRadius:   '999px',
            background:     atEnd ? 'transparent' : 'var(--accent-soft)',
            border:         atEnd ? '0.5px solid var(--border-soft)' : '0.5px solid var(--accent-rim)',
            color:          atEnd ? 'var(--faint)' : 'var(--accent)',
            cursor:         atEnd ? 'default' : 'pointer',
            fontFamily:     'var(--mono)',
            fontSize:       '11px',
            letterSpacing:  '0.06em',
            opacity:        atEnd ? 0.5 : 1,
          }}
        >
          ▶ Next
        </button>

        {/* Reset button */}
        <button
          type="button"
          onClick={reset}
          aria-label="Reset to L0"
          style={{
            display:        'inline-flex',
            alignItems:     'center',
            gap:            '4px',
            padding:        '5px 10px',
            borderRadius:   '999px',
            background:     'transparent',
            border:         '0.5px solid var(--border-soft)',
            color:          'var(--muted)',
            cursor:         'pointer',
            fontFamily:     'var(--mono)',
            fontSize:       '11px',
            letterSpacing:  '0.06em',
          }}
        >
          Esc · L0
        </button>

        {/* Auto-play toggle */}
        <button
          type="button"
          onClick={() => setMode(m => m === 'auto' ? 'manual' : 'auto')}
          aria-label="Toggle auto-play"
          aria-pressed={mode === 'auto'}
          style={{
            display:        'inline-flex',
            alignItems:     'center',
            gap:            '4px',
            padding:        '5px 10px',
            borderRadius:   '999px',
            background:     mode === 'auto' ? 'var(--s2)' : 'transparent',
            border:         '0.5px solid var(--border-soft)',
            color:          mode === 'auto' ? 'var(--accent-2)' : 'var(--faint)',
            cursor:         'pointer',
            fontFamily:     'var(--mono)',
            fontSize:       '11px',
            letterSpacing:  '0.06em',
          }}
        >
          {mode === 'auto' ? '⏸ Auto' : '▷ Auto'}
        </button>
      </div>

      {/* Morphing substrate — single PaletteShell instance, scale prop animates. */}
      <div
        className="transition-demo-morph"
        data-scale={scale}
        data-stage-index={stepIndex}
        aria-live="polite"
      >
        <PaletteShell
          scale={scale}
          pillMode="hover"
          convCount={3}
          query=""
          placeholder="Search Whim…"
          sections={sections}
          breadcrumb="whim › palette › detail"
          detailContent={
            <div style={{
              display:       'flex',
              flexDirection: 'column',
              gap:           '10px',
              fontSize:      '13px',
              color:         'var(--muted)',
              lineHeight:    1.55,
            }}>
              <div style={{
                fontFamily:    'var(--mono)',
                fontSize:      '11px',
                color:         'var(--accent)',
                letterSpacing: '0.08em',
                textTransform: 'uppercase',
              }}>
                Run mission
              </div>
              <div>Spawn the selected mission on the orchestrator. ⌘↵ confirms; Esc returns.</div>
              <div style={{ display: 'flex', alignItems: 'center', gap: '6px', color: 'var(--faint)', fontFamily: 'var(--mono)', fontSize: '11px' }}>
                <Icon name="BoltDefault" size={12} />
                <span>est. 4–6 min · 3 personas</span>
              </div>
            </div>
          }
          railItems={railItems}
          runLog={runLog}
          composerPlaceholder="Ask anything, @tag files, or use / for commands…"
          warmBleed
        />
      </div>

      <p style={{
        fontFamily:    'var(--mono)',
        fontSize:      '10px',
        color:         'var(--ghost)',
        letterSpacing: '0.08em',
        margin:        0,
      }}>
        space advance · esc reset · single substrate, scale prop animates
      </p>
    </div>
  )
}
