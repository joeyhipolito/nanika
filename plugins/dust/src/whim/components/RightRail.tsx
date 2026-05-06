import { useState } from 'react'
import { FilesPanel } from './FilesPanel'
import { TurnDiffInspector } from './TurnDiffInspector'
import { MOCK_FILE_TREE } from '../mocks/files'
import { Icon } from '../icons/Icon'
import type { IconName } from '../icons/registry'

// ─── Types ────────────────────────────────────────────────────────────────────

export type RightRailMode = 'files' | 'turn-diff'

interface RightRailProps {
  mode?:          RightRailMode
  initialMode?:   RightRailMode
  onModeChange?:  (mode: RightRailMode) => void
  onClose?:       () => void
  onSelectFile?:  (path: string) => void
}

// ─── RightRail ────────────────────────────────────────────────────────────────

export function RightRail({ mode: controlledMode, initialMode = 'files', onModeChange, onClose, onSelectFile }: RightRailProps) {
  const [internalMode, setInternalMode] = useState<RightRailMode>(initialMode)
  const mode = controlledMode ?? internalMode

  function setMode(next: RightRailMode) {
    if (controlledMode === undefined) setInternalMode(next)
    onModeChange?.(next)
  }

  return (
    <div style={{
      display:       'flex',
      flexDirection: 'column',
      gap:           '8px',
      alignItems:    'flex-start',
    }}>
      {/* Mode switcher tab strip */}
      <div style={{
        display:       'flex',
        background:    'var(--s2)',
        border:        '0.5px solid var(--border)',
        borderRadius:  '7px',
        padding:       '3px',
        gap:           '2px',
      }}>
        <ModeTab
          label="Files"
          icon="File"
          active={mode === 'files'}
          onClick={() => setMode('files')}
        />
        <ModeTab
          label="Diff"
          icon="GitDiff"
          active={mode === 'turn-diff'}
          onClick={() => setMode('turn-diff')}
        />
      </div>

      {/* Panel */}
      {mode === 'files' ? (
        <FilesPanel
          files={MOCK_FILE_TREE}
          onClose={onClose}
          onSelect={onSelectFile}
        />
      ) : (
        <TurnDiffInspector onClose={onClose} />
      )}
    </div>
  )
}

// ─── ModeTab ──────────────────────────────────────────────────────────────────

function ModeTab({
  label,
  icon,
  active,
  onClick,
}: {
  label: string
  icon: IconName
  active: boolean
  onClick: () => void
}) {
  return (
    <button
      onClick={onClick}
      aria-pressed={active}
      style={{
        display:      'flex',
        alignItems:   'center',
        gap:          '5px',
        background:   active ? 'var(--s3)' : 'transparent',
        border:       'none',
        borderRadius: '5px',
        padding:      '4px 12px',
        cursor:       'pointer',
        fontFamily:   'var(--mono)',
        fontSize:     '11px',
        color:        active ? 'var(--accent)' : 'var(--faint)',
        outline:      active ? '1px solid var(--accent-rim)' : 'none',
        outlineOffset: '-1px',
        transition:   'background 120ms, color 120ms',
      }}
    >
      <Icon name={icon} variant={active ? 'duotone' : 'line'} size={13} />
      {label}
    </button>
  )
}
