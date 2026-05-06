import { useState } from 'react'
import { useDiffs } from '../hooks/useDiffs'
import type { ChangedFile, Hunk, HunkLine } from '../types'

// LiveDiffPanel — wires the M5 live diff surface to the Tauri diff commands.
// list_changed_files populates the file list; get_file_diff loads hunks on
// demand. Selecting a file loads its hunks; y/n keyboard shortcuts accept or
// reject the cursor hunk.
//
// Repo root is hard-coded for the demo; future work wires the project picker.

const DEMO_REPO_ROOT = '/Users/joeyhipolito/nanika'

// ─── Line renderer ────────────────────────────────────────────────────────────

function lineBg(type: HunkLine['type']): string {
  if (type === 'add') return 'rgba(34,197,94,0.08)'
  if (type === 'rem') return 'rgba(239,68,68,0.08)'
  return 'transparent'
}

function lineFg(type: HunkLine['type']): string {
  if (type === 'add') return 'var(--green, #22c55e)'
  if (type === 'rem') return 'var(--red, #ef4444)'
  return 'var(--text-secondary, #9ca3af)'
}

function lineGlyph(type: HunkLine['type']): string {
  if (type === 'add') return '+'
  if (type === 'rem') return '-'
  return ' '
}

// ─── Hunk row ─────────────────────────────────────────────────────────────────

type HunkState = 'pending' | 'accepted' | 'rejected'

function LiveHunkCard({
  hunk,
  state,
  isActive,
  onAccept,
  onReject,
}: {
  hunk:     Hunk
  state:    HunkState
  isActive: boolean
  onAccept: () => void
  onReject: () => void
}) {
  return (
    <div style={{
      border:        `0.5px solid ${isActive ? 'var(--accent, #DA7757)' : 'var(--border, rgba(255,255,255,0.08))'}`,
      borderRadius:  '6px',
      overflow:      'hidden',
      opacity:       state === 'rejected' ? 0.45 : 1,
      marginBottom:  '8px',
    }}>
      {/* Header */}
      <div style={{
        display:        'flex',
        alignItems:     'center',
        justifyContent: 'space-between',
        padding:        '5px 10px',
        background:     'var(--s2, rgba(255,255,255,0.04))',
        fontFamily:     'var(--mono, monospace)',
        fontSize:       '11px',
      }}>
        <span style={{ color: 'var(--faint, #6b7280)', flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
          {hunk.header}
        </span>
        <div style={{ display: 'flex', gap: '6px', flexShrink: 0, marginLeft: '8px' }}>
          {state === 'accepted' ? (
            <span style={{
              fontSize: '10px', fontFamily: 'var(--mono, monospace)',
              color: 'var(--accent, #DA7757)', letterSpacing: '0.06em',
              padding: '2px 7px', borderRadius: '3px',
              background: 'rgba(218,119,87,0.14)',
              border: '0.5px solid rgba(218,119,87,0.4)',
            }}>
              Accepted
            </span>
          ) : (
            <>
              <button
                type="button"
                onClick={onAccept}
                disabled={state !== 'pending'}
                style={{
                  fontFamily: 'var(--mono, monospace)', fontSize: '10px',
                  color: 'var(--accent, #DA7757)', background: 'transparent',
                  border: '0.5px solid var(--accent, #DA7757)',
                  borderRadius: '3px', padding: '2px 7px', cursor: 'pointer',
                  letterSpacing: '0.06em',
                }}
              >
                Accept
              </button>
              <button
                type="button"
                onClick={onReject}
                disabled={state !== 'pending'}
                style={{
                  fontFamily: 'var(--mono, monospace)', fontSize: '10px',
                  color: 'var(--faint, #6b7280)', background: 'transparent',
                  border: '0.5px solid var(--border, rgba(255,255,255,0.12))',
                  borderRadius: '3px', padding: '2px 7px', cursor: 'pointer',
                  letterSpacing: '0.06em',
                }}
              >
                Reject
              </button>
            </>
          )}
        </div>
      </div>
      {/* Lines */}
      <div style={{ fontFamily: 'var(--mono, monospace)', fontSize: '11px', lineHeight: 1.5 }}>
        {hunk.lines.map((line, i) => (
          <div key={i} style={{
            display:    'flex',
            gap:        '6px',
            padding:    '0 10px',
            background: lineBg(line.type),
            color:      lineFg(line.type),
          }}>
            <span style={{ userSelect: 'none', opacity: 0.5, width: '10px', flexShrink: 0 }}>
              {lineGlyph(line.type)}
            </span>
            <span style={{ whiteSpace: 'pre', overflowX: 'auto' }}>{line.content}</span>
          </div>
        ))}
      </div>
    </div>
  )
}

// ─── File tab strip ───────────────────────────────────────────────────────────

function FileTab({
  file,
  active,
  onClick,
}: {
  file:    ChangedFile
  active:  boolean
  onClick: () => void
}) {
  const name = file.path.split('/').pop() ?? file.path
  return (
    <button
      type="button"
      onClick={onClick}
      style={{
        fontFamily:    'var(--mono, monospace)',
        fontSize:      '12px',
        color:         active ? 'var(--text, #f3f4f6)' : 'var(--muted, #9ca3af)',
        background:    active ? 'var(--s2, rgba(255,255,255,0.06))' : 'transparent',
        border:        'none',
        borderBottom:  active ? '1.5px solid var(--accent, #DA7757)' : '1.5px solid transparent',
        padding:       '8px 12px',
        cursor:        'pointer',
        whiteSpace:    'nowrap',
        letterSpacing: '0.01em',
        display:       'flex',
        alignItems:    'center',
        gap:           '6px',
      }}
    >
      <span>{name}</span>
      <span style={{ fontSize: '10px', color: 'var(--green, #22c55e)' }}>+{file.additions}</span>
      {file.deletions > 0 && (
        <span style={{ fontSize: '10px', color: 'var(--red, #ef4444)' }}>-{file.deletions}</span>
      )}
    </button>
  )
}

// ─── LiveDiffPanel ────────────────────────────────────────────────────────────

export interface LiveDiffPanelProps {
  repoRoot?: string
}

export function LiveDiffPanel({ repoRoot = DEMO_REPO_ROOT }: LiveDiffPanelProps) {
  const { files, loading, error, loadDiff, acceptHunk: acceptHunkIpc, rejectHunk } = useDiffs(repoRoot)

  const [selectedPath, setSelectedPath]       = useState<string | null>(null)
  const [hunkStates, setHunkStates]           = useState<Record<string, HunkState>>({})
  const [cursorHunkIndex, setCursorHunkIndex] = useState(0)

  const selectedFile = files.find(f => f.path === selectedPath) ?? files[0] ?? null

  function selectFile(path: string) {
    setSelectedPath(path)
    setCursorHunkIndex(0)
    // Load hunks on demand — list_changed_files returns empty hunks array.
    const file = files.find(f => f.path === path)
    if (file && file.hunks.length === 0) loadDiff(path)
  }

  function handleAccept(hunkId: string) {
    setHunkStates(prev => ({ ...prev, [hunkId]: 'accepted' }))
    acceptHunkIpc(hunkId)
  }

  function handleReject(hunkId: string) {
    setHunkStates(prev => ({ ...prev, [hunkId]: 'rejected' }))
    rejectHunk(hunkId)
  }

  if (loading && files.length === 0) {
    return (
      <div style={{ padding: '24px', fontFamily: 'var(--mono, monospace)', fontSize: '12px', color: 'var(--faint, #6b7280)' }}>
        Loading changed files…
      </div>
    )
  }

  if (error) {
    return (
      <div style={{ padding: '24px', fontFamily: 'var(--mono, monospace)', fontSize: '12px', color: 'var(--red, #ef4444)' }}>
        ⚠ {error}
      </div>
    )
  }

  if (files.length === 0) {
    return (
      <div style={{ padding: '24px', fontFamily: 'var(--mono, monospace)', fontSize: '12px', color: 'var(--faint, #6b7280)' }}>
        No changed files in {repoRoot}
      </div>
    )
  }

  const displayFile = selectedFile ?? files[0]
  const displayPath = displayFile?.path ?? ''

  return (
    <div style={{ display: 'flex', flexDirection: 'column', width: '820px', fontFamily: 'var(--sans, sans-serif)' }}>
      {/* File tab strip */}
      <div style={{
        display:      'flex',
        overflowX:    'auto',
        borderBottom: '0.5px solid var(--border, rgba(255,255,255,0.08))',
        background:   'var(--s1, rgba(255,255,255,0.02))',
      }}>
        {files.map(f => (
          <FileTab
            key={f.path}
            file={f}
            active={f.path === displayPath}
            onClick={() => selectFile(f.path)}
          />
        ))}
      </div>

      {/* Path + why */}
      <div style={{
        padding:      '10px 14px',
        borderBottom: '0.5px solid var(--border, rgba(255,255,255,0.06))',
        display:      'flex',
        alignItems:   'baseline',
        gap:          '12px',
      }}>
        <span style={{ fontFamily: 'var(--mono, monospace)', fontSize: '12px', color: 'var(--muted, #9ca3af)' }}>
          {displayPath}
        </span>
        {displayFile?.why && (
          <span style={{ fontSize: '12px', color: 'var(--faint, #6b7280)', lineHeight: 1.4 }}>
            — {displayFile.why}
          </span>
        )}
      </div>

      {/* Hunk list */}
      <div style={{ padding: '14px', display: 'flex', flexDirection: 'column' }}>
        {displayFile && displayFile.hunks.length === 0 && (
          <div style={{ fontFamily: 'var(--mono, monospace)', fontSize: '11px', color: 'var(--faint, #6b7280)' }}>
            Loading hunks… (click file tab to trigger)
          </div>
        )}
        {displayFile?.hunks.map((hunk, hi) => (
          <LiveHunkCard
            key={hunk.id}
            hunk={hunk}
            state={hunkStates[hunk.id] ?? 'pending'}
            isActive={hi === cursorHunkIndex}
            onAccept={() => handleAccept(hunk.id)}
            onReject={() => handleReject(hunk.id)}
          />
        ))}
      </div>
    </div>
  )
}
