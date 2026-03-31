import { useState, useEffect, useCallback, useMemo, useRef } from 'react'
import { initSharedModules } from './lib/shared-modules'
import { DashboardStateContext } from './components/dashboardContext'
import { useChat } from './hooks/useChat'
import { useOrchestrator } from './hooks/useOrchestrator'
import { useCommandBridge } from './hooks/useCommandBridge'
import { useToast } from './hooks/useToast'
import { usePlugins } from './hooks/usePlugins'
import { runMissionOnce } from './hooks/useMissions'
import { setFullClickThrough, setInteractiveBounds, nenScan, cleanup, reloadPersonas } from './lib/wails'
import { ToastStack } from './components/Toast'
import { ModuleView } from './components/ModuleView'
import { CommandPaletteProvider, CommandPalettePortal, type ActionBridge } from './components/CommandPalette'
import { useCommandPalette, type ConversationBridge } from './components/commandPaletteContext'
import './App.css'

const MODULE_KEYS: Record<string, string> = {
  '1': 'missions',
  '2': 'metrics',
  '3': 'personas',
  '4': 'findings',
  '5': 'events',
  '6': 'settings',
}

interface AppInnerProps {
  activeModuleId: string | null
  openModule: (id: string) => void
  closeModule: () => void
}

function AppInner({ activeModuleId, openModule, closeModule }: AppInnerProps) {
  const {
    isOpen: isPaletteOpen,
    open: openPalette,
    openConversation,
    close: closePalette,
  } = useCommandPalette()
  const { isConnected } = useOrchestrator()
  const chat = useChat()
  const { toasts, addToast, dismissToast } = useToast()
  usePlugins()

  // Manage click-through state based on UI visibility.
  // - Both dismissed → full pass-through (invisible)
  // - Palette closed but module still open → re-report module bounds (palette
  //   may have overwritten them while it was overlaid on the module)
  // - Palette open → bounds reported by CommandPaletteOverlay on mount
  useEffect(() => {
    if (!isPaletteOpen && activeModuleId === null) {
      setFullClickThrough()
    } else if (!isPaletteOpen && activeModuleId !== null) {
      // Re-report module bounds after palette overlay closes
      requestAnimationFrame(() => {
        const el = document.querySelector<HTMLElement>('.module-view')
        if (!el) return
        const rect = el.getBoundingClientRect()
        const screenH = window.innerHeight
        setInteractiveBounds(
          rect.left,
          screenH - (rect.top + rect.height),
          rect.width,
          rect.height,
        )
      })
    }
  }, [isPaletteOpen, activeModuleId])

  const actionBridge = useMemo<ActionBridge>(() => ({
    runMission: (task) => runMissionOnce(task),
    triggerScan: async () => {
      try {
        return await nenScan()
      } catch (err) {
        return { output: '', error: err instanceof Error ? err.message : 'Scan failed' }
      }
    },
    triggerCleanup: async () => {
      try {
        return await cleanup()
      } catch (err) {
        return { output: '', error: err instanceof Error ? err.message : 'Cleanup failed' }
      }
    },
    reloadPersonas: async () => {
      try {
        return await reloadPersonas()
      } catch {
        return false
      }
    },
    addToast,
  }), [addToast])

  const openModuleFromPalette = useCallback((id: string) => {
    closePalette(true) // skip hideWindow — module panel is taking over the window
    openModule(id)
  }, [closePalette, openModule])

  const closeModuleToPalette = useCallback(() => {
    closeModule()
    openPalette()
  }, [closeModule, openPalette])

  useCommandBridge({
    onNavigate(page) {
      if (page === 'missions') {
        openModuleFromPalette('missions')
      } else if (page === 'conversations') {
        openModuleFromPalette('conversations')
      }
    },
    onFocus() {
      openModuleFromPalette('missions')
    },
    onDismiss() {
      closeModule()
      closePalette()
    },
    onSetInput(text) {
      closeModule()
      openConversation(text)
    },
    onNotify(text, type) {
      addToast(text, type)
    },
  })

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && MODULE_KEYS[e.key]) {
        e.preventDefault()
        openModuleFromPalette(MODULE_KEYS[e.key])
        return
      }
      if (e.key === 'Escape' && !isPaletteOpen && activeModuleId !== null) {
        e.preventDefault()
        closeModuleToPalette()
      }
    }
    document.addEventListener('keydown', handler)
    return () => document.removeEventListener('keydown', handler)
  }, [activeModuleId, closeModuleToPalette, isPaletteOpen, openModuleFromPalette])

  return (
    <div className="canvas">
      <ModuleView
        moduleId={activeModuleId}
        isConnected={isConnected}
        onClose={closeModuleToPalette}
      />

      <ToastStack toasts={toasts} onDismiss={dismissToast} />

      <CommandPalettePortal
        onOpenModule={openModuleFromPalette}
        conversationBridge={{
          submit: chat.handleSubmit,
          messages: chat.messages,
        } satisfies ConversationBridge}
        actionBridge={actionBridge}
      />
    </div>
  )
}

function App() {
  // Expose shared modules on window.__nanika_shared__ synchronously — must NOT
  // be in a useEffect because React runs child effects before parent effects.
  // usePlugins() fires in AppInner (a child), so a parent useEffect would run
  // after loadPluginBundle() already tried to read window.__nanika_shared__.
  initSharedModules()

  const [activeModuleId, setActiveModuleId] = useState<string | null>(null)
  const previousModuleIdRef = useRef<string | null>(null)
  const [previousModuleId, setPreviousModuleId] = useState<string | null>(null)

  const openModule = useCallback((id: string) => {
    setPreviousModuleId(previousModuleIdRef.current)
    previousModuleIdRef.current = id
    setActiveModuleId(id)
  }, [])
  const closeModule = useCallback(() => setActiveModuleId(null), [])

  return (
    <DashboardStateContext.Provider value={{ activeModuleId, previousModuleId, openModule }}>
      <CommandPaletteProvider>
        <AppInner
          activeModuleId={activeModuleId}
          openModule={openModule}
          closeModule={closeModule}
        />
      </CommandPaletteProvider>
    </DashboardStateContext.Provider>
  )
}

export default App
