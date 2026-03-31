import { useEffect, useRef } from 'react'
import { getModule } from '../modules/registry'
import { setInteractiveBounds } from '../lib/wails'
import './ModuleView.css'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface ModuleViewProps {
  moduleId: string | null
  isConnected: boolean
  onClose: () => void
}

// ---------------------------------------------------------------------------
// ModuleView
// ---------------------------------------------------------------------------

export function ModuleView({ moduleId, isConnected, onClose }: ModuleViewProps) {
  const mod = moduleId ? getModule(moduleId) : null
  const viewRef = useRef<HTMLDivElement>(null)

  // Report module panel bounds to the Go overlay so its region is interactive.
  useEffect(() => {
    const el = viewRef.current
    if (!el) return
    requestAnimationFrame(() => {
      const rect = el.getBoundingClientRect()
      const screenH = window.innerHeight
      setInteractiveBounds(
        rect.left,
        screenH - (rect.top + rect.height),
        rect.width,
        rect.height,
      )
    })
  }, [moduleId])

  if (!mod) return null

  const ModuleComponent = mod.component
  const Icon = mod.icon

  return (
    <div
      className="module-view-overlay"
      role="dialog"
      aria-modal="true"
      aria-label={mod.name}
    >
      <div ref={viewRef} className="module-view">
        <div className="module-view-topbar">
          <div className="module-view-traffic-lights" aria-hidden="true">
            <button
              type="button"
              className="module-view-tl module-view-tl-close"
              onClick={onClose}
              aria-label="Close (Escape)"
              title="Close"
            />
            <button
              type="button"
              className="module-view-tl module-view-tl-minimize"
              aria-label="Minimize"
              title="Minimize"
            />
            <button
              type="button"
              className="module-view-tl module-view-tl-maximize"
              aria-label="Maximize"
              title="Maximize"
            />
          </div>
          <div className="module-view-title">
            <span className="module-view-title-icon" aria-hidden="true">
              <Icon size={14} />
            </span>
            <span>{mod.name}</span>
          </div>
          <div className="module-view-topbar-spacer" aria-hidden="true" />
        </div>
        <div className="module-view-body">
          <ModuleComponent isConnected={isConnected} />
        </div>
      </div>
    </div>
  )
}
