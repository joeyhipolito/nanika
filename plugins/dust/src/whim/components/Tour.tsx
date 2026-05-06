import { useState, useEffect, useRef, useCallback } from 'react'
import type { TourStep, TourScene } from '../tour/types'
import { TourStepper } from './TourStepper'
import { TourCallout } from './TourCallout'
import { SpotlightRing } from './SpotlightRing'

// callout-in keyframe injected once
const TOUR_CSS = `
@keyframes callout-in {
  from { opacity: 0; transform: translateY(-6px); }
  to   { opacity: 1; transform: translateY(0); }
}
`

export interface TourProps {
  steps: TourStep[]
  initialIndex?: number
  onScene(scene: TourScene): void
  onExit(): void
  reduceMotion?: boolean
  freezeTimers?: boolean
}

type TourStatus = 'playing' | 'paused' | 'idle'

export function Tour({
  steps,
  initialIndex = 0,
  onScene,
  onExit,
  reduceMotion,
  freezeTimers = false,
}: TourProps) {
  const [index,  setIndex]  = useState(initialIndex)
  const [status, setStatus] = useState<TourStatus>('idle')
  const lastSceneKindRef    = useRef<TourScene['kind']>(steps[initialIndex]?.scene.kind ?? 'pill')
  const timerRef            = useRef<ReturnType<typeof setTimeout> | null>(null)
  const spaceHeldRef        = useRef(false)
  const spaceTimerRef       = useRef<ReturnType<typeof setTimeout> | null>(null)

  // Resolve reduceMotion from system when not passed in
  const rm = reduceMotion ?? (
    typeof window !== 'undefined'
      ? window.matchMedia('(prefers-reduced-motion: reduce)').matches
      : false
  )

  // Inject CSS once
  useEffect(() => {
    if (document.getElementById('tour-css')) return
    const el = document.createElement('style')
    el.id = 'tour-css'
    el.textContent = TOUR_CSS
    document.head.appendChild(el)
  }, [])

  // Drive the host scene whenever index changes
  useEffect(() => {
    if (!steps[index]) return
    const scene = steps[index].scene
    lastSceneKindRef.current = scene.kind
    onScene({ kind: scene.kind, state: scene.state })
    setStatus(s => s === 'idle' ? 'paused' : s)
  }, [index]) // eslint-disable-line react-hooks/exhaustive-deps

  // Auto-advance timer
  useEffect(() => {
    if (freezeTimers) return
    if (status !== 'playing') { clearTimer(); return }

    const durationMs = steps[index]?.durationMs ?? 5000
    timerRef.current = setTimeout(() => {
      setIndex(i => Math.min(i + 1, steps.length - 1))
    }, durationMs)

    return clearTimer
  }, [index, status, freezeTimers]) // eslint-disable-line react-hooks/exhaustive-deps

  function clearTimer() {
    if (timerRef.current) { clearTimeout(timerRef.current); timerRef.current = null }
  }

  const advance = useCallback(() => {
    clearTimer()
    setIndex(i => Math.min(i + 1, steps.length - 1))
  }, [steps.length])

  const back = useCallback(() => {
    clearTimer()
    setIndex(i => Math.max(i - 1, 0))
  }, [])

  const togglePause = useCallback(() => {
    setStatus(s => s === 'playing' ? 'paused' : 'playing')
  }, [])

  // Key handler — registered at document level with capture so we fire before substrate
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      switch (e.key) {
        case 'ArrowRight':
          advance(); e.preventDefault(); break
        case 'ArrowLeft':
          back(); e.preventDefault(); break
        case ' ':
          e.preventDefault()
          // Held ≥ 600 ms → enable auto-advance
          if (!spaceHeldRef.current) {
            spaceHeldRef.current = true
            spaceTimerRef.current = setTimeout(() => {
              setStatus('playing')
            }, 600)
          }
          break
        case 'Escape':
          onExit(); e.preventDefault(); break
        case 'j':
          advance(); e.preventDefault(); break
        case 'k':
          back(); e.preventDefault(); break
        default:
          break
      }
    }

    const onKeyUp = (e: KeyboardEvent) => {
      if (e.key === ' ') {
        e.preventDefault()
        if (spaceTimerRef.current) {
          clearTimeout(spaceTimerRef.current)
          spaceTimerRef.current = null
          if (!spaceHeldRef.current) {
            // Short tap: toggle pause
            togglePause()
          }
        }
        spaceHeldRef.current = false
        setStatus(s => s === 'playing' ? 'paused' : s)
      }
    }

    document.addEventListener('keydown', onKeyDown, { capture: true })
    document.addEventListener('keyup',   onKeyUp,   { capture: true })
    return () => {
      document.removeEventListener('keydown', onKeyDown, { capture: true })
      document.removeEventListener('keyup',   onKeyUp,   { capture: true })
    }
  }, [advance, back, togglePause, onExit])

  // Parse launch query params from hash (#/s/tour?tour=full | ?step=N) or location.search
  useEffect(() => {
    const hash = window.location.hash
    const qIdx = hash.indexOf('?')
    const params = new URLSearchParams(qIdx >= 0 ? hash.slice(qIdx + 1) : window.location.search)
    if (params.get('tour') === 'full') setStatus('playing')
  }, [])

  useEffect(() => {
    const hash = window.location.hash
    const qIdx = hash.indexOf('?')
    const params = new URLSearchParams(qIdx >= 0 ? hash.slice(qIdx + 1) : window.location.search)
    const stepN = parseInt(params.get('step') ?? '', 10)
    if (Number.isFinite(stepN)) setIndex(Math.max(0, Math.min(stepN - 1, steps.length - 1)))
  }, [])

  // Clean up timers on unmount
  useEffect(() => () => {
    clearTimer()
    if (spaceTimerRef.current) clearTimeout(spaceTimerRef.current)
  }, [])

  if (!steps.length) return null

  const step = steps[index]

  // Build segment list for stepper
  const segments = steps.map(s => ({
    id:      s.id,
    chapter: s.chapter,
    title:   s.title,
  }))

  // Resolve spotlight rect for callout placement
  const targetEl = document.querySelector(step.spotlightTarget)
  const spotlightRect = targetEl ? targetEl.getBoundingClientRect() : null

  return (
    <>
      <TourStepper
        segments={segments}
        active={index}
        onJump={(i) => { clearTimer(); setIndex(i) }}
        reduceMotion={rm}
      />
      <TourCallout
        step={step}
        totalSteps={steps.length}
        index={index}
        onNext={advance}
        onBack={back}
        onExit={onExit}
        spotlightRect={spotlightRect}
        reduceMotion={rm}
      />
      <SpotlightRing
        target={step.spotlightTarget}
        reduceMotion={rm}
      />
    </>
  )
}
