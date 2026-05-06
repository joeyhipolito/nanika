import { useState } from 'react'
import {
  MOCK_TURNS,
  MOCK_HUNK_LINES,
  MOCK_COLLAPSED_CONTEXT,
  type DiffLine,
  type DiffLineType,
} from '../mocks/turnDiffs'
import type { ChangedFile, Hunk } from '../types'
import { Icon } from '../icons/Icon'

// ─── Types ────────────────────────────────────────────────────────────────────

type LayoutMode = 'unified' | 'split'
type ViewMode   = 'diff' | 'full'

interface TurnDiffInspectorProps {
  // Live props — when provided, the component uses real data.
  // When omitted, falls back to MOCK_TURNS / MOCK_HUNK_LINES.
  files?:           ChangedFile[]
  activePath?:      string | null
  onSetActivePath?: (path: string) => void
  hunks?:           Hunk[]
  acceptHunk?:      (hunkId: string) => void
  rejectHunk?:      (hunkId: string) => void
  onClose?:         () => void
}

// ─── TurnDiffInspector ───────────────────────────────────────────────────────

export function TurnDiffInspector({
  files,
  activePath,
  onSetActivePath,
  hunks,
  acceptHunk,
  rejectHunk,
}: TurnDiffInspectorProps) {
  const [activeTurnId, setActiveTurnId] = useState(2)
  const [viewMode, setViewMode]         = useState<ViewMode>('diff')
  const [layoutMode, setLayoutMode]     = useState<LayoutMode>('unified')
  const [collapsed, setCollapsed]       = useState(false)

  const isLive = files !== undefined

  // Derive display lines from live hunks or fall back to mock
  const liveLines: DiffLine[] = isLive && hunks
    ? hunks.flatMap(h => h.lines as DiffLine[])
    : []
  const displayLines = isLive ? liveLines : MOCK_HUNK_LINES

  // Derive the active file for the header
  const activeFile = isLive
    ? (files?.find(f => f.path === activePath) ?? files?.[0] ?? null)
    : null
  const displayPath = activeFile?.path ?? 'claude/agents/journaler.md'
  const displayAdditions = activeFile?.additions ?? 5
  const displayDeletions = activeFile?.deletions ?? 3

  // The first hunk header for the hunk marker row
  const hunkHeader = (isLive && hunks && hunks.length > 0)
    ? hunks[0].header
    : '@@ -1,9 +1,11 @@ ## journaler'

  return (
    <div style={{
      width:         '300px',
      background:    'var(--s1)',
      border:        '0.5px solid var(--border)',
      borderRadius:  '10px',
      display:       'flex',
      flexDirection: 'column',
      fontFamily:    'var(--sans)',
      overflow:      'hidden',
      flexShrink:    0,
    }}>
      {/* Header */}
      <div style={{
        display:      'flex',
        alignItems:   'center',
        gap:          '4px',
        padding:      '8px 10px',
        borderBottom: '0.5px solid var(--border)',
        flexShrink:   0,
        overflowX:    'auto',
      }}>
        {/* Prev arrow */}
        <button
          onClick={() => {
            if (isLive && files && onSetActivePath) {
              const idx = files.findIndex(f => f.path === activePath)
              if (idx > 0) onSetActivePath(files[idx - 1].path)
            } else {
              setActiveTurnId(id => Math.max(1, id - 1))
            }
          }}
          aria-label="Previous turn"
          style={navBtnStyle}
        >
          <Icon name="ArrowLeft" size={14} />
        </button>

        {/* All turns label */}
        <span style={{
          fontSize:    '11px',
          color:       'var(--faint)',
          fontFamily:  'var(--mono)',
          whiteSpace:  'nowrap',
          flexShrink:  0,
          marginRight: '4px',
        }}>
          {isLive ? 'All files' : 'All turns'}
        </span>

        {/* Turn / file chips */}
        <div style={{ display: 'flex', gap: '4px', flex: 1, overflowX: 'auto' }}>
          {isLive
            ? (files ?? []).map(f => {
                const isActive = f.path === activePath
                const name = f.path.split('/').pop() ?? f.path
                return (
                  <button
                    key={f.path}
                    onClick={() => onSetActivePath?.(f.path)}
                    style={{
                      background:   isActive ? 'var(--s3)' : 'transparent',
                      border:       `1px solid ${isActive ? 'var(--accent-rim)' : 'var(--border)'}`,
                      borderRadius: '5px',
                      padding:      '2px 7px',
                      cursor:       'pointer',
                      fontFamily:   'var(--mono)',
                      fontSize:     '10px',
                      color:        isActive ? 'var(--accent)' : 'var(--faint)',
                      whiteSpace:   'nowrap',
                      flexShrink:   0,
                      outline:      isActive ? '1px solid var(--accent-rim)' : 'none',
                      outlineOffset: '-1px',
                    }}
                  >
                    {name}
                  </button>
                )
              })
            : MOCK_TURNS.map(t => {
                const isActive = t.id === activeTurnId
                return (
                  <button
                    key={t.id}
                    onClick={() => setActiveTurnId(t.id)}
                    style={{
                      background:   isActive ? 'var(--s3)' : 'transparent',
                      border:       `1px solid ${isActive ? 'var(--accent-rim)' : 'var(--border)'}`,
                      borderRadius: '5px',
                      padding:      '2px 7px',
                      cursor:       'pointer',
                      fontFamily:   'var(--mono)',
                      fontSize:     '10px',
                      color:        isActive ? 'var(--accent)' : 'var(--faint)',
                      whiteSpace:   'nowrap',
                      flexShrink:   0,
                      outline:      isActive ? '1px solid var(--accent-rim)' : 'none',
                      outlineOffset: '-1px',
                    }}
                  >
                    {t.label} {t.time}
                  </button>
                )
              })
          }
        </div>

        {/* Next arrow */}
        <button
          onClick={() => {
            if (isLive && files && onSetActivePath) {
              const idx = files.findIndex(f => f.path === activePath)
              if (idx < files.length - 1) onSetActivePath(files[idx + 1].path)
            } else {
              setActiveTurnId(id => Math.min(MOCK_TURNS.length, id + 1))
            }
          }}
          aria-label="Next turn"
          style={navBtnStyle}
        >
          <Icon name="ArrowRight" size={14} />
        </button>

        {/* View toggle */}
        <button
          onClick={() => setViewMode(m => m === 'diff' ? 'full' : 'diff')}
          aria-label="Toggle view mode"
          title={viewMode === 'diff' ? 'Show full file' : 'Show diff only'}
          style={{
            ...navBtnStyle,
            color: viewMode === 'full' ? 'var(--accent)' : 'var(--faint)',
            marginLeft: '4px',
          }}
        >
          <Icon name="PanelRight" size={13} />
        </button>

        {/* Layout toggle */}
        <button
          onClick={() => setLayoutMode(m => m === 'unified' ? 'split' : 'unified')}
          aria-label="Toggle layout"
          title={layoutMode === 'unified' ? 'Split view' : 'Unified view'}
          style={{
            ...navBtnStyle,
            color: layoutMode === 'split' ? 'var(--accent)' : 'var(--faint)',
          }}
        >
          <Icon name="Split" size={13} />
        </button>
      </div>

      {/* Body */}
      <div style={{ flex: 1, overflowY: 'auto' }}>
        {/* File header */}
        <div style={{
          display:      'flex',
          alignItems:   'center',
          gap:          '8px',
          padding:      '8px 12px 6px',
          borderBottom: '0.5px solid var(--border-soft)',
        }}>
          <span style={{
            flex:       1,
            fontFamily: 'var(--mono)',
            fontSize:   '11.5px',
            color:      'var(--accent)',
            overflow:   'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap',
          }}>
            {displayPath}
          </span>

          {/* ±N chip */}
          <span style={{
            fontFamily:   'var(--mono)',
            fontSize:     '10px',
            background:   'var(--s3)',
            borderRadius: '4px',
            padding:      '1px 6px',
            display:      'flex',
            gap:          '4px',
            flexShrink:   0,
          }}>
            <span style={{ color: 'var(--red)' }}>−{displayDeletions}</span>
            <span style={{ color: 'var(--green)' }}>+{displayAdditions}</span>
          </span>
        </div>

        {/* Hunk header */}
        <div style={{
          padding:    '4px 12px',
          fontFamily: 'var(--mono)',
          fontSize:   '10.5px',
          color:      'var(--faint)',
          background: 'var(--s2)',
          borderBottom: '0.5px solid var(--border-soft)',
        }}>
          {hunkHeader}
        </div>

        {/* Diff lines */}
        {displayLines.map((line, i) => (
          <DiffLineRow key={i} line={line} />
        ))}

        {/* Per-hunk accept/reject buttons in live mode */}
        {isLive && hunks && hunks.length > 0 && (acceptHunk || rejectHunk) && (
          <div style={{ display: 'flex', gap: '8px', padding: '8px 12px', borderTop: '0.5px solid var(--border-soft)' }}>
            {acceptHunk && (
              <button
                type="button"
                onClick={() => hunks.forEach(h => acceptHunk(h.id))}
                style={{
                  fontFamily: 'var(--mono)', fontSize: '10px',
                  color: 'var(--accent)', background: 'transparent',
                  border: '0.5px solid var(--accent)',
                  borderRadius: '3px', padding: '2px 10px', cursor: 'pointer',
                }}
              >
                Accept all
              </button>
            )}
            {rejectHunk && (
              <button
                type="button"
                onClick={() => hunks.forEach(h => rejectHunk(h.id))}
                style={{
                  fontFamily: 'var(--mono)', fontSize: '10px',
                  color: 'var(--faint)', background: 'transparent',
                  border: '0.5px solid var(--border)',
                  borderRadius: '3px', padding: '2px 10px', cursor: 'pointer',
                }}
              >
                Reject all
              </button>
            )}
          </div>
        )}

        {/* Collapsed context — mock mode only */}
        {!isLive && (
          <>
            <button
              onClick={() => setCollapsed(c => !c)}
              style={{
                display:     'flex',
                alignItems:  'center',
                gap:         '8px',
                width:       '100%',
                background:  'var(--s2)',
                border:      'none',
                borderTop:   '0.5px solid var(--border-soft)',
                borderBottom: '0.5px solid var(--border-soft)',
                padding:     '5px 12px',
                cursor:      'pointer',
                fontFamily:  'var(--mono)',
                fontSize:    '11px',
                color:       'var(--faint)',
                textAlign:   'left',
              }}
            >
              <span style={{ color: 'var(--ghost)', display: 'flex' }}>
                <Icon name="ChevronRight" size={12} style={{ transform: collapsed ? 'rotate(0deg)' : 'rotate(90deg)', transition: 'transform 150ms ease' }} />
              </span>
              <span>8 unmodified line(s)</span>
            </button>

            {!collapsed && (
              <div style={{ padding: '4px 0', borderBottom: '0.5px solid var(--border-soft)' }}>
                {MOCK_COLLAPSED_CONTEXT.map((line, i) => (
                  <div key={i} style={{
                    fontFamily:  'var(--mono)',
                    fontSize:    '11.5px',
                    color:       'var(--muted)',
                    padding:     '1px 12px 1px 28px',
                    whiteSpace:  'pre',
                    lineHeight:  '1.6',
                  }}>
                    {line || ' '}
                  </div>
                ))}
              </div>
            )}
          </>
        )}
      </div>
    </div>
  )
}

// ─── DiffLineRow ──────────────────────────────────────────────────────────────

function DiffLineRow({ line }: { line: DiffLine }) {
  const bgMap: Record<DiffLineType, string> = {
    add: 'var(--green-soft)',
    rem: 'var(--red-soft)',
    ctx: 'transparent',
  }
  const colorMap: Record<DiffLineType, string> = {
    add: 'var(--text)',
    rem: 'var(--text)',
    ctx: 'var(--muted)',
  }
  const prefixMap: Record<DiffLineType, string> = {
    add: '+',
    rem: '−',
    ctx: ' ',
  }
  const prefixColorMap: Record<DiffLineType, string> = {
    add: 'var(--green)',
    rem: 'var(--red)',
    ctx: 'var(--ghost)',
  }

  return (
    <div style={{
      display:    'flex',
      background: bgMap[line.type],
      padding:    '1px 0',
    }}>
      <span style={{
        width:      '20px',
        flexShrink: 0,
        textAlign:  'center',
        fontFamily: 'var(--mono)',
        fontSize:   '11.5px',
        color:      prefixColorMap[line.type],
        userSelect: 'none',
      }}>
        {prefixMap[line.type]}
      </span>
      <span style={{
        fontFamily: 'var(--mono)',
        fontSize:   '11.5px',
        color:      colorMap[line.type],
        whiteSpace: 'pre',
        flex:       1,
        overflow:   'hidden',
        textOverflow: 'ellipsis',
        lineHeight: '1.6',
      }}>
        {line.content || ' '}
      </span>
    </div>
  )
}

// ─── Shared styles ────────────────────────────────────────────────────────────

const navBtnStyle: React.CSSProperties = {
  background:   'transparent',
  border:       'none',
  cursor:       'pointer',
  color:        'var(--faint)',
  fontFamily:   'var(--mono)',
  fontSize:     '14px',
  padding:      '2px 4px',
  display:      'flex',
  alignItems:   'center',
  borderRadius: '4px',
  flexShrink:   0,
}
