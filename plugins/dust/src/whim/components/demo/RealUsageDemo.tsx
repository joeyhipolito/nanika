import { useEffect, useMemo, useReducer, useRef } from 'react'
import type { CSSProperties } from 'react'
import { PaletteShell } from '../PaletteShell'
import { CompositeCanvas } from '../CompositeCanvas'
import { Keycap } from '../Keycap'
import { DEMO_SCRIPT } from '../../demo/script'
import {
  INITIAL_DEMO_STATE,
  INITIAL_CANVAS_STATE,
  type DemoState,
  type DemoStep,
  type DemoCallout,
  type DemoCursor,
  type DemoSurface,
} from '../../demo/state'

// ─── Reducer ──────────────────────────────────────────────────────────────────

type Action =
  | { type: 'goto';   index: number; nowMs: number }
  | { type: 'tick';   patch: Partial<DemoState> }
  | { type: 'press';  key: string; nowMs: number }
  | { type: 'play';   playing: boolean }
  | { type: 'reduce-motion'; on: boolean }

function applyStep(state: DemoState, step: DemoStep, nowMs: number): DemoState {
  return {
    ...state,
    ...step.sceneMutation,
    canvas:       canvasForSurface(step, state),
    narration:    step.narration,
    explanation:  step.explanation,
    chapter:      step.chapter,
    chapterTitle: step.chapterTitle,
    callout:      step.callout ?? null,
    pressedKey:   step.sceneMutation.pressedKey ?? null,
    pressedAt:    step.sceneMutation.pressedKey ? nowMs : state.pressedAt,
  }
}

function canvasForSurface(step: DemoStep, prev: DemoState) {
  const surface = step.sceneMutation.surface ?? prev.surface
  const id = step.id

  // Step-specific canvas mutations
  let next = { ...INITIAL_CANVAS_STATE }
  if (surface === 'canvas' || surface === 'diff') {
    next = {
      ...next,
      conversationFixture: 'baseline',
    }
  }
  if (id === 'd16-runlog' || id === 'd17-terminal-open' || id === 'd18-terminal-close' || id === 'd19-rail-files') {
    next.missionRun = {
      missionId:     'trk-558',
      title:         'open the diff for the last commit',
      activePhaseId: 'implement',
      elapsedMs:     0,
      phases: [
        { id: 'architect',  persona: 'staff-architect',         status: 'done',    expectedMs: 45_000 },
        { id: 'implement',  persona: 'senior-backend-engineer', status: 'running', expectedMs: 120_000 },
        { id: 'review',     persona: 'staff-code-reviewer',     status: 'pending', expectedMs: 30_000 },
      ],
    }
  }
  if (id === 'd17-terminal-open') next.terminalOpen = true
  if (id === 'd19-rail-files' || id === 'd20-mission-done' || id === 'd21-view-diff') {
    next.rightRailOpen = true
    next.rightRailMode = 'files'
  }
  if (id === 'd20-mission-done' || id === 'd21-view-diff') {
    next.reviewGate = true
  }
  if (surface === 'diff') {
    next.rightRailOpen = true
    next.rightRailMode = 'turn-diff'
    next.reviewGate    = true
  }
  return next
}

function reducer(state: DemoState, action: Action): DemoState {
  switch (action.type) {
    case 'goto': {
      const i = Math.max(0, Math.min(action.index, DEMO_SCRIPT.length - 1))
      const step = DEMO_SCRIPT[i]
      return { ...applyStep(state, step, action.nowMs), stepIndex: i }
    }
    case 'tick':
      return { ...state, ...action.patch }
    case 'press':
      return { ...state, pressedKey: action.key, pressedAt: action.nowMs }
    case 'play':
      return { ...state, playing: action.playing }
    case 'reduce-motion':
      return { ...state, reduceMotion: action.on }
  }
}

// ─── Sub-components ───────────────────────────────────────────────────────────

function NarrationBar({ step, index, total }: { step: DemoStep; index: number; total: number }) {
  return (
    <div
      data-demo-coverage-token="narration-bar"
      style={{
        position:       'absolute',
        top:            0,
        left:           0,
        right:          0,
        padding:        '14px 22px 12px',
        display:        'flex',
        alignItems:     'baseline',
        gap:            '14px',
        background:     'linear-gradient(180deg, rgba(12,13,16,0.92), rgba(12,13,16,0.0))',
        zIndex:         50,
        pointerEvents:  'none',
        fontFamily:     'var(--sans)',
      }}
    >
      <span style={{
        fontFamily:    'var(--mono)',
        fontSize:      '10px',
        color:         'var(--accent)',
        letterSpacing: '0.16em',
        textTransform: 'uppercase',
        flexShrink:    0,
      }}>
        {String(index + 1).padStart(2, '0')} / {total} · {step.chapterTitle}
      </span>
      <span style={{ fontSize: '15px', color: 'var(--text)', fontWeight: 500, flex: 1 }}>
        {step.narration}
      </span>
    </div>
  )
}

function ExplanationStrip({ step }: { step: DemoStep }) {
  return (
    <div
      data-demo-coverage-token="explanation-strip"
      style={{
        position:      'absolute',
        bottom:        0,
        left:          0,
        right:         0,
        padding:       '14px 22px 16px',
        background:    'linear-gradient(0deg, rgba(12,13,16,0.92), rgba(12,13,16,0.0))',
        zIndex:        50,
        pointerEvents: 'none',
        fontFamily:    'var(--sans)',
        fontSize:      '12px',
        color:         'var(--muted)',
        lineHeight:    1.55,
        textAlign:     'center',
      }}
    >
      {step.explanation}
    </div>
  )
}

function KeyHUD({ pressedKey, since, nowMs, reduceMotion }: { pressedKey: string | null; since: number; nowMs: number; reduceMotion: boolean }) {
  const elapsed = nowMs - since
  const visible = pressedKey !== null && elapsed >= 0 && elapsed < 600
  const opacity = reduceMotion ? (visible ? 1 : 0) : Math.max(0, 1 - elapsed / 600)
  return (
    <div
      data-demo-coverage-token="key-hud"
      aria-hidden="true"
      style={{
        position:       'absolute',
        bottom:         48,
        left:           '50%',
        transform:      'translateX(-50%)',
        zIndex:         60,
        opacity,
        transition:     reduceMotion ? 'none' : 'opacity 0.18s ease',
        pointerEvents:  'none',
      }}
    >
      {pressedKey && (
        <Keycap variant="acc">{pressedKey}</Keycap>
      )}
    </div>
  )
}

function GhostCursor({ cursor, reduceMotion }: { cursor: DemoCursor; reduceMotion: boolean }) {
  return (
    <svg
      data-demo-coverage-token="ghost-cursor-overlay"
      width="18"
      height="20"
      viewBox="0 0 18 20"
      aria-hidden="true"
      style={{
        position:      'absolute',
        left:          0,
        top:           0,
        transform:     `translate(${cursor.x}%, ${cursor.y}%) translate(-2px, -2px)`,
        opacity:       cursor.visible ? 1 : 0,
        transition:    reduceMotion ? 'none' : 'transform 0.24s ease, opacity 0.18s ease',
        zIndex:        70,
        pointerEvents: 'none',
        filter:        'drop-shadow(0 1px 2px rgba(0,0,0,0.5))',
      }}
    >
      <path
        d="M1 1 L1 14 L5 11 L7.5 17 L10 16 L7.5 10 L13 10 Z"
        fill="var(--text)"
        stroke="var(--s0)"
        strokeWidth="0.8"
        strokeLinejoin="round"
      />
    </svg>
  )
}

function CalloutCard({ callout }: { callout: DemoCallout | null }) {
  if (!callout) return null
  return (
    <div
      data-demo-coverage-token="callout-card"
      style={{
        position:     'absolute',
        top:          56,
        right:        24,
        maxWidth:     260,
        padding:      '10px 14px',
        background:   'var(--s2)',
        border:       '0.5px solid var(--accent-rim)',
        borderRadius: 10,
        zIndex:       55,
        fontFamily:   'var(--sans)',
        boxShadow:    '0 8px 24px rgba(0,0,0,0.4)',
      }}
    >
      <div style={{
        fontFamily:    'var(--mono)',
        fontSize:      '10px',
        color:         'var(--accent)',
        letterSpacing: '0.12em',
        textTransform: 'uppercase',
        marginBottom:  6,
      }}>
        {callout.title}
      </div>
      {callout.body && (
        <div style={{ fontSize: '12px', color: 'var(--muted)', lineHeight: 1.45 }}>
          {callout.body}
        </div>
      )}
    </div>
  )
}

// ─── Surface renderers ────────────────────────────────────────────────────────

function PillSurface({ state, reduceMotion }: { state: DemoState; reduceMotion: boolean }) {
  const visible = state.surface === 'pill'
  return (
    <div
      data-demo-surface="pill"
      data-demo-coverage-token={state.pillMode === 'idle' ? 'pill-idle-mount' : 'pill-active-mount'}
      style={{
        position:      'absolute',
        inset:         0,
        display:       'flex',
        alignItems:    'flex-end',
        justifyContent: 'flex-start',
        padding:       '0 0 36px 64px',
        opacity:       visible ? 1 : 0,
        transition:    reduceMotion ? 'none' : 'opacity 0.22s ease',
        pointerEvents: visible ? 'auto' : 'none',
      }}
    >
      <PaletteShell
        scale="pill"
        pillMode={state.pillMode}
        recording={state.recording}
        convCount={2}
        query={state.query}
      />
    </div>
  )
}

function PaletteSurface({ state, reduceMotion }: { state: DemoState; reduceMotion: boolean }) {
  const visible = state.surface === 'palette' || state.surface === 'detail'
  const sections = useMemo(() => ([
    {
      label: 'Actions',
      items: [
        { id: 'open-diff', icon: '⇄', name: 'Open the diff for the last commit', meta: '⌘D', selected: true },
        { id: 'open-pr',   icon: '⎘', name: 'Open PR for current branch',           meta: '↵' },
      ],
    },
    {
      label: 'Recent',
      items: [
        { id: 'r1', icon: '·', name: 'commit: phase pill-sizing', meta: '2m' },
        { id: 'r2', icon: '·', name: 'mission: real-usage demo',  meta: '14m' },
      ],
    },
    {
      label: 'Projects',
      items: [{ id: 'p1', icon: '◐', name: 'nanika', meta: 'main' }],
    },
    {
      label: 'Tracker',
      items: [{ id: 't1', icon: '◇', name: 'TRK-558 · Nanika → T3 parity', meta: 'P1' }],
    },
    {
      label: 'Plugins',
      items: [{ id: 'pl1', icon: '◈', name: 'tracker · open issue', meta: 'plugin' }],
    },
  ]), [])

  return (
    <div
      data-demo-surface="palette"
      style={{
        position:      'absolute',
        inset:         0,
        display:       'grid',
        placeItems:    'center',
        opacity:       visible ? 1 : 0,
        transition:    reduceMotion ? 'none' : 'opacity 0.22s ease, transform 0.25s ease',
        transform:     visible ? 'scale(1)' : 'scale(0.96)',
        pointerEvents: visible ? 'auto' : 'none',
      }}
    >
      <PaletteShell
        scale="palette"
        query={state.query}
        placeholder="Search Whim…"
        recording={state.recording && state.surface === 'palette' && state.pillMode === 'voice'}
        sections={state.query.length > 0 ? sections : undefined}
      />
    </div>
  )
}

function CanvasSurface({ state, reduceMotion }: { state: DemoState; reduceMotion: boolean }) {
  const visible = state.surface === 'canvas' || state.surface === 'diff'
  return (
    <div
      data-demo-surface="canvas"
      style={{
        position:      'absolute',
        inset:         0,
        opacity:       visible ? 1 : 0,
        transition:    reduceMotion ? 'none' : 'opacity 0.28s ease, transform 0.28s ease',
        transform:     visible ? 'scale(1)' : 'scale(1.02)',
        pointerEvents: visible ? 'auto' : 'none',
        overflow:      'hidden',
      }}
    >
      <CompositeCanvas state={state.canvas} />
    </div>
  )
}

// ─── Coverage tokens (cumulative, for verification) ──────────────────────────

function CoverageTokens({ tokens }: { tokens: string[] }) {
  return (
    <div aria-hidden="true" style={{ position: 'absolute', width: 0, height: 0, overflow: 'hidden' }}>
      {tokens.map(t => (
        <span key={t} data-demo-coverage-token={t} />
      ))}
    </div>
  )
}

// ─── Synthetic event helper — keeps demonstrated keys local ──────────────────

function isDemonstratedKey(key: string): boolean {
  // Keys the demo "demonstrates" (visual only, must not leak)
  return key === '⌘J' || key === '⌘B' || key === '⌘D' || key === '⌘[' || key === '⌘]' ||
         key === 'V' || key === 'y' || key === 'n' || key === 'Y' || key === 'a' ||
         key === 'c' || key === 'r' || key === '↵' || key === '⌥Space'
}

function isControlKey(key: string): boolean {
  return key === 'ArrowRight' || key === 'ArrowLeft' || key === ' ' ||
         key === 'j' || key === 'k' || key === 'Home' || key === 'End' || key === 'Escape'
}

// ─── Main component ───────────────────────────────────────────────────────────

export function RealUsageDemo() {
  const [state, dispatch] = useReducer(reducer, INITIAL_DEMO_STATE)
  const stateRef    = useRef(state)
  const playingRef  = useRef(false)
  const tickTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const stepTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  stateRef.current = state

  // prefers-reduced-motion
  useEffect(() => {
    if (typeof window === 'undefined' || !window.matchMedia) return
    const mq = window.matchMedia('(prefers-reduced-motion: reduce)')
    const sync = () => dispatch({ type: 'reduce-motion', on: mq.matches })
    sync()
    mq.addEventListener?.('change', sync)
    return () => mq.removeEventListener?.('change', sync)
  }, [])

  // Single-mount sanity check (dev only)
  useEffect(() => {
    if (import.meta.env.DEV) {
      // eslint-disable-next-line no-console
      console.count('demo-root-mounted')
    }
  }, [])

  const clearTimers = () => {
    if (tickTimerRef.current) { clearTimeout(tickTimerRef.current); tickTimerRef.current = null }
    if (stepTimerRef.current) { clearTimeout(stepTimerRef.current); stepTimerRef.current = null }
  }

  const goto = (index: number) => {
    clearTimers()
    const i = Math.max(0, Math.min(index, DEMO_SCRIPT.length - 1))
    dispatch({ type: 'goto', index: i, nowMs: performance.now() })
  }

  const next = () => goto(stateRef.current.stepIndex + 1)
  const prev = () => goto(stateRef.current.stepIndex - 1)

  // Schedule ticks + autoplay advance
  useEffect(() => {
    clearTimers()
    const step = DEMO_SCRIPT[state.stepIndex]
    if (step.ticks?.length) {
      const div = state.reduceMotion ? 4 : 1
      step.ticks.forEach(tick => {
        const t = setTimeout(() => {
          dispatch({ type: 'tick', patch: tick.patch })
        }, tick.atMs / div)
        // we don't track each individually; clearTimers fires only on goto
        void t
      })
    }
    if (playingRef.current) {
      const dur = (step.durationMs ?? 2000) / (state.reduceMotion ? 2 : 1)
      stepTimerRef.current = setTimeout(() => {
        const nextIdx = stateRef.current.stepIndex + 1
        if (nextIdx >= DEMO_SCRIPT.length) {
          playingRef.current = false
          dispatch({ type: 'play', playing: false })
        } else {
          goto(nextIdx)
        }
      }, dur)
    }
    return clearTimers
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.stepIndex, state.playing, state.reduceMotion])

  const togglePlay = () => {
    const next = !playingRef.current
    playingRef.current = next
    dispatch({ type: 'play', playing: next })
  }

  // Keyboard handler — capture phase + stopPropagation so demonstrated keys
  // don't leak to other scenarios' window-level handlers
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const k = e.key
      const chord =
        e.metaKey && k.toLowerCase() === 'j' ? '⌘J' :
        e.metaKey && k.toLowerCase() === 'b' ? '⌘B' :
        e.metaKey && k.toLowerCase() === 'd' ? '⌘D' :
        null

      // Demonstrated chords (⌘J ⌘B ⌘D) — eat them so they don't leak to
      // global handlers in adjacent scenarios that share the page.
      if (chord && isDemonstratedKey(chord)) {
        e.preventDefault()
        e.stopPropagation()
        dispatch({ type: 'press', key: chord, nowMs: performance.now() })
        return
      }

      // Single-letter demonstrated keys also eaten.
      if (!e.metaKey && isDemonstratedKey(k)) {
        e.preventDefault()
        e.stopPropagation()
        dispatch({ type: 'press', key: k, nowMs: performance.now() })
        return
      }

      if (!isControlKey(k)) return
      e.preventDefault()
      e.stopPropagation()

      if (k === 'ArrowRight' || k === 'j') next()
      else if (k === 'ArrowLeft' || k === 'k') prev()
      else if (k === ' ') togglePlay()
      else if (k === 'Home') goto(0)
      else if (k === 'End')  goto(DEMO_SCRIPT.length - 1)
      else if (k === 'Escape') {
        playingRef.current = false
        dispatch({ type: 'play', playing: false })
        if (typeof window !== 'undefined') window.location.hash = '#/scenarios'
      }
    }
    window.addEventListener('keydown', handler, { capture: true })
    return () => window.removeEventListener('keydown', handler, { capture: true } as EventListenerOptions)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const step = DEMO_SCRIPT[state.stepIndex]

  // Cumulative tokens for coverage verification
  const cumulativeTokens = useMemo(() => {
    const seen = new Set<string>()
    for (let i = 0; i <= state.stepIndex; i++) {
      for (const t of DEMO_SCRIPT[i]['data-demo-coverage-tokens']) seen.add(t)
    }
    return Array.from(seen)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.stepIndex])

  // Demo-root bounds: scale interpolates within these
  const rootSize = surfaceSize(state.surface, state.pillMode, state.reduceMotion)

  return (
    <div
      style={{
        position: 'relative',
        width:    '100%',
        minHeight: '100vh',
        background: 'var(--s0)',
        overflow: 'hidden',
        fontFamily: 'var(--sans)',
      }}
    >
      <NarrationBar step={step} index={state.stepIndex} total={DEMO_SCRIPT.length} />

      <div
        data-demo-root
        data-demo-coverage-token="demo-root"
        style={{
          position:     'absolute',
          left:         '50%',
          top:          '50%',
          transform:    `translate(-50%, -50%)`,
          width:        rootSize.width,
          height:       rootSize.height,
          transition:   state.reduceMotion
            ? 'none'
            : 'width 0.28s ease, height 0.28s ease, transform 0.28s ease',
          background:   'transparent',
        }}
      >
        <PillSurface     state={state} reduceMotion={state.reduceMotion} />
        <PaletteSurface  state={state} reduceMotion={state.reduceMotion} />
        <CanvasSurface   state={state} reduceMotion={state.reduceMotion} />

        <GhostCursor cursor={state.cursor} reduceMotion={state.reduceMotion} />
        <CalloutCard callout={state.callout} />
        <CoverageTokens tokens={cumulativeTokens} />
      </div>

      <KeyHUD
        pressedKey={state.pressedKey}
        since={state.pressedAt}
        nowMs={performance.now()}
        reduceMotion={state.reduceMotion}
      />

      <ExplanationStrip step={step} />

      <ControlBar
        index={state.stepIndex}
        total={DEMO_SCRIPT.length}
        playing={state.playing}
      />
    </div>
  )
}

function ControlBar({ index, total, playing }: { index: number; total: number; playing: boolean }) {
  return (
    <div
      data-demo-coverage-token="control-bar"
      style={{
        position:    'fixed',
        bottom:      12,
        right:       16,
        fontFamily:  'var(--mono)',
        fontSize:    10,
        color:       'var(--ghost)',
        letterSpacing: '0.06em',
        background:  'var(--s1)',
        border:      '0.5px solid var(--border)',
        borderRadius: 8,
        padding:     '6px 10px',
        zIndex:      90,
        pointerEvents: 'none',
        display:     'flex',
        alignItems:  'center',
        gap:         10,
      }}
    >
      <span style={{ color: playing ? 'var(--accent)' : 'var(--faint)' }}>
        {playing ? 'AUTOPLAY' : 'STEP'}
      </span>
      <span>·</span>
      <span>{index + 1}/{total}</span>
      <span>·</span>
      <span>→ ← j k · space · home/end · esc</span>
    </div>
  )
}

// Returns the sizing for the demo-root container at each surface so the
// outer width/height interpolates and the inner content morphs.
function surfaceSize(surface: DemoSurface, pillMode: DemoState['pillMode'], _reduce: boolean): { width: CSSProperties['width']; height: CSSProperties['height'] } {
  if (surface === 'pill') {
    if (pillMode === 'idle')  return { width: 200, height: 80 }
    return { width: 280, height: 120 }
  }
  if (surface === 'palette') return { width: 860, height: 520 }
  if (surface === 'detail')  return { width: 860, height: 560 }
  // canvas / diff
  return { width: 1280, height: 720 }
}
