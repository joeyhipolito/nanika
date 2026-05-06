"use client"
import { useMemo } from 'react'
import { useMissions } from '../hooks/useMissions'
import { useTerminalLog } from '../hooks/useTerminalLog'
import { TerminalDrawer } from './TerminalDrawer'
import type { TerminalLine } from '../mocks/terminal'

// Heuristic colorization — log lines are plain strings; tag them with kinds
// so TerminalDrawer's coloring still works.
function classify(raw: string): TerminalLine {
  const text = raw
  if (/^\s*\$ /.test(text))                                   return { kind: 'cmd',    text: text.replace(/^\s*\$ /, '') }
  if (/^(error|err|fail|panic|✗)/i.test(text) || /^!/.test(text)) return { kind: 'stderr', text }
  if (/^(▸|→|info|i:|\[info\])/i.test(text))                  return { kind: 'info',   text }
  return { kind: 'stdout', text }
}

export function LiveTerminalDrawer() {
  const { activeMissionId } = useMissions()
  const { lines } = useTerminalLog(activeMissionId)

  const terminalLines = useMemo(() => lines.map(classify), [lines])

  if (!activeMissionId) {
    return (
      <div
        role="status"
        style={{
          display:        'flex',
          alignItems:     'center',
          justifyContent: 'center',
          height:         '180px',
          background:     'var(--s0)',
          borderTop:      '0.5px solid var(--border)',
          fontFamily:     'var(--mono)',
          fontSize:       '12px',
          color:          'var(--ghost)',
          letterSpacing:  '0.06em',
        }}
      >
        no active mission
      </div>
    )
  }

  return <TerminalDrawer defaultOpen={true} lines={terminalLines} />
}
