import { useEffect, useRef } from 'react'

export interface SpotlightRingProps {
  target: string
  padding?: number
  strokeWidth?: number
  strokeToken?: string
  reduceMotion?: boolean
}

// Motion CSS injected once
const RING_CSS = `
.tour-spotlight-ring {
  position: fixed;
  top: 0;
  left: 0;
  pointer-events: none;
  border-radius: 10px;
  z-index: 8500;
  transition-property: opacity, transform, width, height;
  transition-timing-function: cubic-bezier(0.2, 0.8, 0.2, 1);
  transition-duration: 220ms;
}
.tour-spotlight-ring[data-mounted="true"] {
  opacity: 1;
}
.tour-spotlight-ring[data-mounted="false"] {
  opacity: 0;
}
`

export function SpotlightRing({
  target,
  padding = 6,
  strokeWidth = 2,
  strokeToken = '--accent',
  reduceMotion = false,
}: SpotlightRingProps) {
  const ringRef = useRef<HTMLDivElement>(null)
  const warnedRef = useRef<Set<string>>(new Set())

  // Inject CSS once
  useEffect(() => {
    if (document.getElementById('tour-spotlight-css')) return
    const el = document.createElement('style')
    el.id = 'tour-spotlight-css'
    el.textContent = RING_CSS
    document.head.appendChild(el)
  }, [])

  useEffect(() => {
    const ring = ringRef.current
    if (!ring) return

    const el = document.querySelector(target) as HTMLElement | null
    if (!el) {
      if (!warnedRef.current.has(target)) {
        console.warn(`[SpotlightRing] target not found: ${target}`)
        warnedRef.current.add(target)
      }
      ring.dataset.mounted = 'false'
      return
    }

    // Animate mount
    ring.dataset.mounted = 'false'
    const mountRaf = requestAnimationFrame(() => {
      ring.dataset.mounted = 'true'
    })

    if (reduceMotion) {
      // Snap once — no RAF loop
      const r = el.getBoundingClientRect()
      ring.style.transform = `translate(${r.left - padding}px, ${r.top - padding}px)`
      ring.style.width     = `${r.width  + 2 * padding}px`
      ring.style.height    = `${r.height + 2 * padding}px`
      return () => {
        cancelAnimationFrame(mountRaf)
        ring.dataset.mounted = 'false'
      }
    }

    // RAF tracking loop
    let rafId: number
    const tick = () => {
      const r = el.getBoundingClientRect()
      ring.style.transform = `translate(${r.left - padding}px, ${r.top - padding}px)`
      ring.style.width     = `${r.width  + 2 * padding}px`
      ring.style.height    = `${r.height + 2 * padding}px`
      rafId = requestAnimationFrame(tick)
    }
    rafId = requestAnimationFrame(tick)

    return () => {
      cancelAnimationFrame(mountRaf)
      cancelAnimationFrame(rafId)
      ring.dataset.mounted = 'false'
    }
  }, [target, padding, reduceMotion])

  return (
    <div
      ref={ringRef}
      className="tour-spotlight-ring"
      data-mounted="false"
      aria-hidden="true"
      style={{
        border:          `${strokeWidth}px solid var(${strokeToken})`,
        boxShadow:       `0 0 0 1px rgba(0,0,0,0.3), 0 0 12px rgba(212,120,86,0.25)`,
        transitionDuration: reduceMotion ? '16ms' : '220ms',
      }}
    />
  )
}
