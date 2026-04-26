import { useEffect, useRef } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { EditorView, keymap, lineNumbers } from '@codemirror/view'
import { EditorState } from '@codemirror/state'
import { defaultKeymap, indentWithTab } from '@codemirror/commands'
import { bracketMatching } from '@codemirror/language'

type Props = {
  path: string
  basename: string
  line?: number
  onClose: () => void
}

async function resolveLanguage(ext: string) {
  switch (ext.toLowerCase()) {
    case 'js':
    case 'mjs':
    case 'cjs': {
      const { javascript } = await import('@codemirror/lang-javascript')
      return javascript()
    }
    case 'jsx': {
      const { javascript } = await import('@codemirror/lang-javascript')
      return javascript({ jsx: true })
    }
    case 'ts': {
      const { javascript } = await import('@codemirror/lang-javascript')
      return javascript({ typescript: true })
    }
    case 'tsx': {
      const { javascript } = await import('@codemirror/lang-javascript')
      return javascript({ jsx: true, typescript: true })
    }
    case 'rs': {
      const { rust } = await import('@codemirror/lang-rust')
      return rust()
    }
    case 'py': {
      const { python } = await import('@codemirror/lang-python')
      return python()
    }
    case 'md':
    case 'mdx': {
      const { markdown } = await import('@codemirror/lang-markdown')
      return markdown()
    }
    case 'css': {
      const { css } = await import('@codemirror/lang-css')
      return css()
    }
    case 'html':
    case 'htm': {
      const { html } = await import('@codemirror/lang-html')
      return html()
    }
    case 'json':
    case 'jsonc': {
      const { json } = await import('@codemirror/lang-json')
      return json()
    }
    default:
      return null
  }
}

export function QuickEditor({ path, basename, line, onClose }: Props) {
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const container = containerRef.current
    if (!container) return

    let view: EditorView | null = null
    let destroyed = false
    const t0 = performance.now()

    async function init() {
      const content = await invoke<string>('read_file', { path })
      if (destroyed) return

      const ext = path.split('.').pop() ?? ''
      const langExt = await resolveLanguage(ext)
      if (destroyed) return

      const saveKeymap = keymap.of([
        {
          key: 'Mod-s',
          run() {
            const doc = view?.state.doc.toString() ?? ''
            invoke('write_file', { path, content: doc })
              .then(() => onClose())
              .catch(console.error)
            return true
          },
        },
        {
          key: 'Escape',
          run() {
            onClose()
            return true
          },
        },
      ])

      const extensions = [
        lineNumbers(),
        bracketMatching(),
        keymap.of([...defaultKeymap, indentWithTab]),
        saveKeymap,
        EditorView.theme({
          '&': {
            height: '100%',
            fontSize: '12px',
            fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
            background: 'var(--bg)',
          },
          '.cm-scroller': { overflow: 'auto', height: '100%' },
          '.cm-content': {
            color: 'var(--text-primary)',
            caretColor: 'var(--color-accent)',
            padding: '8px 0',
          },
          '.cm-gutters': {
            background: 'var(--bg-elevated)',
            borderRight: '1px solid var(--border)',
            color: 'var(--text-secondary)',
          },
          '.cm-activeLineGutter': { background: 'var(--selected-bg)' },
          '.cm-activeLine': { background: 'var(--selected-bg)' },
          '.cm-matchingBracket': {
            outline: '1px solid var(--color-accent)',
            borderRadius: '2px',
            background: 'transparent',
          },
          '.cm-cursor': { borderLeftColor: 'var(--color-accent)' },
          '.cm-selectionBackground': { background: 'rgba(100,100,255,0.2) !important' },
          '&.cm-focused .cm-selectionBackground': {
            background: 'rgba(100,100,255,0.3) !important',
          },
        }),
        ...(langExt ? [langExt] : []),
      ]

      const state = EditorState.create({ doc: content, extensions })
      view = new EditorView({ state, parent: container ?? undefined })

      if (line && line > 1) {
        const lineObj = state.doc.line(Math.min(line, state.doc.lines))
        view.dispatch({ selection: { anchor: lineObj.from }, scrollIntoView: true })
      }

      view.focus()
      const latency = (performance.now() - t0).toFixed(1)
      // [bench] editor-open: wall-clock ms at focus; delta from trigger is captured by shell-bench.sh
      console.debug(`[bench] editor-focused wall=${Date.now()} delta=${latency}`)
      console.debug(`[QuickEditor] open latency: ${latency}ms`)
    }

    init().catch(console.error)

    return () => {
      destroyed = true
      view?.destroy()
    }
  }, [path])

  return (
    <div className="flex flex-1 flex-col overflow-hidden" style={{ height: '100%' }}>
      {/* Header bar */}
      <div
        className="flex shrink-0 items-center justify-between px-3 py-1.5"
        style={{
          background: 'var(--bg-elevated)',
          borderBottom: '1px solid var(--border)',
        }}
      >
        <span className="truncate text-[11px] font-mono" style={{ color: 'var(--text-primary)' }}>
          {basename}
        </span>
        <div className="flex items-center gap-2 text-[10px]" style={{ color: 'var(--text-secondary)' }}>
          <span><kbd style={{ fontFamily: 'inherit' }}>⌘S</kbd> save</span>
          <span><kbd style={{ fontFamily: 'inherit' }}>Esc</kbd> cancel</span>
        </div>
      </div>

      {/* CodeMirror mount point */}
      <div ref={containerRef} className="flex-1 overflow-hidden" style={{ display: 'flex', flexDirection: 'column' }} />
    </div>
  )
}
