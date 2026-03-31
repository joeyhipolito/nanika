import { Component, createElement, useEffect, useRef } from 'react'
import type { ComponentType, ReactNode } from 'react'
import { registerModule } from '../modules/registry'
import { PluginModule, PlugIcon } from '../components/PluginModule'
import type { PluginInfo, PluginViewProps } from '../types'
import { listPlugins, getPluginUIBundle } from '../lib/wails'
import { iconMap } from '../lib/plugin-icons'

// ---------------------------------------------------------------------------
// Error boundary — prevents a crash in one plugin UI from taking down the
// entire dashboard. Falls back to the generic PluginModule on render errors.
// ---------------------------------------------------------------------------

interface ErrorBoundaryProps {
  pluginName: string
  children: ReactNode
}

interface ErrorBoundaryState {
  hasError: boolean
}

class PluginErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  constructor(props: ErrorBoundaryProps) {
    super(props)
    this.state = { hasError: false }
  }

  static getDerivedStateFromError(): ErrorBoundaryState {
    return { hasError: true }
  }

  componentDidCatch(error: Error) {
    console.error(`[plugin:${this.props.pluginName}] render error:`, error)
  }

  render() {
    if (this.state.hasError) {
      return createElement(PluginModule, { pluginName: this.props.pluginName })
    }
    return this.props.children
  }
}

// ---------------------------------------------------------------------------
// debugOverlay — appends a visible log line to a fixed overlay div in the DOM.
// Useful for debugging plugin load issues in the Wails WebView where DevTools
// may not be immediately accessible.
// ---------------------------------------------------------------------------

function debugOverlay(pluginName: string, message: string) {
  console.log(`[plugin-loader:${pluginName}] ${message}`)
}

// ---------------------------------------------------------------------------
// Per-plugin component factory — wraps a custom or generic view with an error
// boundary so a crash in one plugin doesn't affect the rest of the dashboard.
// ---------------------------------------------------------------------------

function makePluginComponent(
  pluginName: string,
  CustomView?: ComponentType<PluginViewProps>,
  loadError?: string,
) {
  function PluginModulePanel({ isConnected }: { isConnected?: boolean }) {
    if (CustomView) {
      return createElement(
        PluginErrorBoundary,
        { pluginName },
        createElement(CustomView, { isConnected }),
      )
    }
    return createElement(PluginModule, { pluginName, isConnected, loadError })
  }
  PluginModulePanel.displayName = `PluginModulePanel(${pluginName})`
  return PluginModulePanel
}

// ---------------------------------------------------------------------------
// loadPluginBundle — fetches the plugin's prebuilt JS bundle, creates a blob
// URL, and dynamic-imports it to extract the default export component.
// Returns { component, error } — component is null on any failure.
// ---------------------------------------------------------------------------

interface BundleResult {
  component: ComponentType<PluginViewProps> | null
  error: string | null
}

async function loadPluginBundle(name: string): Promise<BundleResult> {
  debugOverlay(name, `loadPluginBundle called`)
  debugOverlay(name, `window.__nanika_shared__ present: ${typeof (window as any).__nanika_shared__ !== 'undefined' && (window as any).__nanika_shared__ != null}`)
  try {
    const source = await getPluginUIBundle(name)
    debugOverlay(name, `bundle source length: ${source?.length ?? 0}`)
    const blob = new Blob([source], { type: 'application/javascript' })
    const url = URL.createObjectURL(blob)
    debugOverlay(name, `blob URL created: ${url}`)
    try {
      debugOverlay(name, `starting dynamic import()`)
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const mod = await import(/* @vite-ignore */ url) as any
      const modType = typeof mod
      const defaultType = typeof mod?.default
      debugOverlay(name, `import() succeeded — mod type: ${modType}, mod.default type: ${defaultType}, mod keys: ${mod ? Object.keys(mod).join(', ') : 'null'}`)
      debugOverlay(name, `window.__nanika_plugin__ after import: ${JSON.stringify((window as any).__nanika_plugin__)}`)
      const Comp = mod?.default
      if (typeof Comp !== 'function') {
        const err = `bundle default export is not a component (mod type: ${modType}, default type: ${defaultType})`
        debugOverlay(name, `ERROR: ${err}`)
        return { component: null, error: err }
      }
      debugOverlay(name, `component loaded successfully`)
      return { component: Comp as ComponentType<PluginViewProps>, error: null }
    } catch (importErr) {
      const msg = importErr instanceof Error ? `${importErr.message}\n${importErr.stack ?? ''}` : String(importErr)
      debugOverlay(name, `import() threw: ${msg}`)
      return { component: null, error: msg }
    } finally {
      URL.revokeObjectURL(url)
    }
  } catch (err) {
    const msg = err instanceof Error ? `${err.message}\n${err.stack ?? ''}` : String(err)
    debugOverlay(name, `load error: ${msg}`)
    return { component: null, error: msg }
  }
}

// ---------------------------------------------------------------------------
// usePlugins
//
// Runs on app mount. Calls window.go.main.App.ListPlugins() (via wails bridge)
// and registers each discovered plugin (api_version >= 1) as a dashboard module.
// Plugins with ui:true get their prebuilt bundle loaded at runtime via dynamic
// import(); all others fall back to the generic PluginModule.
//
// Re-registration of the same plugin name is idempotent (skipped via ref set).
// Status/items are NOT polled here — each PluginModule manages its own poll.
// ---------------------------------------------------------------------------

export function usePlugins(): void {
  const registeredRef = useRef(new Set<string>())

  useEffect(() => {
    let cancelled = false

    async function discover() {
      try {
        const plugins = await listPlugins() as PluginInfo[]
        if (cancelled) return

        for (const plugin of plugins) {
          if (registeredRef.current.has(plugin.name)) continue
          registeredRef.current.add(plugin.name)

          const displayName = plugin.name.charAt(0).toUpperCase() + plugin.name.slice(1)
          const keywords = [
            plugin.name,
            ...(plugin.tags ?? []),
            ...(plugin.provides ?? []),
            'plugin',
          ]

          const icon = (plugin.icon && iconMap[plugin.icon]) ?? PlugIcon

          // Load custom UI bundle at runtime if the plugin declares ui:true.
          // Falls back to generic PluginModule on any load error.
          let CustomView: ComponentType<PluginViewProps> | undefined
          let loadError: string | undefined
          debugOverlay(plugin.name, `plugin.ui=${plugin.ui}`)
          if (plugin.ui) {
            const result = await loadPluginBundle(plugin.name)
            CustomView = result.component ?? undefined
            loadError = result.error ?? undefined
            debugOverlay(plugin.name, CustomView ? 'using custom UI' : `falling back to generic (${loadError ?? 'unknown'})`)
          }

          registerModule({
            id: `plugin:${plugin.name}`,
            name: displayName,
            description: plugin.description,
            icon,
            keywords,
            component: makePluginComponent(plugin.name, CustomView, loadError),
          })
        }
      } catch {
        // network unavailable or wails not ready — silently ignore
      }
    }

    discover()
    return () => { cancelled = true }
  }, []) // run once on mount
}
