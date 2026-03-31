// vite-plugin-nanika-shared.ts — Vite plugin for plugin UI builds.
//
// Resolves shared module imports to synthetic virtual modules that read from
// window.__nanika_shared__ at runtime. This lets plugin bundles be built as
// ES modules with no external import() dependencies — every import of 'react'
// becomes a local const binding to window.__nanika_shared__.react, which the
// dashboard initializes on App mount.
//
// GOTCHA: This plugin must be listed before @vitejs/plugin-react in the
// plugins array, otherwise the react plugin processes the imports first.
//
// DECISION: Virtual module approach (vs Rollup globals + IIFE) keeps output
// as a true ES module so dynamic import() works in the Wails webview without
// import maps or extra server configuration.

import type { Plugin } from 'vite'

interface SharedModuleConfig {
  /** Expression on window.__nanika_shared__ to bind as default export */
  global: string
  /** Named exports to destructure from the global */
  exports: string[]
}

// FINDING: React 18 exposes all hooks and utilities on the default export
// object, so a single const binding + destructure covers all common imports.
// GOTCHA: __SECRET_INTERNALS_DO_NOT_USE_OR_YOU_WILL_BE_FIRED must be included
// here so the bundled react/jsx-runtime (which is NOT shimmed directly) can
// access He.__SECRET_INTERNALS_DO_NOT_USE_OR_YOU_WILL_BE_FIRED via the jr()
// interop helper in plugin bundles. Without this, any bundle built with
// @vitejs/plugin-react crashes at module init because the jsx-runtime reads
// this internal from the shimmed React namespace object.
const REACT_NAMED_EXPORTS = [
  // Hooks
  'useState', 'useEffect', 'useCallback', 'useMemo', 'useRef',
  'useContext', 'useReducer', 'useLayoutEffect', 'useImperativeHandle',
  'useDebugValue', 'useId', 'useSyncExternalStore', 'useTransition',
  'useDeferredValue', 'useInsertionEffect',
  // Component utilities
  'createContext', 'forwardRef', 'memo', 'lazy', 'Suspense', 'StrictMode',
  'Fragment', 'createElement', 'cloneElement', 'isValidElement',
  'Children', 'Component', 'PureComponent', 'createRef', 'createPortal',
  'startTransition', 'version',
  // React internals — required by bundled react/jsx-runtime at module init.
  // The jsx-runtime calls He.__SECRET_INTERNALS_DO_NOT_USE_OR_YOU_WILL_BE_FIRED
  // where He = jr(_r) (the Rollup interop-processed React namespace). Without
  // this export, He lacks the key and the bundle crashes before rendering.
  '__SECRET_INTERNALS_DO_NOT_USE_OR_YOU_WILL_BE_FIRED',
]

const REACT_DOM_NAMED_EXPORTS = [
  'createPortal', 'findDOMNode', 'flushSync', 'render',
  'unmountComponentAtNode', 'unstable_batchedUpdates',
]

const REACT_DOM_CLIENT_NAMED_EXPORTS = [
  'createRoot', 'hydrateRoot',
]

const NANIKA_UI_EXPORTS = [
  'Button', 'buttonVariants',
  'Badge', 'badgeVariants',
  'Card', 'CardHeader', 'CardFooter', 'CardTitle', 'CardDescription', 'CardContent',
  'Tabs', 'TabsList', 'TabsTrigger', 'TabsContent',
  'cn',
]

const NANIKA_WAILS_EXPORTS = [
  'isWails', 'wailsRuntime',
  'setInteractiveBounds', 'setFullClickThrough',
  'listPlugins', 'queryPluginStatus', 'queryPluginItems', 'pluginAction',
  'listMissions', 'getMissionDetail', 'getMissionEvents', 'getMissionDAG',
  'cancelMission', 'runMission',
  'listPersonas', 'getPersonaDetail', 'reloadPersonas',
  'getMetrics', 'getFindings',
  'listScanners', 'nenScan', 'cleanup',
  'getChannelStatus', 'checkHealth',
]

const SHARED_MODULES: Record<string, SharedModuleConfig> = {
  'react': {
    global: 'window.__nanika_shared__.react',
    exports: REACT_NAMED_EXPORTS,
  },
  'react-dom': {
    global: 'window.__nanika_shared__.reactDom',
    exports: REACT_DOM_NAMED_EXPORTS,
  },
  'react-dom/client': {
    global: 'window.__nanika_shared__.reactDomClient',
    exports: REACT_DOM_CLIENT_NAMED_EXPORTS,
  },
  // @nanika/ui and @nanika/wails are virtual package names — no real package
  // needs to be installed. Plugin authors use these as import aliases.
  '@nanika/ui': {
    global: 'window.__nanika_shared__.ui',
    exports: NANIKA_UI_EXPORTS,
  },
  '@nanika/wails': {
    global: 'window.__nanika_shared__.wails',
    exports: NANIKA_WAILS_EXPORTS,
  },
  // Dashboard @/ path aliases — intercept the conventional dashboard import
  // paths so plugin bundles that use @/components/ui/* and @/lib/wails resolve
  // to the same shared globals without bundling duplicates.
  '@/components/ui/button': {
    global: 'window.__nanika_shared__.ui',
    exports: NANIKA_UI_EXPORTS,
  },
  '@/components/ui/badge': {
    global: 'window.__nanika_shared__.ui',
    exports: NANIKA_UI_EXPORTS,
  },
  '@/components/ui/card': {
    global: 'window.__nanika_shared__.ui',
    exports: NANIKA_UI_EXPORTS,
  },
  '@/components/ui/tabs': {
    global: 'window.__nanika_shared__.ui',
    exports: NANIKA_UI_EXPORTS,
  },
  '@/lib/wails': {
    global: 'window.__nanika_shared__.wails',
    exports: NANIKA_WAILS_EXPORTS,
  },
}

const PREFIX = '\0nanika-shared:'

function generateSyntheticModule(config: SharedModuleConfig): string {
  const lines: string[] = [
    `const _m = ${config.global};`,
    'export default _m;',
    ...config.exports.map((name) => `export const ${name} = _m?.${name};`),
  ]
  return lines.join('\n')
}

export function nanikaSharedPlugin(): Plugin {
  return {
    name: 'nanika-shared',
    enforce: 'pre',

    resolveId(id) {
      if (id in SHARED_MODULES) {
        return PREFIX + id
      }
    },

    load(id) {
      if (!id.startsWith(PREFIX)) return
      const moduleId = id.slice(PREFIX.length)
      const config = SHARED_MODULES[moduleId]
      if (!config) return
      return generateSyntheticModule(config)
    },
  }
}
