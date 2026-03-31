// plugin-vite.config.ts — reusable Vite config factory for plugin UI builds.
//
// Usage in a plugin's vite.config.ts:
//
//   import { createPluginViteConfig } from '../../dashboard/frontend/plugin-vite.config'
//
//   export default createPluginViteConfig({ pluginDir: __dirname })
//
// The factory wires up:
//  - nanikaSharedPlugin() so react/react-dom/@nanika/* resolve to window.__nanika_shared__
//  - @vitejs/plugin-react for JSX/TSX transformation
//  - lib mode output as a single ES module (index.js)
//
// DECISION: No Tailwind in plugin bundles by default. Dashboard CSS variables
// are already in the global stylesheet so Tailwind utilities work without a
// separate Tailwind pass in each plugin. If a plugin ships its own Tailwind
// config, pass tailwindcss() in extraPlugins.

import { defineConfig, type UserConfig, type Plugin } from 'vite'
import react from '@vitejs/plugin-react'
import path from 'path'
import { nanikaSharedPlugin } from './vite-plugin-nanika-shared'

export interface PluginViteConfigOptions {
  /** Absolute path to the plugin's ui directory (where vite.config.ts lives) */
  pluginDir: string
  /** Entry file relative to pluginDir. Defaults to 'index.tsx'. */
  entry?: string
  /** Output directory relative to pluginDir. Defaults to 'dist'. */
  outDir?: string
  /** Additional Vite plugins to merge (e.g. tailwindcss() for standalone styling). */
  extraPlugins?: Plugin[]
}

export function createPluginViteConfig(options: PluginViteConfigOptions): UserConfig {
  const {
    pluginDir,
    entry = 'index.tsx',
    outDir = 'dist',
    extraPlugins = [],
  } = options

  return defineConfig({
    plugins: [
      // nanikaSharedPlugin must be first — it intercepts import IDs before
      // the react plugin gets a chance to process them.
      nanikaSharedPlugin(),
      react(),
      ...extraPlugins,
    ],

    // GOTCHA: Vite replaces process.env.NODE_ENV in app builds automatically,
    // but NOT in library (lib) builds. Without this define, the bundled
    // react-jsx-runtime CJS shim emits `process.env.NODE_ENV === "production"`
    // verbatim, which throws ReferenceError in WebKit (no `process` global).
    // Setting it here makes Vite replace it with "production" and dead-code
    // eliminate the development jsx-runtime branch entirely.
    define: {
      'process.env.NODE_ENV': '"production"',
    },

    build: {
      outDir: path.resolve(pluginDir, outDir),
      emptyOutDir: true,

      lib: {
        entry: path.resolve(pluginDir, entry),
        formats: ['es'],
        fileName: 'index',
      },

      rollupOptions: {
        // No externals: all shared deps are resolved by nanikaSharedPlugin into
        // synthetic virtual modules that read from window.__nanika_shared__.
        // The result is a self-contained ES module loadable via import().
        output: {
          // Ensure a single output chunk without code-splitting so plugin
          // consumers can import() a single predictable path.
          inlineDynamicImports: true,
        },
      },
    },
  })
}
