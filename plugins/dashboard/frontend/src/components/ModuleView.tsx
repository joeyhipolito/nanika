import { useCallback, useEffect, useRef, useState } from 'react'
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

  const [pos, setPos] = useState({ x: 0, y: 0 })
  const [minimized, setMinimized] = useState(false)
  const [maximized, setMaximized] = useState(false)
  const dragState = useRef<{ active: boolean; startX: number; startY: number; originX: number; originY: number }>({
    active: false,
    startX: 0,
    startY: 0,
    originX: 0,
    originY: 0,
  })

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
  }, [moduleId, pos])

  // Reset position and window state when module changes.
  useEffect(() => {
    setPos({ x: 0, y: 0 })
    setMinimized(false)
    setMaximized(false)
  }, [moduleId])

  const handleTitleBarMouseDown = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    // Only primary button; ignore clicks on traffic light buttons
    if (e.button !== 0) return
    if ((e.target as HTMLElement).closest('button')) return

    e.preventDefault()
    dragState.current = {
      active: true,
      startX: e.clientX,
      startY: e.clientY,
      originX: pos.x,
      originY: pos.y,
    }
  }, [pos])

  useEffect(() => {
    const onMouseMove = (e: MouseEvent) => {
      if (!dragState.current.active) return
      const el = viewRef.current
      if (!el) return

      const dx = e.clientX - dragState.current.startX
      const dy = e.clientY - dragState.current.startY
      let newX = dragState.current.originX + dx
      let newY = dragState.current.originY + dy

      // Clamp within viewport
      const rect = el.getBoundingClientRect()
      // rect reflects the current transform, so use natural (un-translated) position
      const naturalLeft = rect.left - dragState.current.originX
      const naturalTop = rect.top - dragState.current.originY
      const maxX = window.innerWidth - (naturalLeft + rect.width)
      const maxY = window.innerHeight - (naturalTop + rect.height)
      const minX = -naturalLeft
      const minY = -naturalTop

      newX = Math.min(Math.max(newX, minX), maxX)
      newY = Math.min(Math.max(newY, minY), maxY)

      setPos({ x: newX, y: newY })
    }

    const onMouseUp = () => {
      dragState.current.active = false
    }

    window.addEventListener('mousemove', onMouseMove)
    window.addEventListener('mouseup', onMouseUp)
    return () => {
      window.removeEventListener('mousemove', onMouseMove)
      window.removeEventListener('mouseup', onMouseUp)
    }
  }, [])

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
      <div
        ref={viewRef}
        className={[
          'module-view',
          minimized ? 'module-view--minimized' : '',
          maximized ? 'module-view--maximized' : '',
        ].filter(Boolean).join(' ')}
        style={{ transform: maximized ? 'none' : `translate(${pos.x}px, ${pos.y}px)` }}
      >
        <div
          className="module-view-topbar"
          onMouseDown={handleTitleBarMouseDown}
          style={{ cursor: 'grab' }}
        >
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
              onClick={() => setMinimized(m => !m)}
              aria-label={minimized ? 'Restore' : 'Minimize'}
              title={minimized ? 'Restore' : 'Minimize'}
            />
            <button
              type="button"
              className="module-view-tl module-view-tl-maximize"
              onClick={() => setMaximized(m => !m)}
              aria-label={maximized ? 'Restore' : 'Maximize'}
              title={maximized ? 'Restore' : 'Maximize'}
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
