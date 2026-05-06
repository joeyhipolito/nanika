import { useState, useRef, useCallback } from 'react'
import { Icon } from '../icons/Icon'
import { ComposerChips } from './ComposerChips'
import { ComposerFooter } from './ComposerFooter'
import { ProjectsTree } from './ProjectsTree'
import { LeftRail } from './LeftRail'
import { RightRail } from './RightRail'
import { TerminalDrawer } from './TerminalDrawer'
import { TopActionBar } from './TopActionBar'
import { DocumentTurn } from './DocumentTurn'
import { ToolBeat } from './ToolBeat'
import { CommitSummaryCard } from './CommitSummaryCard'
import { FileViewer } from './FileViewer'
import { MOCK_PROJECTS } from '../mocks/projects'
import { MOCK_RECENTS, MOCK_ROUTINES } from '../mocks/leftrail'
import { MOCK_FILE_VIEWER } from '../mocks/fileViewer'
import { CHANGED_FILES, DIFF_TOTAL_ADDITIONS, DIFF_TOTAL_DELETIONS } from '../mocks/diffs'
import type { ConvItem } from '../types'

export type RailVariant     = 'projects-tree' | 'left-rail'
export type RightRailMode   = 'files' | 'turn-diff'
export type ComposerStateId = 'idle' | 'drafting' | 'recording-voice' | 'recording-vhold' | 'error'
export type ConversationFixtureId =
  | 'baseline'
  | 'short'
  | 'long-scroll'
  | 'with-plugin'
  | 'empty'

export interface CompositeCanvasState {
  railVariant:         RailVariant
  terminalOpen:        boolean
  rightRailOpen:       boolean
  rightRailMode:       RightRailMode
  selectedFile:        { path: string; line?: number } | null
  conversationFixture: ConversationFixtureId
  composerState:       ComposerStateId
  errorMessage: {
    kind:        'phase-failed' | 'apply-failed' | 'permission-denied' | 'connectivity'
    body:        string
    retryLabel?: string
  } | null
  notifications: Array<{
    id:            string
    kind:          'info' | 'success' | 'warn' | 'error'
    surface?:      'hud' | 'banner' | 'badge'
    body:          string
    actionLabel?:  string
    autoDismissMs?: number
  }>
  missionRun: {
    missionId:     string
    title:         string
    phases:        Array<{ id: string; persona: string; status: 'pending' | 'running' | 'done' | 'failed'; expectedMs: number }>
    activePhaseId: string | null
    elapsedMs:     number
  } | null
  pluginInline: {
    prefix:         string
    afterTurnIndex: number
    body:           unknown[]
  } | null
  streamingTurn:    string | null
  workingForMs:     number | null
  reviewGate:       boolean
  voiceTranscript:  string | null
  /** When set, overrides `conversationFixture` with live data. */
  liveConversation?: ConvItem[]
}

// ─── Conversation fixtures ────────────────────────────────────────────────────

const COMPOSITE_CONV_BASELINE: ConvItem[] = [
  {
    kind: 'user',
    text: 'Walk me through the whim-web slice 2 scene layout and key decisions',
  },
  {
    kind: 'tool-beat',
    summary: 'Recalled 5 memories',
    body: 'memory: project rename from via → nanika\nmemory: alluka naming is intentional\nmemory: nen architecture (en/gyo/ryu observers)\nmemory: Width 280px for ProjectsTree, 240px for LeftRail\nmemory: Vite 5 + React 18 build floor ~46 KB gzipped',
  },
  {
    kind: 'tool-beat',
    summary: 'Read 4 files, ran 2 commands',
    body: 'read: src/scenarios/Scenes.tsx\nread: src/App.tsx\nread: src/mocks/conversation.ts\nread: src/components/ProjectsTree.tsx\nran: npm run build\nran: grep -E "import.*ProjectsTree" src/scenarios/Scenes.tsx',
  },
  {
    kind: 'doc-turn',
    content: `# Whim Canvas — Slice 2 Layout Reference

The canonical canvas shape at **1440×900** composes five structural layers from top to bottom, with two optional columns that open on keyboard shortcut.

## Section Overview

| Section | What it shows | Width / Height | Toggle |
|---------|--------------|----------------|--------|
| TopActionBar | Breadcrumb chips, + Add action, Open ▾, Commit & push ▾ | 100% × 56 px | always visible |
| ProjectsTree | T3-style project list with ⌘K search and j/k nav | 280 px wide | always visible |
| Main column | Conversation transcript in document-mode, max-width 760 px | flex fill | always visible |
| RightRail | FilesPanel or TurnDiffInspector, toggled via mode tab strip | 300 px wide | ⌘B |
| TerminalDrawer | Bottom-docked shell with live prompt and colorized output | 240 px tall | ⌘J |

## Keyboard Shortcuts

\`\`\`ts
useEffect(() => {
  const handler = (e: KeyboardEvent) => {
    if (e.metaKey && e.key === 'j') { e.preventDefault(); setTermOpen(v => !v) }
    if (e.metaKey && e.key === 'b') { e.preventDefault(); setRailOpen(v => !v) }
  }
  window.addEventListener('keydown', handler)
  return () => window.removeEventListener('keydown', handler)
}, [])
\`\`\`

Opening the terminal pushes the conversation up via flex layout. Opening the right rail shifts the conversation column — CSS grid/flex adds a third column at 300 px and narrows the center pane accordingly.`,
  },
  {
    kind: 'tool-beat',
    summary: 'Ran npm run build',
    body: '✓ built in 577ms · 0 errors · 0 warnings',
  },
  {
    kind: 'commit-summary',
    from:      'main',
    to:        'via/20260427-4cfbf67a/target-repo-nanika-status-active-mission',
    additions: 312,
    deletions: 18,
  },
]

const FIXTURES: Record<ConversationFixtureId, ConvItem[]> = {
  baseline:      COMPOSITE_CONV_BASELINE,
  short:         COMPOSITE_CONV_BASELINE,
  'long-scroll': COMPOSITE_CONV_BASELINE,
  'with-plugin': COMPOSITE_CONV_BASELINE,
  empty:         [],
}

// ─── Sub-components ───────────────────────────────────────────────────────────

export interface CompositeCanvasProps {
  state:               CompositeCanvasState
  onCloseTerminal?:    () => void
  onStop?:             () => void
  onSelectFile?:       (path: string) => void
  onComposerSubmit?:   (text: string) => void
}

function VoiceOverlay({ bins = 9 }: { bins?: number }) {
  return (
    <div
      aria-label="Voice recording in progress"
      style={{ display: 'flex', alignItems: 'center', gap: '4px', flex: 1, height: '24px', paddingLeft: '2px' }}
    >
      {Array.from({ length: bins }).map((_, i) => (
        <span
          key={i}
          aria-hidden="true"
          style={{
            display:         'block',
            width:           '3px',
            height:          '24px',
            borderRadius:    '2px',
            background:      'var(--accent)',
            animation:       'wave-bar 0.55s ease-in-out infinite',
            animationDelay:  `${i * 0.06}s`,
            transformOrigin: 'center',
          }}
        />
      ))}
    </div>
  )
}

function ReviewGateBlock() {
  const [collapsed, setCollapsed] = useState(false)
  return (
    <div data-tour-anchor="changed-files-card" style={{ background: 'var(--s1)', border: '0.5px solid var(--border)', borderRadius: '8px', overflow: 'hidden' }}>
      <div style={{
        display:      'flex',
        alignItems:   'center',
        padding:      '10px 14px',
        borderBottom: collapsed ? 'none' : '0.5px solid var(--border-soft)',
        background:   'var(--s2)',
      }}>
        <span style={{
          fontFamily:    'var(--mono)',
          fontSize:      '11px',
          color:         'var(--muted)',
          letterSpacing: '0.12em',
          textTransform: 'uppercase',
          flex:          1,
        }}>
          Changed Files ({CHANGED_FILES.length})
          <span style={{ color: 'var(--ghost)', margin: '0 8px' }}>•</span>
          <span style={{ color: 'var(--green)' }}>+{DIFF_TOTAL_ADDITIONS}</span>
          <span style={{ color: 'var(--ghost)', margin: '0 4px' }}>/</span>
          <span style={{ color: 'var(--red)' }}>-{DIFF_TOTAL_DELETIONS}</span>
        </span>
        <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
          <button
            type="button"
            onClick={() => setCollapsed(c => !c)}
            style={{
              fontFamily:    'var(--mono)',
              fontSize:      '11px',
              color:         'var(--faint)',
              background:    'transparent',
              border:        'none',
              cursor:        'pointer',
              padding:       '4px 8px',
              borderRadius:  '5px',
              letterSpacing: '0.04em',
            }}
          >
            {collapsed ? 'Expand' : 'Collapse'}
          </button>
          <a
            href="#/s/3.9b-diff-viewer"
            style={{
              fontFamily:     'var(--mono)',
              fontSize:       '11px',
              color:          'var(--accent)',
              textDecoration: 'none',
              padding:        '4px 10px',
              borderRadius:   '5px',
              background:     'var(--accent-soft)',
              border:         '0.5px solid var(--accent-rim)',
              letterSpacing:  '0.06em',
            }}
          >
            View diff
          </a>
        </div>
      </div>
      {!collapsed && (
        <div style={{ padding: '10px 14px', fontFamily: 'var(--mono)', fontSize: '12px' }}>
          {CHANGED_FILES.slice(0, 6).map(f => {
            const filename = f.path.split('/').pop() ?? f.path
            return (
              <div key={f.path} style={{ display: 'flex', alignItems: 'center', gap: '8px', padding: '3px 0' }}>
                <span style={{ flex: 1, color: 'var(--text)' }}>{filename}</span>
                <span style={{ color: 'var(--green)', fontVariantNumeric: 'tabular-nums', minWidth: '36px', textAlign: 'right' }}>
                  +{f.additions}
                </span>
                {f.deletions > 0 && (
                  <span style={{ color: 'var(--red)', fontVariantNumeric: 'tabular-nums', minWidth: '28px' }}>
                    -{f.deletions}
                  </span>
                )}
              </div>
            )
          })}
        </div>
      )}
    </div>
  )
}

type PluginBodyItem =
  | { kind: 'issue'; id: string; title: string; status: string; priority?: string }
  | { kind: 'draft'; title: string; preview: string; status: string }
  | { kind: 'text';  content: string }

function PluginInlineBlock({ plugin }: { plugin: NonNullable<CompositeCanvasState['pluginInline']> }) {
  const items = plugin.body as PluginBodyItem[]
  return (
    <div data-tour-anchor="plugin-inline-block" style={{ background: 'var(--s1)', border: '0.5px solid var(--border)', borderRadius: '8px', overflow: 'hidden' }}>
      <div style={{
        display:      'flex',
        alignItems:   'center',
        gap:          '8px',
        padding:      '7px 12px',
        borderBottom: '0.5px solid var(--border-soft)',
        background:   'var(--s2)',
      }}>
        <span style={{
          width:        '8px',
          height:       '8px',
          borderRadius: '3px',
          background:   plugin.prefix === 'tracker' ? 'var(--blue)' : 'var(--accent)',
          flexShrink:   0,
        }} />
        <span style={{
          fontFamily:    'var(--mono)',
          fontSize:      '10.5px',
          color:         'var(--muted)',
          letterSpacing: '0.1em',
          textTransform: 'uppercase',
          flex:          1,
        }}>
          {plugin.prefix}
        </span>
      </div>
      <div style={{ padding: '10px 12px', display: 'flex', flexDirection: 'column', gap: '6px' }}>
        {items.map((item, i) => {
          if (item.kind === 'issue') {
            return (
              <div key={i} style={{ display: 'flex', alignItems: 'center', gap: '10px' }}>
                <span style={{ fontFamily: 'var(--mono)', fontSize: '12px', color: 'var(--accent)', flexShrink: 0 }}>
                  {item.id}
                </span>
                <span style={{
                  fontSize:     '13px',
                  color:        'var(--text)',
                  flex:         1,
                  overflow:     'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace:   'nowrap',
                }}>
                  {item.title}
                </span>
                <span style={{
                  fontFamily:   'var(--mono)',
                  fontSize:     '10px',
                  color:        'var(--muted)',
                  padding:      '2px 7px',
                  borderRadius: '4px',
                  border:       '0.5px solid var(--border)',
                  background:   'var(--s2)',
                  flexShrink:   0,
                }}>
                  {item.status}
                </span>
                {item.priority && (
                  <span style={{
                    fontFamily:   'var(--mono)',
                    fontSize:     '10px',
                    flexShrink:   0,
                    color:        item.priority === 'P0' ? 'var(--red)' : item.priority === 'P1' ? '#DA7757' : 'var(--muted)',
                    padding:      '2px 7px',
                    borderRadius: '4px',
                    border:       '0.5px solid var(--border)',
                    background:   'var(--s2)',
                  }}>
                    {item.priority}
                  </span>
                )}
              </div>
            )
          }
          if (item.kind === 'draft') {
            return (
              <div key={i} style={{ padding: '8px 10px', background: 'var(--s2)', borderRadius: '6px', border: '0.5px solid var(--border)' }}>
                <div style={{
                  fontFamily:    'var(--mono)',
                  fontSize:      '11px',
                  color:         'var(--accent)',
                  marginBottom:  '4px',
                  display:       'flex',
                  justifyContent: 'space-between',
                }}>
                  <span>{item.title}</span>
                  <span style={{ color: 'var(--muted)' }}>{item.status}</span>
                </div>
                <p style={{ margin: 0, fontSize: '12px', color: 'var(--muted)', lineHeight: 1.5 }}>
                  {item.preview}
                </p>
              </div>
            )
          }
          if (item.kind === 'text') {
            return (
              <p key={i} style={{ margin: 0, fontSize: '12.5px', color: 'var(--muted)', lineHeight: 1.5 }}>
                {item.content}
              </p>
            )
          }
          return null
        })}
      </div>
    </div>
  )
}

// ─── CompositeCanvas ──────────────────────────────────────────────────────────

export function CompositeCanvas({ state, onCloseTerminal, onStop, onSelectFile, onComposerSubmit }: CompositeCanvasProps) {
  const conversation = state.liveConversation ?? FIXTURES[state.conversationFixture]

  const composerRef = useRef<HTMLTextAreaElement>(null)
  const handleComposerKeyDown = useCallback((e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey && onComposerSubmit) {
      e.preventDefault()
      const text = composerRef.current?.value.trim() ?? ''
      if (text) {
        onComposerSubmit(text)
        if (composerRef.current) composerRef.current.value = ''
      }
    }
  }, [onComposerSubmit])

  const hudNotes    = state.notifications.filter(n => n.surface === 'hud')
  const bannerNotes = state.notifications.filter(n => n.surface === 'banner')
  const badgeNotes  = state.notifications.filter(n => n.surface === 'badge')
  const toastNotes  = state.notifications.filter(n => !n.surface)

  const isError = state.composerState === 'error'
  const isVoice = state.composerState === 'recording-voice'

  const fileViewerData = state.selectedFile ? MOCK_FILE_VIEWER : null

  return (
    <div style={{
      width:         '100vw',
      height:        '100vh',
      display:       'flex',
      flexDirection: 'column',
      background:    'var(--s0)',
      overflow:      'hidden',
      fontFamily:    'var(--sans)',
      position:      'relative',
    }}>

      {/* ── Ambient HUD — non-blocking top-right overlay ── */}
      {hudNotes.length > 0 && (
        <div data-tour-anchor="ambient-hud" style={{
          position:      'absolute',
          top:           '64px',
          right:         '16px',
          zIndex:        20,
          display:       'flex',
          flexDirection: 'column',
          alignItems:    'flex-end',
          gap:           '5px',
          pointerEvents: 'none',
        }}>
          {hudNotes.map(n => (
            <div
              key={n.id}
              data-notification-surface="hud"
              style={{
                display:        'flex',
                alignItems:     'center',
                gap:            '8px',
                padding:        '4px 10px',
                borderRadius:   '20px',
                background:     'var(--glass)',
                backdropFilter: 'blur(12px)',
                border:         '0.5px solid var(--border)',
                boxShadow:      '0 4px 16px rgba(0,0,0,0.5)',
              }}
            >
              <span style={{
                width:        '5px',
                height:       '5px',
                borderRadius: '50%',
                background:   'var(--accent)',
                boxShadow:    '0 0 5px var(--accent)',
                animation:    'pulse 1.4s infinite',
                flexShrink:   0,
              }} />
              <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--accent)', letterSpacing: '0.08em' }}>
                {n.body}
              </span>
            </div>
          ))}
        </div>
      )}

      {/* TopActionBar — 56 px */}
      <div style={{
        flexShrink:    0,
        height:        '56px',
        display:       'flex',
        flexDirection: 'column',
        justifyContent: 'center',
        background:    'var(--s1)',
        borderBottom:  '0.5px solid var(--border)',
      }}>
        <TopActionBar
          breadcrumbs={[
            { label: 'Identify current context' },
            { label: 'nanika' },
          ]}
        />
      </div>

      {/* ── Notification banner — shown immediately below TopActionBar ── */}
      {bannerNotes.length > 0 && (
        <div style={{ flexShrink: 0 }}>
          {bannerNotes.map(n => (
            <div
              key={n.id}
              data-notification-surface="banner"
              style={{
                display:      'flex',
                alignItems:   'center',
                gap:          '10px',
                padding:      '8px 16px',
                background:   n.kind === 'error' ? 'rgba(239,68,68,0.15)' : 'rgba(218,119,87,0.18)',
                borderBottom: n.kind === 'error' ? '0.5px solid rgba(239,68,68,0.35)' : '0.5px solid rgba(218,119,87,0.35)',
              }}
            >
              <span style={{
                fontFamily:    'var(--mono)',
                fontSize:      '10px',
                color:         n.kind === 'error' ? 'var(--red)' : '#DA7757',
                letterSpacing: '0.12em',
                textTransform: 'uppercase',
                flexShrink:    0,
              }}>
                {n.kind === 'error' ? '⚠ Error' : '⚠ Notice'}
              </span>
              <span style={{
                fontFamily: 'var(--sans)',
                fontSize:   '12.5px',
                color:      n.kind === 'error' ? 'rgba(239,68,68,0.9)' : 'rgba(218,119,87,0.9)',
                flex:       1,
              }}>
                {n.body}
              </span>
            </div>
          ))}
        </div>
      )}

      {/* Body row: left rail + main + optional RightRail */}
      <div style={{ flex: 1, display: 'flex', overflow: 'hidden' }}>

        {/* Left rail — 280 px (ProjectsTree) or 240 px (LeftRail) */}
        <div style={{
          width:         state.railVariant === 'left-rail' ? '240px' : '280px',
          flexShrink:    0,
          borderRight:   '0.5px solid var(--border)',
          overflow:      'hidden',
          display:       'flex',
          flexDirection: 'column',
        }}>
          {state.railVariant === 'left-rail'
            ? <LeftRail recents={MOCK_RECENTS} routines={MOCK_ROUTINES} />
            : <ProjectsTree projects={MOCK_PROJECTS} />
          }
        </div>

        {/* Center column */}
        <div style={{
          flex:          1,
          display:       'flex',
          flexDirection: 'column',
          overflow:      'hidden',
          minWidth:      0,
        }}>

          {/* Conversation scroll area */}
          <div data-tour-anchor="canvas-chat-rail" style={{
            flex:           1,
            overflowY:      'auto',
            display:        'flex',
            justifyContent: 'center',
            padding:        '24px 24px 16px',
          }}>
            <div style={{
              width:         '100%',
              maxWidth:      '760px',
              display:       'flex',
              flexDirection: 'column',
              gap:           '4px',
            }}>

              {/* Mission run block — shown when missionRun is present */}
              {state.missionRun && (
                <div style={{
                  fontFamily:   'var(--mono)',
                  fontSize:     '12px',
                  lineHeight:   '1.9',
                  padding:      '12px 0 8px',
                  borderBottom: '0.5px dashed var(--border)',
                  marginBottom: '8px',
                }}>
                  <div style={{ color: 'var(--muted)', fontSize: '11px', letterSpacing: '0.12em', textTransform: 'uppercase', marginBottom: '6px' }}>
                    {state.missionRun.title}
                  </div>
                  {state.missionRun.phases.map(ph => {
                    const isDone    = ph.status === 'done'
                    const isRunning = ph.status === 'running'
                    const elapsedS  = isRunning ? Math.floor(state.missionRun!.elapsedMs / 1000) : null
                    return (
                      <div
                        key={ph.id}
                        data-tour-anchor={isRunning ? 'runlog-phase-active' : undefined}
                        style={{
                          display:             'grid',
                          gridTemplateColumns: '18px 180px 1fr auto',
                          gap:                 '10px',
                          alignItems:          'baseline',
                        }}
                      >
                        <span style={{ color: isDone ? 'var(--green)' : isRunning ? 'var(--accent)' : 'var(--ghost)' }}>
                          {isDone ? '✓' : '○'}
                        </span>
                        <span style={{ color: isDone || isRunning ? 'var(--text)' : 'var(--muted)' }}>{ph.persona}</span>
                        <span style={{ color: 'var(--muted)' }}>{ph.id}</span>
                        {elapsedS !== null && (
                          <span style={{ color: 'var(--accent)', fontVariantNumeric: 'tabular-nums' }}>· {elapsedS}s</span>
                        )}
                      </div>
                    )
                  })}
                </div>
              )}

              {/* Conversation turns with optional plugin-inline injection */}
              {conversation.flatMap((item, i) => {
                const elements: React.ReactNode[] = []

                if (item.kind === 'user') {
                  elements.push(<DocumentTurn key={`t-${i}`} role="user" content={item.text} />)
                } else if (item.kind === 'tool-beat') {
                  elements.push(<ToolBeat key={`t-${i}`} summary={item.summary} body={item.body} />)
                } else if (item.kind === 'doc-turn') {
                  elements.push(<DocumentTurn key={`t-${i}`} role="assistant" content={item.content} />)
                } else if (item.kind === 'commit-summary') {
                  elements.push(
                    <CommitSummaryCard
                      key={`t-${i}`}
                      from={item.from}
                      to={item.to}
                      additions={item.additions}
                      deletions={item.deletions}
                    />
                  )
                }

                if (state.pluginInline?.afterTurnIndex === i) {
                  elements.push(<PluginInlineBlock key={`pi-${i}`} plugin={state.pluginInline} />)
                }

                return elements
              })}

              {/* Review gate block — shown when reviewGate is true */}
              {state.reviewGate && <ReviewGateBlock />}

              {/* Streaming assistant turn — appended live */}
              {state.streamingTurn !== null && (
                <DocumentTurn role="assistant" content={state.streamingTurn || '▊'} />
              )}
            </div>
          </div>

          {/* FileViewer — rendered above the composer when a file is selected */}
          {fileViewerData && (
            <div style={{
              flexShrink:     0,
              maxHeight:      '260px',
              overflow:       'hidden',
              borderTop:      '0.5px solid var(--border)',
              display:        'flex',
              justifyContent: 'center',
              padding:        '8px 24px',
            }}>
              <FileViewer
                filename={fileViewerData.filename}
                language={fileViewerData.language}
                lines={fileViewerData.lines}
              />
            </div>
          )}

          {/* Composer area — ~72 px (chips + footer) */}
          <div style={{
            flexShrink: 0,
            borderTop:  '0.5px solid var(--border)',
            background: 'var(--s1)',
            padding:    '8px 24px 0',
          }}>
            <div style={{ maxWidth: '760px', margin: '0 auto' }}>

              {/* Error label — shown above the composer when composerState='error' */}
              {isError && state.errorMessage && (
                <div data-tour-anchor="error-banner" style={{
                  display:      'flex',
                  alignItems:   'center',
                  gap:          '8px',
                  padding:      '6px 10px',
                  marginBottom: '6px',
                  background:   'var(--red-soft)',
                  border:       '0.5px solid rgba(239,68,68,0.35)',
                  borderRadius: '7px',
                }}>
                  <span style={{ display: 'flex', color: 'var(--red)', flexShrink: 0 }}>
                    <Icon name="Alert" size={13} />
                  </span>
                  <span style={{
                    fontFamily: 'var(--mono)',
                    fontSize:   '11.5px',
                    color:      'var(--red)',
                    flex:       1,
                    lineHeight: 1.4,
                  }}>
                    {state.errorMessage.body}
                  </span>
                  {state.errorMessage.retryLabel && (
                    <button style={{
                      fontFamily:    'var(--mono)',
                      fontSize:      '11px',
                      color:         'var(--red)',
                      background:    'transparent',
                      border:        '0.5px solid rgba(239,68,68,0.4)',
                      borderRadius:  '5px',
                      padding:       '3px 8px',
                      cursor:        'pointer',
                      letterSpacing: '0.04em',
                    }}>
                      {state.errorMessage.retryLabel}
                    </button>
                  )}
                </div>
              )}

              {state.workingForMs !== null ? (
                /* Streaming composer — working-for counter + stop button */
                <div style={{
                  background:     'var(--s0)',
                  border:         '0.5px solid var(--border)',
                  borderRadius:   '10px',
                  overflow:       'hidden',
                  display:        'flex',
                  alignItems:     'center',
                  justifyContent: 'space-between',
                  padding:        '0 16px',
                  height:         '52px',
                }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                    <span style={{
                      width:        '6px',
                      height:       '6px',
                      borderRadius: '50%',
                      background:   'var(--accent)',
                      boxShadow:    '0 0 6px var(--accent)',
                      animation:    'pulse 1.6s ease-in-out infinite',
                      flexShrink:   0,
                    }} />
                    <span style={{
                      fontFamily:    'var(--mono)',
                      fontSize:      '12px',
                      color:         'var(--accent)',
                      letterSpacing: '0.12em',
                    }}>
                      Working for {Math.floor(state.workingForMs / 1000)}s
                    </span>
                  </div>
                  <button
                    onClick={onStop}
                    style={{
                      padding:       '5px 14px',
                      background:    '#c0392b',
                      border:        'none',
                      borderRadius:  '6px',
                      cursor:        'pointer',
                      fontFamily:    'var(--mono)',
                      fontSize:      '12px',
                      color:         '#fff',
                      letterSpacing: '0.06em',
                    }}
                  >
                    stop
                  </button>
                </div>
              ) : isVoice ? (
                /* Voice composer — 9-bin waveform + transcript preview */
                <div data-tour-anchor="composer-voice-overlay" style={{
                  background:   'var(--s0)',
                  border:       '0.5px solid var(--accent)',
                  borderRadius: '10px',
                  overflow:     'hidden',
                }}>
                  <div style={{
                    height:     '44px',
                    padding:    '10px 16px',
                    display:    'flex',
                    alignItems: 'center',
                    gap:        '12px',
                  }}>
                    <span style={{
                      width:        '8px',
                      height:       '8px',
                      borderRadius: '50%',
                      background:   'var(--accent)',
                      boxShadow:    '0 0 6px var(--accent)',
                      flexShrink:   0,
                      animation:    'pulse 1s ease-in-out infinite',
                    }} />
                    <VoiceOverlay bins={9} />
                    <span style={{
                      fontFamily:    'var(--mono)',
                      fontSize:      '11px',
                      color:         'var(--accent)',
                      letterSpacing: '0.18em',
                      textTransform: 'uppercase',
                      flexShrink:    0,
                    }}>
                      Listening
                    </span>
                  </div>
                  {state.voiceTranscript && (
                    <div style={{
                      padding:    '4px 16px 8px',
                      fontFamily: 'var(--mono)',
                      fontSize:   '12.5px',
                      color:      'var(--muted)',
                      borderTop:  '0.5px solid var(--border-soft)',
                    }}>
                      {state.voiceTranscript}
                    </div>
                  )}
                  <ComposerChips />
                  <ComposerFooter />
                </div>
              ) : (
                /* Normal composer — error state gets red border-color */
                <div data-tour-anchor="composer-input" style={{
                  background:   'var(--s0)',
                  border:       isError ? '0.5px solid var(--red)' : '0.5px solid var(--border)',
                  borderRadius: '10px',
                  overflow:     'hidden',
                }}>
                  {/* Composer input — live when onComposerSubmit is provided */}
                  <textarea
                    ref={composerRef}
                    onKeyDown={handleComposerKeyDown}
                    placeholder={onComposerSubmit ? 'Ask anything… (Enter to send)' : ''}
                    aria-label="Composer input"
                    rows={1}
                    style={{
                      display:    'block',
                      width:      '100%',
                      minHeight:  '44px',
                      padding:    '12px 16px',
                      fontFamily: 'var(--mono)',
                      fontSize:   '13px',
                      color:      onComposerSubmit ? 'var(--fg)' : 'var(--faint)',
                      background: 'transparent',
                      border:     'none',
                      outline:    'none',
                      resize:     'none',
                      boxSizing:  'border-box',
                    }}
                  />
                  <div data-tour-anchor="composer-chips">
                    <ComposerChips />
                  </div>
                  <div data-tour-anchor="composer-keycaps">
                    <ComposerFooter />
                  </div>
                </div>
              )}
            </div>
          </div>

          {/* TerminalDrawer — 240 px when open */}
          {state.terminalOpen && (
            <div data-tour-anchor="terminal-drawer" style={{ flexShrink: 0 }}>
              <TerminalDrawer defaultOpen={true} onClose={onCloseTerminal} />
            </div>
          )}

          {/* Ambient footer — 28 px */}
          <div style={{
            height:         '28px',
            flexShrink:     0,
            display:        'flex',
            alignItems:     'center',
            justifyContent: 'space-between',
            padding:        '0 16px',
            background:     'var(--s1)',
            borderTop:      '0.5px solid var(--border)',
            fontFamily:     'var(--mono)',
            fontSize:       '11px',
            color:          'var(--faint)',
            letterSpacing:  '0.04em',
          }}>
            <span>Local checkout</span>
            <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
              {/* Badge notifications */}
              {badgeNotes.map(n => (
                <span
                  key={n.id}
                  data-notification-surface="badge"
                  style={{
                    padding:      '1px 7px',
                    borderRadius: '4px',
                    fontSize:     '10px',
                    background:   n.kind === 'error' ? 'var(--red-soft)'
                                : n.kind === 'warn'  ? 'rgba(218,119,87,0.12)'
                                : 'var(--s2)',
                    border:       n.kind === 'error' ? '0.5px solid rgba(239,68,68,0.3)'
                                : n.kind === 'warn'  ? '0.5px solid rgba(218,119,87,0.3)'
                                : '0.5px solid var(--border)',
                    color:        n.kind === 'error' ? 'var(--red)'
                                : n.kind === 'warn'  ? '#DA7757'
                                : 'var(--faint)',
                  }}
                >
                  {n.body}
                </span>
              ))}
              <span style={{ display: 'flex', alignItems: 'center', gap: '4px' }}>
                <Icon name="Branch" size={11} />
                <span>main</span>
              </span>
            </div>
          </div>
        </div>

        {/* RightRail — 300 px when open */}
        {state.rightRailOpen && (
          <div data-tour-anchor="right-rail-header" style={{
            width:      '300px',
            flexShrink: 0,
            borderLeft: '0.5px solid var(--border)',
            overflowY:  'auto',
            background: 'var(--s1)',
            padding:    '12px',
          }}>
            <RightRail
              initialMode={state.rightRailMode}
              onSelectFile={onSelectFile}
            />
          </div>
        )}
      </div>

      {/* Toast stack — top-right, up to 3 visible */}
      {toastNotes.length > 0 && (
        <div data-tour-anchor="toast-stack" style={{
          position:      'fixed',
          top:           '64px',
          right:         '16px',
          zIndex:        9000,
          display:       'flex',
          flexDirection: 'column',
          gap:           '6px',
          width:         '280px',
          pointerEvents: 'none',
        }}>
          {toastNotes.slice(0, 3).map(n => (
            <div key={n.id} style={{
              display:        'flex',
              alignItems:     'center',
              gap:            '8px',
              padding:        '8px 12px',
              borderRadius:   '8px',
              background:     'var(--s2)',
              border:         '0.5px solid var(--border)',
              boxShadow:      '0 4px 16px rgba(0,0,0,0.5)',
            }}>
              <span style={{
                width:        '6px',
                height:       '6px',
                borderRadius: '50%',
                background:   n.kind === 'error' ? 'var(--red)' : n.kind === 'success' ? 'var(--green)' : n.kind === 'warn' ? 'var(--accent)' : 'var(--blue)',
                flexShrink:   0,
              }} />
              <span style={{ fontFamily: 'var(--sans)', fontSize: '12px', color: 'var(--muted)', flex: 1, lineHeight: 1.4 }}>
                {n.body}
              </span>
            </div>
          ))}
          {toastNotes.length > 3 && (
            <div style={{
              fontFamily:    'var(--mono)',
              fontSize:      '10px',
              color:         'var(--faint)',
              letterSpacing: '0.06em',
              textAlign:     'right',
              paddingRight:  '4px',
            }}>
              +{toastNotes.length - 3} more
            </div>
          )}
        </div>
      )}

      {/* Toggle hints overlay */}
      <div style={{
        position:      'fixed',
        bottom:        '36px',
        right:         '16px',
        fontFamily:    'var(--mono)',
        fontSize:      '10px',
        color:         'var(--ghost)',
        letterSpacing: '0.06em',
        pointerEvents: 'none',
        lineHeight:    '1.8',
        textAlign:     'right',
      }}>
        <div>⌘J {state.terminalOpen ? 'close terminal' : 'open terminal'}</div>
        <div>⌘B {state.rightRailOpen ? 'close right rail' : 'open right rail'}</div>
      </div>
    </div>
  )
}
