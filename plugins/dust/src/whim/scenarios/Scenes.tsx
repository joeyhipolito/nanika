import { useState, useEffect, useRef, useReducer } from 'react'
import { Tour } from '../components/Tour'
import { TourScene as TourSceneRenderer } from '../components/tour/TourScene'
import TOUR_STEPS from '../tour/steps'
import type { TourScene as TourSceneSpec } from '../tour/types'
import { ActionPalette } from '../components/ActionPalette'
import { FanView, type FanThread } from '../components/FanView'
import { PillDragOverlay } from '../components/PillDragOverlay'
import { TransitionDemo } from '../components/TransitionDemo'
import { MOCK_ACTION_ITEM, MOCK_ACTION_PALETTE_ACTIONS } from '../mocks/actionPaletteItems'
import { Icon } from '../icons/Icon'
import { ComposerChips } from '../components/ComposerChips'
import { ComposerFooter } from '../components/ComposerFooter'
import { PaletteShell, type RailItem, type ResultSection } from '../components/PaletteShell'
import { ProjectsTree } from '../components/ProjectsTree'
import { LeftRail } from '../components/LeftRail'
import { RightRail } from '../components/RightRail'
import { FilesPanel } from '../components/FilesPanel'
import { TurnDiffInspector } from '../components/TurnDiffInspector'
import { FileViewer } from '../components/FileViewer'
import { MOCK_FILE_TREE } from '../mocks/files'
import { MOCK_FILE_VIEWER } from '../mocks/fileViewer'
import { TerminalDrawer } from '../components/TerminalDrawer'
import { TopActionBar } from '../components/TopActionBar'
import { DocumentTurn } from '../components/DocumentTurn'
import { ToolBeat } from '../components/ToolBeat'
import { CommitSummaryCard } from '../components/CommitSummaryCard'
import { CompositeCanvas, type CompositeCanvasState } from '../components/CompositeCanvas'
import { LiveCompositeCanvas } from '../components/LiveCompositeCanvas'
import { LiveFilesPanel } from '../components/LiveFilesPanel'
import { LiveDiffPanel } from '../components/LiveDiffPanel'
import { LiveTurnDiffInspector } from '../components/LiveTurnDiffInspector'
import { LiveTerminalDrawer } from '../components/LiveTerminalDrawer'
import { LiveMissionRunCanvas } from '../components/LiveMissionRunCanvas'
import { LiveLeftRail } from '../components/LiveLeftRail'
import { LiveTopActionBar } from '../components/LiveTopActionBar'
import { LiveCommitSummaryCard } from '../components/LiveCommitSummaryCard'
import { LiveNotificationsCanvas } from '../components/LiveNotificationsCanvas'
import {
  stateDefault,
  stateAllOpen,
  stateLeftrail,
  stateEmpty,
  stateStreaming,
  stateMissionRunning,
  stateReviewGate,
  stateError,
  stateVoice,
  stateFilesAndViewer,
  statePluginInline,
  stateNotifications,
} from '../mocks/canvasStates'
// MOCK_CONVERSATION inlined here after removal from mocks/conversation.ts (M3 Phase 1).
// Used only by DocumentModeScene — not imported from the now-empty mocks file.
const MOCK_CONVERSATION = [
  { kind: 'user' as const, text: 'what is this' },
  {
    kind: 'tool-beat' as const,
    summary: 'Recalled 3 memories',
    body: 'memory: project rename from via → nanika\nmemory: alluka naming is intentional\nmemory: nen architecture (en/gyo/ryu observers)',
  },
  {
    kind: 'tool-beat' as const,
    summary: 'Read a file, ran a command',
    body: 'read: CLAUDE.md\nran: orchestrator hooks preflight',
  },
  {
    kind: 'doc-turn' as const,
    content: '## Nanika\n\nNanika is a **self-improving multi-agent orchestrator** built on top of Claude Code.',
  },
  {
    kind: 'tool-beat' as const,
    summary: 'Ran 4 commands, read 2 files, created 2 files',
    body: 'ran: orchestrator dream run --since 24h\nran: npm run build\nran: grep -r "SceneShell" src/\nran: git status\nread: src/scenarios/Scenes.tsx\nread: src/App.tsx\ncreated: src/components/DocumentTurn.tsx\ncreated: src/mocks/conversation.ts',
  },
  {
    kind: 'commit-summary' as const,
    from: 'main',
    to: 'via/20260427-4cfbf67a/target-repo-nanika',
    additions: 147741,
    deletions: 70564,
  },
]
import { MOCK_PROJECTS } from '../mocks/projects'
import { MOCK_RECENTS, MOCK_ROUTINES } from '../mocks/leftrail'
import { MOCK_SECTIONS, MOCK_TRANSCRIPT_LINES, MOCK_TRANSCRIPT_COMMITTED } from '../mocks/index'
import { CHANGED_FILES, DIFF_TOTAL_ADDITIONS, DIFF_TOTAL_DELETIONS, type ChangedFile, type Hunk, type HunkLine } from '../mocks/diffs'
import {
  MISSION_PHASES,
  MISSION_TOTAL_MS,
  buildRunLog,
  formatElapsed,
  ACTIVE_MISSIONS,
  RECENT_MISSIONS,
  SCHEDULED_MISSIONS,
} from '../mocks/events'

// ─── Shared timer hook ─────────────────────────────────────────────────────────

function useNow(active: boolean, intervalMs = 1000): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!active) return
    const id = setInterval(() => setNow(Date.now()), intervalMs)
    return () => clearInterval(id)
  }, [active, intervalMs])
  return now
}

// ─── Shared scene layout ───────────────────────────────────────────────────────

function SceneShell({
  id,
  title,
  hint,
  children,
}: {
  id: string
  title: string
  hint?: string
  children: React.ReactNode
}) {
  return (
    <div style={{
      display:       'flex',
      flexDirection: 'column',
      alignItems:    'center',
      padding:       '48px 40px 80px',
      gap:           '28px',
      fontFamily:    'var(--sans)',
      minHeight:     '100vh',
    }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: '12px', width: '820px' }}>
        <a href="#/scenarios" style={{
          display:        'inline-flex',
          alignItems:     'center',
          gap:            '4px',
          fontFamily:     'var(--mono)',
          fontSize:       '11px',
          color:          'var(--faint)',
          textDecoration: 'none',
          letterSpacing:  '0.08em',
        }}>
          <Icon name="ChevronLeft" size={12} /> scenarios
        </a>
        <span style={{ color: 'var(--ghost)' }}>·</span>
        <span style={{
          fontFamily:    'var(--mono)',
          fontSize:      '11px',
          color:         'var(--accent)',
          letterSpacing: '0.08em',
          textTransform: 'uppercase',
        }}>
          {id}
        </span>
        <span style={{ color: 'var(--ghost)' }}>·</span>
        <span style={{
          fontFamily:    'var(--mono)',
          fontSize:      '11px',
          color:         'var(--muted)',
          letterSpacing: '0.04em',
        }}>
          {title}
        </span>
      </div>
      {children}
      {hint && (
        <p style={{
          fontFamily:    'var(--mono)',
          fontSize:      '11px',
          color:         'var(--faint)',
          margin:        0,
          letterSpacing: '0.06em',
        }}>
          {hint}
        </p>
      )}
    </div>
  )
}

// ─── 00 · What is Whim ────────────────────────────────────────────────────────

function MiniPill() {
  return (
    <div style={{
      width:        '130px',
      height:       '28px',
      borderRadius: '999px',
      background:   'var(--s2)',
      border:       '0.5px solid var(--border)',
      display:      'flex',
      alignItems:   'center',
      padding:      '0 12px',
      gap:          '8px',
      boxShadow:    '0 8px 24px rgba(0,0,0,0.4)',
    }}>
      <span style={{
        width:        '6px',
        height:       '6px',
        borderRadius: '50%',
        background:   'var(--accent)',
        boxShadow:    '0 0 6px var(--accent)',
        flexShrink:   0,
      }} />
      <span style={{ flex: 1, height: '2px', borderRadius: '1px', background: 'var(--ghost)' }} />
    </div>
  )
}

function MiniPalette() {
  return (
    <div style={{
      width:        '190px',
      borderRadius: '10px',
      background:   'var(--s1)',
      border:       '0.5px solid var(--border)',
      overflow:     'hidden',
      boxShadow:    '0 16px 40px rgba(0,0,0,0.45)',
    }}>
      <div style={{
        display:      'flex',
        alignItems:   'center',
        gap:          '8px',
        padding:      '10px 12px',
        borderBottom: '0.5px solid var(--border-soft)',
      }}>
        <span style={{ width: '10px', height: '10px', borderRadius: '50%', background: 'var(--ghost)', flexShrink: 0 }} />
        <div style={{ flex: 1, height: '2px', background: 'var(--ghost)', borderRadius: '1px' }} />
      </div>
      <div style={{ padding: '6px 0' }}>
        <div style={{ padding: '4px 12px 3px', height: '2px', background: 'var(--ghost)', borderRadius: '1px', width: '40px', marginBottom: '6px' }} />
        {[80, 60, 70].map((w, i) => (
          <div key={i} style={{ display: 'flex', alignItems: 'center', gap: '8px', padding: '5px 12px' }}>
            <span style={{
              width:        '10px',
              height:       '10px',
              borderRadius: '3px',
              background:   i === 0 ? 'var(--accent-soft)' : 'var(--s2)',
              border:       '0.5px solid var(--border)',
              flexShrink:   0,
            }} />
            <div style={{
              width:        `${w}%`,
              height:       '2px',
              background:   i === 0 ? 'rgba(212,120,86,0.5)' : 'var(--ghost)',
              borderRadius: '1px',
            }} />
          </div>
        ))}
      </div>
    </div>
  )
}

function MiniCanvas() {
  return (
    <div style={{
      width:        '220px',
      height:       '130px',
      borderRadius: '10px',
      background:   'var(--s1)',
      border:       '0.5px solid var(--border)',
      display:      'flex',
      overflow:     'hidden',
      boxShadow:    '0 16px 40px rgba(0,0,0,0.45)',
    }}>
      <div style={{
        width:         '44px',
        background:    'rgba(12,13,16,0.6)',
        borderRight:   '0.5px solid var(--border)',
        padding:       '8px 6px',
        display:       'flex',
        flexDirection: 'column',
        gap:           '5px',
      }}>
        {[true, false, false].map((active, i) => (
          <div key={i} style={{ display: 'flex', alignItems: 'center', gap: '4px' }}>
            <span style={{
              width:        '5px',
              height:       '5px',
              borderRadius: '50%',
              background:   active ? 'var(--green)' : 'var(--ghost)',
              flexShrink:   0,
            }} />
            <div style={{ flex: 1, height: '2px', background: 'var(--ghost)', borderRadius: '1px' }} />
          </div>
        ))}
      </div>
      <div style={{ flex: 1, padding: '8px 10px', display: 'flex', flexDirection: 'column', gap: '4px' }}>
        {[90, 70, 80, 60, 75].map((w, i) => (
          <div key={i} style={{
            width:        `${w}%`,
            height:       '2px',
            background:   i === 0 ? 'var(--muted)' : 'var(--ghost)',
            borderRadius: '1px',
          }} />
        ))}
        <div style={{ marginTop: 'auto', paddingTop: '6px', borderTop: '0.5px solid var(--border-soft)', display: 'flex', gap: '5px' }}>
          <div style={{ flex: 1, height: '10px', background: 'var(--s2)', borderRadius: '3px' }} />
          <div style={{ width: '14px', height: '10px', background: 'var(--blue)', borderRadius: '3px' }} />
        </div>
      </div>
    </div>
  )
}

function FlowArrow() {
  return (
    <div style={{ display: 'flex', alignItems: 'center', color: 'var(--ghost)' }}>
      <div style={{ width: '28px', height: '1px', background: 'var(--ghost)' }} />
      <div style={{
        borderLeft:   '6px solid var(--ghost)',
        borderTop:    '4px solid transparent',
        borderBottom: '4px solid transparent',
      }} />
    </div>
  )
}

export function WhatIsWhimScene() {
  return (
    <SceneShell id="00-what-is-whim" title="what is whim?">
      {/* Flow diagram */}
      <div style={{
        display:        'flex',
        alignItems:     'flex-start',
        gap:            '16px',
        flexWrap:       'wrap',
        justifyContent: 'center',
        paddingTop:     '16px',
      }}>
        {[
          { mini: <MiniPill />,    level: 'L0 · Pill',    desc: 'Edge-docked · always-on' },
          { mini: <MiniPalette />, level: 'L1 · Palette', desc: 'Summoned · results list' },
          { mini: <MiniCanvas />,  level: 'L3 · Canvas',  desc: 'Expanded · mission surface' },
        ].reduce<React.ReactNode[]>((acc, item, i, arr) => {
          acc.push(
            <div key={item.level} style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: '16px' }}>
              {item.mini}
              <div style={{ textAlign: 'center' }}>
                <div style={{ fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--accent)', letterSpacing: '0.12em', textTransform: 'uppercase' }}>
                  {item.level}
                </div>
                <div style={{ fontFamily: 'var(--sans)', fontSize: '12px', color: 'var(--muted)', marginTop: '4px' }}>
                  {item.desc}
                </div>
              </div>
            </div>
          )
          if (i < arr.length - 1) {
            acc.push(<div key={`arrow-${i}`} style={{ paddingTop: '14px' }}><FlowArrow /></div>)
          }
          return acc
        }, [])}
      </div>

      {/* Description card */}
      <div style={{
        width:         '640px',
        padding:       '24px 28px',
        borderRadius:  '12px',
        background:    'var(--s1)',
        border:        '0.5px solid var(--border)',
        display:       'flex',
        flexDirection: 'column',
        gap:           '14px',
      }}>
        <p style={{ margin: 0, fontSize: '15px', color: 'var(--text)', lineHeight: 1.6 }}>
          <strong style={{ fontWeight: 600 }}>Whim</strong> is a command surface that lives as a tiny pill at the edge of your screen.
          Hover to reveal search, type to summon the palette, and expand into a full mission canvas.
        </p>
        <div style={{ display: 'flex', flexDirection: 'column', gap: '10px' }}>
          {([
            ['⌥Space', 'Push-to-talk voice input'],
            ['Type',    'Search actions, threads, and projects'],
            ['↵',       'Execute the selected action'],
            ['Esc',     'Step back up the scale ladder'],
          ] as [string, string][]).map(([key, desc]) => (
            <div key={key} style={{ display: 'flex', alignItems: 'center', gap: '12px' }}>
              <span className="K acc" style={{ flexShrink: 0 }}>{key}</span>
              <span style={{ fontSize: '13px', color: 'var(--muted)' }}>{desc}</span>
            </div>
          ))}
        </div>
      </div>
    </SceneShell>
  )
}

// ─── 3.1 · Idle ───────────────────────────────────────────────────────────────

export function IdleScene() {
  const [posY, setPosY]         = useState(50)
  const containerRef            = useRef<HTMLDivElement>(null)
  const draggingRef             = useRef(false)
  const startRef                = useRef({ startY: 0, posY: 50 })

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!draggingRef.current || !containerRef.current) return
      const rect = containerRef.current.getBoundingClientRect()
      const dy   = e.clientY - startRef.current.startY
      setPosY(Math.max(5, Math.min(92, startRef.current.posY + (dy / rect.height) * 100)))
    }
    const onUp = () => {
      if (!draggingRef.current) return
      draggingRef.current = false
      document.body.style.cursor = ''
      setPosY(y => {
        if (Math.abs(y - 40) < 10) return 40
        if (Math.abs(y - 60) < 10) return 60
        return y
      })
    }
    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup',   onUp)
    return () => {
      window.removeEventListener('mousemove', onMove)
      window.removeEventListener('mouseup',   onUp)
    }
  }, [])

  const near40 = Math.abs(posY - 40) < 10
  const near60 = Math.abs(posY - 60) < 10
  const at40   = Math.abs(posY - 40) < 1
  const at60   = Math.abs(posY - 60) < 1

  return (
    <SceneShell
      id="3.1-idle"
      title="idle — edge-docked pill"
      hint="drag to reposition · snaps to 40% and 60% viewport lines"
    >
      <div
        ref={containerRef}
        style={{
          position:     'relative',
          width:        '820px',
          height:       '480px',
          background:   'var(--s0)',
          borderRadius: '12px',
          border:       '0.5px solid var(--border)',
          overflow:     'hidden',
          userSelect:   'none',
        }}
      >
        {/* Snap guide 40% */}
        <div style={{
          position:      'absolute',
          top:           '40%',
          left:          0,
          right:         0,
          height:        '1px',
          background:    at40 ? 'var(--accent)' : near40 ? 'var(--muted)' : 'var(--ghost)',
          opacity:       near40 ? 0.7 : 0.25,
          transition:    'background 0.15s, opacity 0.15s',
          pointerEvents: 'none',
          zIndex:        1,
        }}>
          <span style={{
            position:      'absolute',
            left:          '10px',
            top:           '-18px',
            fontFamily:    'var(--mono)',
            fontSize:      '9px',
            color:         at40 ? 'var(--accent)' : near40 ? 'var(--muted)' : 'var(--ghost)',
            letterSpacing: '0.1em',
            transition:    'color 0.15s',
          }}>40%</span>
        </div>
        {/* Snap guide 60% */}
        <div style={{
          position:      'absolute',
          top:           '60%',
          left:          0,
          right:         0,
          height:        '1px',
          background:    at60 ? 'var(--accent)' : near60 ? 'var(--muted)' : 'var(--ghost)',
          opacity:       near60 ? 0.7 : 0.25,
          transition:    'background 0.15s, opacity 0.15s',
          pointerEvents: 'none',
          zIndex:        1,
        }}>
          <span style={{
            position:      'absolute',
            left:          '10px',
            top:           '-18px',
            fontFamily:    'var(--mono)',
            fontSize:      '9px',
            color:         at60 ? 'var(--accent)' : near60 ? 'var(--muted)' : 'var(--ghost)',
            letterSpacing: '0.1em',
            transition:    'color 0.15s',
          }}>60%</span>
        </div>
        {/* Draggable pill */}
        <div
          style={{
            position:  'absolute',
            top:       `${posY}%`,
            left:      0,
            right:     0,
            transform: 'translateY(-50%)',
            cursor:    'grab',
            zIndex:    2,
          }}
          onMouseDown={e => {
            draggingRef.current = true
            startRef.current    = { startY: e.clientY, posY }
            document.body.style.cursor = 'grabbing'
            e.preventDefault()
          }}
        >
          <PaletteShell scale="pill" pillMode="idle" />
        </div>
      </div>
    </SceneShell>
  )
}

// ─── 3.2 · Hover ─────────────────────────────────────────────────────────────

export function HoverScene() {
  return (
    <SceneShell
      id="3.2-hover"
      title="hover — input row revealed"
      hint="read-only input · mic button · conversation count badge"
    >
      <PaletteShell scale="pill" pillMode="hover" convCount={3} placeholder="Search Whim…" />
    </SceneShell>
  )
}

// ─── 3.3 · Type ───────────────────────────────────────────────────────────────

export function TypeScene() {
  const [query, setQuery] = useState('')

  const filtered = query.trim()
    ? MOCK_SECTIONS
        .map(sec => ({
          ...sec,
          items: sec.items.filter(item =>
            typeof item.name === 'string'
              ? item.name.toLowerCase().includes(query.toLowerCase())
              : false
          ),
        }))
        .filter(sec => sec.items.length > 0)
    : MOCK_SECTIONS

  return (
    <SceneShell
      id="3.3-type"
      title="type — palette with grouped results"
      hint="Actions · Recent Threads · Projects · no mode pills"
    >
      <PaletteShell
        scale="palette"
        query={query}
        placeholder="Search actions, threads, projects…"
        sections={filtered}
        onQueryChange={setQuery}
        hints={[
          { keys: ['↑', '↓'], label: 'Navigate' },
          { keys: ['↵'],       label: 'Select' },
          { keys: ['Esc'],     label: 'Close' },
        ]}
      />
    </SceneShell>
  )
}

// ─── 3.4 · Voice listening ────────────────────────────────────────────────────

export function VoiceListeningScene() {
  const [lineIdx, setLineIdx] = useState(0)
  const [visible, setVisible] = useState(true)

  useEffect(() => {
    const timer = setInterval(() => {
      setVisible(false)
      setTimeout(() => {
        setLineIdx(i => (i + 1) % MOCK_TRANSCRIPT_LINES.length)
        setVisible(true)
      }, 350)
    }, 2600)
    return () => clearInterval(timer)
  }, [])

  return (
    <SceneShell
      id="3.4-voice-listening"
      title="voice listening — 5-bar waveform"
      hint="5 animated bars · scrolling transcript line"
    >
      <PaletteShell scale="pill" pillMode="voice" recording />
      {/* Scrolling transcript ticker */}
      <div style={{
        display:        'flex',
        alignItems:     'center',
        gap:            '12px',
        padding:        '12px 20px',
        borderRadius:   '8px',
        background:     'var(--s1)',
        border:         '0.5px solid var(--border)',
        minWidth:       '340px',
        justifyContent: 'center',
      }}>
        <span style={{
          width:        '6px',
          height:       '6px',
          borderRadius: '50%',
          background:   'var(--accent)',
          boxShadow:    '0 0 6px var(--accent)',
          flexShrink:   0,
          animation:    'pulse 1s ease-in-out infinite',
        }} />
        <span style={{
          fontFamily:    'var(--mono)',
          fontSize:      '13px',
          color:         'var(--muted)',
          letterSpacing: '0.04em',
          opacity:       visible ? 1 : 0,
          transition:    'opacity 0.3s ease',
        }}>
          {MOCK_TRANSCRIPT_LINES[lineIdx]}…
        </span>
      </div>
    </SceneShell>
  )
}

// ─── 3.5 · Voice recording ────────────────────────────────────────────────────

export function VoiceRecordingScene() {
  const [recording, setRecording] = useState(false)

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.altKey && e.code === 'Space') {
        e.preventDefault()
        setRecording(true)
      }
    }
    const onKeyUp = (e: KeyboardEvent) => {
      if (e.code === 'Space') {
        e.preventDefault()
        setRecording(false)
      }
    }
    window.addEventListener('keydown', onKeyDown)
    window.addEventListener('keyup',   onKeyUp)
    return () => {
      window.removeEventListener('keydown', onKeyDown)
      window.removeEventListener('keyup',   onKeyUp)
    }
  }, [])

  return (
    <SceneShell
      id="3.5-voice-recording"
      title="voice recording — ⌥Space push-to-talk"
      hint="hold ⌥Space to record · release to commit · 9-bin waveform in input row"
    >
      <PaletteShell
        scale="palette"
        recording={recording}
        sections={MOCK_SECTIONS}
        placeholder="Search Whim…"
        hints={[
          { keys: ['⌥', 'Space'], label: 'Push-to-talk' },
          { keys: ['Esc'],        label: 'Close' },
        ]}
      />
      <div style={{ display: 'flex', alignItems: 'center', gap: '10px' }}>
        <span className={`K ${recording ? 'acc' : 'nav'}`}>⌥Space</span>
        <span style={{
          fontFamily:    'var(--mono)',
          fontSize:      '11px',
          color:         recording ? 'var(--accent)' : 'var(--faint)',
          letterSpacing: '0.12em',
          textTransform: 'uppercase',
          transition:    'color 0.15s',
        }}>
          {recording ? 'Recording…' : 'Hold to record'}
        </span>
      </div>
    </SceneShell>
  )
}

// ─── 3.6 · Transcript preview ─────────────────────────────────────────────────

export function TranscriptPreviewScene() {
  const [query, setQuery]       = useState(MOCK_TRANSCRIPT_COMMITTED)
  const [submitted, setSubmitted] = useState(false)
  const [lastRun, setLastRun]   = useState('')

  const handleSubmit = () => {
    if (submitted || !query.trim()) return
    setLastRun(query)
    setSubmitted(true)
    setTimeout(() => {
      setQuery(MOCK_TRANSCRIPT_COMMITTED)
      setSubmitted(false)
    }, 2200)
  }

  return (
    <SceneShell
      id="3.6-transcript-preview"
      title="transcript preview — edit before Enter"
      hint="transcript pre-filled · edit freely · ↵ to run · not auto-submitted"
    >
      {submitted ? (
        <div style={{
          padding:      '20px 28px',
          borderRadius: '10px',
          background:   'var(--green-soft)',
          border:       '0.5px solid rgba(34,197,94,0.35)',
          fontFamily:   'var(--mono)',
          fontSize:     '13px',
          color:        'var(--green)',
          letterSpacing: '0.06em',
          maxWidth:     '820px',
          width:        '100%',
        }}>
          ✓ Running: {lastRun}
        </div>
      ) : (
        <PaletteShell
          scale="palette"
          query={query}
          onQueryChange={setQuery}
          onSubmit={handleSubmit}
          sections={MOCK_SECTIONS}
          placeholder="Edit transcript…"
          hints={[
            { keys: ['↵'],   label: 'Run' },
            { keys: ['Esc'], label: 'Discard' },
          ]}
        />
      )}
      {!submitted && (
        <div style={{
          display:    'flex',
          alignItems: 'center',
          gap:        '10px',
          padding:    '10px 16px',
          borderRadius: '8px',
          background: 'var(--s1)',
          border:     '0.5px solid var(--border)',
        }}>
          <span style={{
            width:        '6px',
            height:       '6px',
            borderRadius: '50%',
            background:   'var(--accent)',
            boxShadow:    '0 0 5px var(--accent)',
            flexShrink:   0,
          }} />
          <span style={{
            fontFamily:    'var(--mono)',
            fontSize:      '11px',
            color:         'var(--accent)',
            letterSpacing: '0.12em',
            textTransform: 'uppercase',
          }}>
            Transcript
          </span>
          <span style={{
            fontFamily:    'var(--mono)',
            fontSize:      '11px',
            color:         'var(--faint)',
            letterSpacing: '0.06em',
          }}>
            — edit then press ↵ to run
          </span>
        </div>
      )}
    </SceneShell>
  )
}

// ─── Mission canvas (shared by 3.7, 3.8, 3.13) ────────────────────────────────

function MissionCanvas({ query, startedAt }: { query: string; startedAt: number }) {
  const now       = useNow(true)
  const elapsedMs = Math.max(0, now - startedAt)
  const elapsedS  = Math.floor(elapsedMs / 1000)
  const log       = buildRunLog(elapsedMs)
  const working   = elapsedMs < MISSION_TOTAL_MS

  const railItems: RailItem[] = [
    {
      id:      'this',
      label:   query.length > 22 ? query.slice(0, 22) + '…' : query,
      active:  true,
      working,
      meta:    formatElapsed(elapsedMs),
    },
    { id: 'tracker-parity', label: 'tracker-parity' },
    { id: 'voice-overlay',  label: 'voice-overlay' },
  ]

  return (
    <PaletteShell
      scale="canvas"
      breadcrumb={`mission · ${query.length > 28 ? query.slice(0, 28) + '…' : query}`}
      railItems={railItems}
      mainContent={
        <div style={{ display: 'flex', flexDirection: 'column', gap: '20px' }}>
          {/* "Working for Ns" dashed rule */}
          <div style={{
            display:        'flex',
            alignItems:     'center',
            gap:            '10px',
            paddingBottom:  '12px',
            borderBottom:   '1px dashed var(--border)',
          }}>
            <span aria-hidden="true" style={{
              width:        '6px',
              height:       '6px',
              borderRadius: '50%',
              background:   working ? 'var(--accent)' : 'var(--green)',
              boxShadow:    `0 0 6px ${working ? 'var(--accent)' : 'var(--green)'}`,
              animation:    'pulse 1.6s ease-in-out infinite',
              flexShrink:   0,
            }} />
            <span style={{
              fontFamily:    'var(--mono)',
              fontSize:      '11px',
              color:         working ? 'var(--accent)' : 'var(--green)',
              letterSpacing: '0.18em',
              textTransform: 'uppercase',
            }}>
              {working ? `Working for ${elapsedS}s` : `Done · ${elapsedS}s`}
            </span>
          </div>

          {/* Optimistic user bubble */}
          <div style={{ display: 'flex', justifyContent: 'flex-end' }}>
            <div style={{
              maxWidth:     '80%',
              padding:      '10px 14px',
              borderRadius: '12px 12px 4px 12px',
              background:   'var(--accent-soft)',
              border:       '0.5px solid var(--accent-rim)',
              fontSize:     '14px',
              color:        'var(--text)',
              lineHeight:   1.5,
            }}>
              {query}
            </div>
          </div>

          {/* Stacked run log */}
          <div style={{ fontFamily: 'var(--mono)', fontSize: '13px', lineHeight: 2 }}>
            {log.map(row => (
              <div key={row.id} style={{
                display:             'grid',
                gridTemplateColumns: '18px 200px 1fr auto',
                gap:                 '12px',
                alignItems:          'baseline',
              }}>
                <span style={{ color: row.done ? 'var(--green)' : row.live ? 'var(--accent)' : 'var(--ghost)' }}>
                  {row.done ? '✓' : '○'}
                </span>
                <span style={{ color: row.done || row.live ? 'var(--text)' : 'var(--muted)' }}>
                  {row.persona}
                </span>
                <span style={{ color: 'var(--muted)' }}>{row.phase}</span>
                {row.durationLabel && (
                  <span style={{
                    color:              row.live ? 'var(--accent)' : 'var(--faint)',
                    fontVariantNumeric: 'tabular-nums',
                  }}>
                    · {row.durationLabel}
                  </span>
                )}
              </div>
            ))}
          </div>
        </div>
      }
    />
  )
}

// ─── 3.7 · Mission spawn ──────────────────────────────────────────────────────

export function MissionSpawnScene() {
  const [query, setQuery]         = useState('')
  const [startedAt, setStartedAt] = useState<number | null>(null)

  const onSubmit = () => {
    if (!query.trim() || startedAt !== null) return
    setStartedAt(Date.now())
  }

  if (startedAt !== null) {
    return (
      <SceneShell
        id="3.7-mission-spawn"
        title="mission spawn → progress"
        hint={`mission live · ${MISSION_PHASES.length} phases · ↻ refresh to spawn another`}
      >
        <MissionCanvas query={query} startedAt={startedAt} />
      </SceneShell>
    )
  }

  return (
    <SceneShell
      id="3.7-mission-spawn"
      title="mission spawn — type and press ↵"
      hint="type a mission · ↵ to spawn → switches to mission canvas"
    >
      <PaletteShell
        scale="palette"
        query={query}
        onQueryChange={setQuery}
        onSubmit={onSubmit}
        sections={MOCK_SECTIONS}
        placeholder="Describe the mission and press ↵…"
        hints={[
          { keys: ['↵'],   label: 'Spawn mission' },
          { keys: ['Esc'], label: 'Cancel' },
        ]}
      />
    </SceneShell>
  )
}

// ─── 3.8 · Mission progress ───────────────────────────────────────────────────

export function MissionProgressScene() {
  const [startedAt] = useState(() => Date.now())
  return (
    <SceneShell
      id="3.8-mission-progress"
      title="mission progress — live run log"
      hint={`○ → ✓ phase glyphs · live duration · 3 phases · ${Math.round(MISSION_TOTAL_MS / 1000)}s total`}
    >
      <MissionCanvas query={MOCK_TRANSCRIPT_COMMITTED} startedAt={startedAt} />
    </SceneShell>
  )
}

// ─── 3.13 · Multi-mission directory ───────────────────────────────────────────

function MissionRow({
  id,
  phase,
  duration,
  workers,
  live,
  subdued,
}: {
  id:        string
  phase:     string
  duration:  string
  workers:   number
  live?:     boolean
  subdued?:  boolean
}) {
  return (
    <div style={{
      display:    'flex',
      alignItems: 'center',
      gap:        '14px',
      minWidth:   0,
      flex:       1,
    }}>
      <span style={{
        fontFamily: 'var(--mono)',
        fontSize:   '13px',
        color:      subdued ? 'var(--muted)' : 'var(--text)',
        flexShrink: 0,
      }}>
        {id}
      </span>
      <span style={{ color: 'var(--ghost)', flexShrink: 0 }}>·</span>
      <span style={{
        fontFamily:   'var(--sans)',
        fontSize:     '13px',
        color:        'var(--muted)',
        flex:         1,
        overflow:     'hidden',
        textOverflow: 'ellipsis',
        whiteSpace:   'nowrap',
      }}>
        {phase}
      </span>
      <span style={{
        fontFamily:         'var(--mono)',
        fontSize:           '11.5px',
        color:              live ? 'var(--accent)' : 'var(--faint)',
        fontVariantNumeric: 'tabular-nums',
        flexShrink:         0,
      }}>
        {duration}
      </span>
      <span style={{
        fontFamily:   'var(--mono)',
        fontSize:     '10.5px',
        color:        'var(--faint)',
        padding:      '2px 7px',
        borderRadius: '4px',
        border:       '0.5px solid var(--border)',
        background:   'var(--s2)',
        flexShrink:   0,
      }}>
        {workers}w
      </span>
    </div>
  )
}

export function MultiMissionScene() {
  const [opened, setOpened] = useState<{ id: string; startedAt: number } | null>(null)
  const mountedAtRef         = useRef<number>(Date.now())
  const now                  = useNow(opened === null)
  const [query, setQuery]    = useState('')

  if (opened !== null) {
    return (
      <SceneShell
        id="3.13-multi-mission"
        title={`mission · ${opened.id}`}
        hint="mission canvas L3 — ← scenarios to return"
      >
        <MissionCanvas query={`run ${opened.id}`} startedAt={opened.startedAt} />
      </SceneShell>
    )
  }

  const ticked  = now - mountedAtRef.current
  const matches = (s: string) => !query.trim() || s.toLowerCase().includes(query.trim().toLowerCase())

  const activeFiltered    = ACTIVE_MISSIONS.filter(m => matches(m.id) || matches(m.phase))
  const recentFiltered    = RECENT_MISSIONS.filter(m => matches(m.id) || matches(m.phase))
  const scheduledFiltered = SCHEDULED_MISSIONS.filter(m => matches(m.id) || matches(m.phase))

  const sections: ResultSection[] = []
  if (activeFiltered.length > 0) {
    sections.push({
      label: `Active · ${activeFiltered.length}`,
      items: activeFiltered.map(m => ({
        id:   `active:${m.id}`,
        name: <MissionRow
          id={m.id}
          phase={m.phase}
          duration={formatElapsed(m.startedMsAgo + ticked)}
          workers={m.workers}
          live
        />,
      })),
    })
  }
  if (recentFiltered.length > 0) {
    sections.push({
      label: `Recent · ${recentFiltered.length}`,
      items: recentFiltered.map(m => ({
        id:   `recent:${m.id}`,
        name: <MissionRow
          id={m.id}
          phase={`${m.phase} · ${m.finishedAgo}`}
          duration={m.duration}
          workers={m.workers}
          subdued
        />,
      })),
    })
  }
  if (scheduledFiltered.length > 0) {
    sections.push({
      label: `Scheduled · ${scheduledFiltered.length}`,
      items: scheduledFiltered.map(m => ({
        id:   `scheduled:${m.id}`,
        name: <MissionRow
          id={m.id}
          phase={m.phase}
          duration={m.schedule}
          workers={m.workers}
          subdued
        />,
      })),
    })
  }

  const onSelect = (id: string) => {
    if (id.startsWith('active:')) {
      const real = id.slice('active:'.length)
      setOpened({ id: real, startedAt: Date.now() })
    }
  }

  return (
    <SceneShell
      id="3.13-multi-mission"
      title="multi-mission — palette directory"
      hint="Active · Recent · Scheduled — click an Active row to enter its mission canvas"
    >
      <PaletteShell
        scale="palette"
        query={query}
        onQueryChange={setQuery}
        sections={sections}
        onResultSelect={onSelect}
        placeholder="Search missions…"
        hints={[
          { keys: ['↑', '↓'], label: 'Navigate' },
          { keys: ['↵'],      label: 'Open' },
          { keys: ['Esc'],    label: 'Close' },
        ]}
      />
    </SceneShell>
  )
}

// ─── 3.9 · Review gate ────────────────────────────────────────────────────────

function FileTree({ files }: { files: ChangedFile[] }) {
  const byDir = new Map<string, ChangedFile[]>()
  files.forEach(f => {
    const parts = f.path.split('/')
    const dir   = parts.slice(0, -1).join('/')
    const arr   = byDir.get(dir) ?? []
    arr.push(f)
    byDir.set(dir, arr)
  })

  return (
    <div style={{ fontFamily: 'var(--mono)', fontSize: '12px' }}>
      <div style={{ color: 'var(--muted)', padding: '4px 0', letterSpacing: '0.06em' }}>src/</div>
      {[...byDir.entries()].map(([dir, dirFiles]) => {
        const shortDir = dir.replace(/^src\//, '')
        return (
          <div key={dir}>
            <div style={{ color: 'var(--faint)', padding: '3px 0 3px 14px', letterSpacing: '0.04em' }}>
              {shortDir}/
            </div>
            {dirFiles.map(f => {
              const filename = f.path.split('/').pop() ?? f.path
              return (
                <div key={f.path} style={{
                  display:    'flex',
                  alignItems: 'center',
                  padding:    '3px 0 3px 28px',
                  gap:        '8px',
                }}>
                  <span style={{ flex: 1, color: 'var(--text)' }}>{filename}</span>
                  <span style={{
                    color:              'var(--green)',
                    fontVariantNumeric: 'tabular-nums',
                    minWidth:           '36px',
                    textAlign:          'right',
                  }}>
                    +{f.additions}
                  </span>
                  {f.deletions > 0 && (
                    <span style={{
                      color:              'var(--red)',
                      fontVariantNumeric: 'tabular-nums',
                      minWidth:           '28px',
                    }}>
                      -{f.deletions}
                    </span>
                  )}
                  {f.deletions === 0 && (
                    <span style={{ minWidth: '28px' }} />
                  )}
                </div>
              )
            })}
          </div>
        )
      })}
    </div>
  )
}

export function ReviewGateScene() {
  const [collapsed, setCollapsed] = useState(false)

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey && e.key === 'd') {
        e.preventDefault()
        window.location.hash = '#/s/3.9b-diff-viewer'
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  const totalFiles = CHANGED_FILES.length
  const totalAdd   = DIFF_TOTAL_ADDITIONS
  const totalDel   = DIFF_TOTAL_DELETIONS

  return (
    <SceneShell
      id="3.9-review-gate"
      title="review gate — changed-files card"
      hint="⌘D or View diff to enter the diff viewer"
    >
      <div style={{
        width:         '820px',
        borderRadius:  '12px',
        background:    'var(--s1)',
        border:        '0.5px solid var(--border)',
        overflow:      'hidden',
      }}>
        {/* Card header */}
        <div style={{
          display:        'flex',
          alignItems:     'center',
          padding:        '12px 16px',
          borderBottom:   collapsed ? 'none' : '0.5px solid var(--border-soft)',
          background:     'var(--s2)',
        }}>
          <span style={{
            fontFamily:    'var(--mono)',
            fontSize:      '11px',
            color:         'var(--muted)',
            letterSpacing: '0.12em',
            textTransform: 'uppercase',
            flex:          1,
          }}>
            Changed Files ({totalFiles})
            <span style={{ color: 'var(--ghost)', margin: '0 8px' }}>•</span>
            <span style={{ color: 'var(--green)' }}>+{totalAdd}</span>
            <span style={{ color: 'var(--ghost)', margin: '0 4px' }}>/</span>
            <span style={{ color: 'var(--red)' }}>-{totalDel}</span>
          </span>
          <div style={{ display: 'flex', alignItems: 'center', gap: '10px' }}>
            <button
              type="button"
              onClick={() => setCollapsed(c => !c)}
              style={{
                fontFamily:  'var(--mono)',
                fontSize:    '11px',
                color:       'var(--faint)',
                background:  'transparent',
                border:      'none',
                cursor:      'pointer',
                padding:     '4px 8px',
                borderRadius: '5px',
                letterSpacing: '0.04em',
              }}
              onMouseEnter={e => (e.currentTarget.style.color = 'var(--muted)')}
              onMouseLeave={e => (e.currentTarget.style.color = 'var(--faint)')}
            >
              {collapsed ? 'Expand all' : 'Collapse all'}
            </button>
            <a
              href="#/s/3.9b-diff-viewer"
              style={{
                fontFamily:    'var(--mono)',
                fontSize:      '11px',
                color:         'var(--accent)',
                textDecoration: 'none',
                padding:       '4px 10px',
                borderRadius:  '5px',
                background:    'var(--accent-soft)',
                border:        '0.5px solid var(--accent-rim)',
                letterSpacing: '0.06em',
                display:       'flex',
                alignItems:    'center',
                gap:           '6px',
              }}
            >
              View diff
              <span style={{ fontFamily: 'var(--sans)', fontSize: '10px', color: 'var(--faint)' }}>
                <span className="K acc">⌘D</span>
              </span>
            </a>
          </div>
        </div>

        {/* File tree */}
        {!collapsed && (
          <div style={{ padding: '12px 16px', borderBottom: '0.5px solid var(--border-soft)' }}>
            <FileTree files={CHANGED_FILES} />
          </div>
        )}

        {/* Per-file summary table */}
        {!collapsed && (
          <div style={{ padding: '12px 16px' }}>
            <div style={{
              fontFamily:    'var(--mono)',
              fontSize:      '10px',
              color:         'var(--ghost)',
              letterSpacing: '0.12em',
              textTransform: 'uppercase',
              marginBottom:  '10px',
            }}>
              Summary
            </div>
            <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
              {CHANGED_FILES.map(f => {
                const filename = f.path.split('/').pop() ?? f.path
                return (
                  <div key={f.path} style={{ display: 'flex', alignItems: 'baseline', gap: '12px' }}>
                    <span style={{
                      fontFamily:  'var(--mono)',
                      fontSize:    '12px',
                      color:       'var(--text)',
                      flexShrink:  0,
                      width:       '180px',
                      overflow:    'hidden',
                      textOverflow:'ellipsis',
                      whiteSpace:  'nowrap',
                    }}>
                      {filename}
                    </span>
                    <span style={{
                      fontSize:    '12.5px',
                      color:       'var(--muted)',
                      lineHeight:  1.5,
                    }}>
                      {f.why}
                    </span>
                  </div>
                )
              })}
            </div>
          </div>
        )}
      </div>
    </SceneShell>
  )
}

// ─── 3.10 · Plugin embeds ─────────────────────────────────────────────────────

// Mock plugin component types
type PluginComponent =
  | { kind: 'Markdown'; content: string }
  | { kind: 'List';     items: string[] }
  | { kind: 'Divider' }

const MOCK_PLUGIN_COMPONENTS: PluginComponent[] = [
  { kind: 'Markdown', content: '**tracker plugin** v1.2.0 — issue tracking and project management via `tracker` CLI.' },
  { kind: 'Divider' },
  { kind: 'List', items: ['tracker list --status open', 'tracker update TRK-001 --status done', 'tracker create --title "Bug fix"'] },
  { kind: 'Markdown', content: 'Supports hierarchical relationships, blocking links, and priority-based ready detection.' },
]

function PluginMarkdown({ content }: { content: string }) {
  const parts = content.split(/(\*\*[^*]+\*\*|`[^`]+`)/g)
  return (
    <p style={{ margin: 0, fontSize: '13px', color: 'var(--muted)', lineHeight: 1.6 }}>
      {parts.map((p, i) => {
        if (p.startsWith('**') && p.endsWith('**')) {
          return <strong key={i} style={{ color: 'var(--text)', fontWeight: 600 }}>{p.slice(2, -2)}</strong>
        }
        if (p.startsWith('`') && p.endsWith('`')) {
          return <code key={i} style={{ fontFamily: 'var(--mono)', fontSize: '11.5px', color: 'var(--accent)', background: 'var(--accent-soft)', padding: '1px 5px', borderRadius: '3px' }}>{p.slice(1, -1)}</code>
        }
        return <span key={i}>{p}</span>
      })}
    </p>
  )
}

function PluginComponentRenderer({ components }: { components: PluginComponent[] }) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '10px' }}>
      {components.map((c, i) => {
        if (c.kind === 'Markdown') {
          return <PluginMarkdown key={i} content={c.content} />
        }
        if (c.kind === 'Divider') {
          return <div key={i} style={{ height: '1px', background: 'var(--border-soft)' }} />
        }
        if (c.kind === 'List') {
          return (
            <ul key={i} style={{ margin: 0, padding: '0 0 0 16px', display: 'flex', flexDirection: 'column', gap: '4px' }}>
              {c.items.map((item, j) => (
                <li key={j} style={{ fontFamily: 'var(--mono)', fontSize: '11.5px', color: 'var(--muted)' }}>{item}</li>
              ))}
            </ul>
          )
        }
        return null
      })}
    </div>
  )
}

// Sidebar widget countdown
function SidebarWidget() {
  const REFRESH_SECS = 30
  const [remaining, setRemaining] = useState(REFRESH_SECS)
  const [refreshing, setRefreshing] = useState(false)

  useEffect(() => {
    const id = setInterval(() => {
      setRemaining(r => {
        if (r <= 1) {
          setRefreshing(true)
          setTimeout(() => setRefreshing(false), 800)
          return REFRESH_SECS
        }
        return r - 1
      })
    }, 1000)
    return () => clearInterval(id)
  }, [])

  const pct = ((REFRESH_SECS - remaining) / REFRESH_SECS) * 100

  return (
    <div style={{
      background:    'var(--s1)',
      border:        '0.5px solid var(--border)',
      borderRadius:  '10px',
      overflow:      'hidden',
      width:         '100%',
    }}>
      <div style={{
        display:      'flex',
        alignItems:   'center',
        padding:      '8px 12px',
        borderBottom: '0.5px solid var(--border-soft)',
        background:   'var(--s2)',
        gap:          '8px',
      }}>
        <span style={{
          width:        '8px',
          height:       '8px',
          borderRadius: '3px',
          background:   'var(--blue)',
          flexShrink:   0,
        }} />
        <span style={{ fontFamily: 'var(--mono)', fontSize: '10.5px', color: 'var(--muted)', letterSpacing: '0.1em', textTransform: 'uppercase', flex: 1 }}>
          tracker · open issues
        </span>
        <span style={{
          fontFamily: 'var(--mono)',
          fontSize:   '10px',
          color:      refreshing ? 'var(--accent)' : 'var(--faint)',
          transition: 'color 0.2s',
        }}>
          {refreshing ? 'refreshing…' : `${remaining}s`}
        </span>
      </div>
      {/* Progress bar */}
      <div style={{ height: '2px', background: 'var(--border-soft)', position: 'relative' }}>
        <div style={{
          position:   'absolute',
          left:       0,
          top:        0,
          height:     '100%',
          width:      `${pct}%`,
          background: 'var(--blue)',
          transition: 'width 0.9s linear',
        }} />
      </div>
      <div style={{ padding: '10px 12px', display: 'flex', flexDirection: 'column', gap: '6px' }}>
        {[
          { id: 'TRK-573', title: 'CodeDiff: fix apply_hunk algorithm', priority: 'P0' },
          { id: 'TRK-558', title: 'Nanika → T3 Code parity',          priority: 'P1' },
          { id: 'TRK-575', title: 'Tool-use: switch --tools to --mcp', priority: 'P1' },
        ].map(issue => (
          <div key={issue.id} style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
            <span style={{
              fontFamily:   'var(--mono)',
              fontSize:     '9.5px',
              color:        issue.priority === 'P0' ? 'var(--red)' : 'var(--accent)',
              padding:      '1px 5px',
              borderRadius: '3px',
              background:   issue.priority === 'P0' ? 'var(--red-soft)' : 'var(--accent-soft)',
              border:       `0.5px solid ${issue.priority === 'P0' ? 'rgba(239,68,68,0.35)' : 'var(--accent-rim)'}`,
              flexShrink:   0,
            }}>
              {issue.priority}
            </span>
            <span style={{ fontFamily: 'var(--mono)', fontSize: '10.5px', color: 'var(--faint)', flexShrink: 0 }}>{issue.id}</span>
            <span style={{ fontSize: '12px', color: 'var(--muted)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{issue.title}</span>
          </div>
        ))}
      </div>
    </div>
  )
}

// Inline chat blocks
type ChatBlock =
  | { kind: 'AgentTurn';    content: string }
  | { kind: 'ToolCallBeat'; tool: string; args: string; result?: string }
  | { kind: 'CodeDiff';     filename: string; additions: number; deletions: number }
  | { kind: 'FileRef';      path: string; lines?: string }

function InlineChatBlock({ block }: { block: ChatBlock }) {
  if (block.kind === 'AgentTurn') {
    return (
      <div style={{
        padding:      '10px 14px',
        borderRadius: '8px',
        background:   'var(--s2)',
        border:       '0.5px solid var(--border)',
        fontSize:     '13px',
        color:        'var(--text)',
        lineHeight:   1.6,
      }}>
        {block.content}
      </div>
    )
  }
  if (block.kind === 'ToolCallBeat') {
    return (
      <div style={{
        borderRadius:  '8px',
        border:        '0.5px solid var(--border)',
        overflow:      'hidden',
      }}>
        <div style={{
          display:      'flex',
          alignItems:   'center',
          gap:          '8px',
          padding:      '6px 12px',
          background:   'var(--s2)',
          borderBottom: '0.5px solid var(--border-soft)',
        }}>
          <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--blue)', letterSpacing: '0.1em', textTransform: 'uppercase' }}>
            tool call
          </span>
          <span style={{ fontFamily: 'var(--mono)', fontSize: '11.5px', color: 'var(--text)' }}>{block.tool}</span>
          <span style={{ marginLeft: 'auto', fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--green)' }}>✓</span>
        </div>
        <div style={{ padding: '6px 12px', fontFamily: 'var(--mono)', fontSize: '11.5px', color: 'var(--faint)' }}>
          {block.args}
          {block.result && (
            <div style={{ marginTop: '4px', color: 'var(--muted)' }}>→ {block.result}</div>
          )}
        </div>
      </div>
    )
  }
  if (block.kind === 'CodeDiff') {
    return (
      <div style={{
        display:      'flex',
        alignItems:   'center',
        gap:          '10px',
        padding:      '7px 12px',
        borderRadius: '8px',
        background:   'var(--s2)',
        border:       '0.5px solid var(--border)',
      }}>
        <span style={{ fontFamily: 'var(--mono)', fontSize: '11.5px', color: 'var(--text)', flex: 1 }}>
          {block.filename}
        </span>
        <span style={{ fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--green)' }}>+{block.additions}</span>
        <span style={{ fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--red)' }}>-{block.deletions}</span>
      </div>
    )
  }
  if (block.kind === 'FileRef') {
    return (
      <div style={{
        display:      'flex',
        alignItems:   'center',
        gap:          '8px',
        padding:      '5px 10px',
        borderRadius: '6px',
        background:   'var(--blue-soft)',
        border:       '0.5px solid rgba(59,130,246,0.25)',
        width:        'fit-content',
      }}>
        <span style={{ fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--blue)' }}>📄 {block.path}</span>
        {block.lines && (
          <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--faint)' }}>:{block.lines}</span>
        )}
      </div>
    )
  }
  return null
}

const MOCK_CHAT_BLOCKS: ChatBlock[] = [
  { kind: 'AgentTurn',    content: 'Running the tracker query to find all open P0 issues assigned to the current sprint.' },
  { kind: 'ToolCallBeat', tool: 'tracker list', args: '--status open --priority P0', result: '3 issues' },
  { kind: 'CodeDiff',     filename: 'src/scenarios/Scenes.tsx', additions: 42, deletions: 7 },
  { kind: 'FileRef',      path: 'skills/orchestrator/main.go', lines: '112-145' },
]

function PluginPaletteRow({ name, score, prefix }: { name: string; score: number; prefix: string }) {
  return (
    <div style={{
      display:      'flex',
      alignItems:   'center',
      gap:          '10px',
      padding:      '7px 12px',
      background:   'var(--accent-soft)',
      border:       '0.5px solid var(--accent-rim)',
      borderRadius: '7px',
    }}>
      {/* Plugin icon */}
      <div style={{
        width:        '24px',
        height:       '24px',
        borderRadius: '6px',
        background:   'var(--blue)',
        display:      'flex',
        alignItems:   'center',
        justifyContent: 'center',
        flexShrink:   0,
        fontSize:     '11px',
        color:        '#fff',
        fontFamily:   'var(--mono)',
        fontWeight:   700,
      }}>
        T
      </div>
      <span style={{ fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--faint)', flexShrink: 0 }}>{prefix}</span>
      <span style={{ fontSize: '13px', color: 'var(--text)', flex: 1 }}>{name}</span>
      {/* Match score badge */}
      <span style={{
        fontFamily:   'var(--mono)',
        fontSize:     '10px',
        color:        score > 0.8 ? 'var(--green)' : 'var(--muted)',
        padding:      '1px 6px',
        borderRadius: '4px',
        background:   score > 0.8 ? 'var(--green-soft)' : 'var(--s3)',
        border:       `0.5px solid ${score > 0.8 ? 'rgba(34,197,94,0.3)' : 'var(--border)'}`,
      }}>
        {Math.round(score * 100)}%
      </span>
    </div>
  )
}

function EmbedLabel({ letter, title }: { letter: string; title: string }) {
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: '8px', marginBottom: '10px' }}>
      <span style={{
        fontFamily:    'var(--mono)',
        fontSize:      '10px',
        color:         'var(--accent)',
        background:    'var(--accent-soft)',
        border:        '0.5px solid var(--accent-rim)',
        borderRadius:  '4px',
        padding:       '1px 6px',
        letterSpacing: '0.08em',
        flexShrink:    0,
      }}>
        {letter}
      </span>
      <span style={{ fontFamily: 'var(--mono)', fontSize: '10.5px', color: 'var(--faint)', letterSpacing: '0.06em' }}>
        {title}
      </span>
    </div>
  )
}

export function PluginEmbedsScene() {
  return (
    <SceneShell
      id="3.10-plugin-embeds"
      title="plugin embeds — four embed points"
      hint="palette row · detail pane · sidebar widget · inline chat block"
    >
      <div style={{
        display:               'grid',
        gridTemplateColumns:   '1fr 1fr',
        gap:                   '16px',
        width:                 '820px',
      }}>
        {/* A: Palette result row */}
        <div style={{
          padding:      '16px',
          borderRadius: '10px',
          background:   'var(--s1)',
          border:       '0.5px solid var(--border)',
          display:      'flex',
          flexDirection:'column',
        }}>
          <EmbedLabel letter="A" title="palette result row" />
          <div style={{ display: 'flex', flexDirection: 'column', gap: '6px' }}>
            <PluginPaletteRow name="List open P0 issues" score={0.92} prefix="tracker:" />
            <PluginPaletteRow name="Create tracker issue" score={0.78} prefix="tracker:" />
            <PluginPaletteRow name="Update issue status" score={0.61} prefix="tracker:" />
          </div>
        </div>

        {/* B: Detail pane */}
        <div style={{
          padding:      '16px',
          borderRadius: '10px',
          background:   'var(--s1)',
          border:       '0.5px solid var(--border)',
          display:      'flex',
          flexDirection:'column',
        }}>
          <EmbedLabel letter="B" title="detail pane · Component[]" />
          <div style={{
            padding:      '12px 14px',
            borderRadius: '8px',
            background:   'var(--s0)',
            border:       '0.5px solid var(--border)',
            flex:         1,
          }}>
            <PluginComponentRenderer components={MOCK_PLUGIN_COMPONENTS} />
          </div>
        </div>

        {/* C: Sidebar widget */}
        <div style={{
          padding:      '16px',
          borderRadius: '10px',
          background:   'var(--s1)',
          border:       '0.5px solid var(--border)',
          display:      'flex',
          flexDirection:'column',
        }}>
          <EmbedLabel letter="C" title={`sidebar widget · refresh_secs countdown`} />
          <SidebarWidget />
        </div>

        {/* D: Inline chat block */}
        <div style={{
          padding:      '16px',
          borderRadius: '10px',
          background:   'var(--s1)',
          border:       '0.5px solid var(--border)',
          display:      'flex',
          flexDirection:'column',
        }}>
          <EmbedLabel letter="D" title="inline chat · AgentTurn / ToolCallBeat / CodeDiff / FileRef" />
          <div style={{ display: 'flex', flexDirection: 'column', gap: '7px' }}>
            {MOCK_CHAT_BLOCKS.map((block, i) => (
              <InlineChatBlock key={i} block={block} />
            ))}
          </div>
        </div>
      </div>
    </SceneShell>
  )
}

// ─── 3.11 · Notification tiers ────────────────────────────────────────────────

export function NotificationScene() {
  const startRef    = useRef(Date.now())
  const now         = useNow(true)
  const elapsedS    = Math.floor((now - startRef.current) / 1000)
  const [hudVisible, setHudVisible]   = useState(true)
  const [bannerDismissed, setBannerDismissed] = useState(false)

  // Fade HUD in/out every 4s
  useEffect(() => {
    const id = setInterval(() => setHudVisible(v => !v), 4000)
    return () => clearInterval(id)
  }, [])

  // Banner dismisses on any keystroke
  useEffect(() => {
    const onKey = () => setBannerDismissed(true)
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  const mins   = Math.floor(elapsedS / 60)
  const secs   = elapsedS % 60
  const workingLabel = mins > 0
    ? `${mins}m ${secs}s`
    : `${secs}s`

  return (
    <SceneShell
      id="3.11-notification"
      title="notification tiers — all three simultaneously"
      hint="(a) ambient HUD top-right  ·  (b) footer badge strip  ·  (c) terracotta banner — press any key to dismiss"
    >
      {/* Outer container that hosts both fixed overlays and main content */}
      <div style={{ position: 'relative', width: '820px' }}>

        {/* ── (a) Ambient HUD — top-right, position:absolute within scene ── */}
        <div style={{
          position:   'absolute',
          top:        0,
          right:      0,
          zIndex:     10,
          display:    'flex',
          flexDirection: 'column',
          alignItems: 'flex-end',
          gap:        '6px',
          pointerEvents: 'none',
        }}>
          <div style={{
            opacity:    hudVisible ? 1 : 0,
            transition: 'opacity 1.2s ease',
            display:    'flex',
            flexDirection: 'column',
            alignItems: 'flex-end',
            gap:        '5px',
          }}>
            {/* Context window pill */}
            <div style={{
              display:      'flex',
              alignItems:   'center',
              gap:          '8px',
              padding:      '4px 10px',
              borderRadius: '20px',
              background:   'var(--glass)',
              backdropFilter: 'blur(12px)',
              border:       '0.5px solid var(--border)',
              boxShadow:    '0 4px 16px rgba(0,0,0,0.5)',
            }}>
              <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--faint)', letterSpacing: '0.12em', textTransform: 'uppercase' }}>
                Context Window
              </span>
              <div style={{
                width:        '60px',
                height:       '4px',
                borderRadius: '2px',
                background:   'var(--border)',
                overflow:     'hidden',
              }}>
                <div style={{ width: '12%', height: '100%', background: 'var(--green)', borderRadius: '2px' }} />
              </div>
              <span style={{
                fontFamily:         'var(--mono)',
                fontSize:           '10px',
                color:              'var(--green)',
                fontVariantNumeric: 'tabular-nums',
              }}>
                12%
              </span>
            </div>
            {/* Working-for counter */}
            <div style={{
              padding:      '3px 10px',
              borderRadius: '20px',
              background:   'var(--glass)',
              backdropFilter: 'blur(12px)',
              border:       '0.5px solid var(--border)',
              boxShadow:    '0 4px 16px rgba(0,0,0,0.5)',
              display:      'flex',
              alignItems:   'center',
              gap:          '6px',
            }}>
              <span style={{ width: '5px', height: '5px', borderRadius: '50%', background: 'var(--accent)', boxShadow: '0 0 5px var(--accent)', animation: 'pulse 1.4s infinite' }} />
              <span style={{
                fontFamily:         'var(--mono)',
                fontSize:           '10px',
                color:              'var(--accent)',
                fontVariantNumeric: 'tabular-nums',
                letterSpacing:      '0.08em',
              }}>
                working for {workingLabel}
              </span>
            </div>
          </div>
        </div>

        {/* ── Main content area (palette mock) — no layout shift from HUD ── */}
        <div style={{
          borderRadius: '12px',
          background:   'var(--s1)',
          border:       '0.5px solid var(--border)',
          overflow:     'hidden',
          marginBottom: '12px',
        }}>
          {/* Input row */}
          <div style={{
            display:      'flex',
            alignItems:   'center',
            gap:          '10px',
            padding:      '10px 14px',
            borderBottom: bannerDismissed ? 'none' : '0.5px solid var(--border-soft)',
          }}>
            <span style={{ width: '7px', height: '7px', borderRadius: '50%', background: 'var(--ghost)' }} />
            <span style={{ fontFamily: 'var(--sans)', fontSize: '14px', color: 'var(--faint)', flex: 1 }}>
              Search Whim…
            </span>
          </div>

          {/* ── (c) Terracotta palette banner ── */}
          {!bannerDismissed && (
            <div style={{
              display:         'flex',
              alignItems:      'center',
              gap:             '10px',
              padding:         '8px 14px',
              background:      'rgba(218,119,87,0.18)',
              borderBottom:    '0.5px solid rgba(218,119,87,0.35)',
            }}>
              <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: '#DA7757', letterSpacing: '0.12em', textTransform: 'uppercase', flexShrink: 0 }}>
                ⚠ Notice
              </span>
              <span style={{ fontFamily: 'var(--sans)', fontSize: '12.5px', color: 'rgba(218,119,87,0.9)', flex: 1 }}>
                nen-daemon staleness detected — last heartbeat 4m ago. Run{' '}
                <code style={{ fontFamily: 'var(--mono)', fontSize: '11px', background: 'rgba(218,119,87,0.2)', padding: '0 4px', borderRadius: '3px' }}>nen-daemon restart</code>
              </span>
              <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'rgba(218,119,87,0.6)' }}>
                press any key to dismiss
              </span>
            </div>
          )}

          {/* Result rows placeholder */}
          <div style={{ padding: '8px 0' }}>
            {['Run tracker list --status open', 'Spawn mission: audit nen health', 'Open diff viewer for TRK-573'].map((item, i) => (
              <div key={i} style={{
                display:      'flex',
                alignItems:   'center',
                gap:          '10px',
                padding:      '7px 14px',
                background:   i === 0 ? 'var(--accent-soft)' : 'transparent',
              }}>
                <span style={{
                  width:      '18px',
                  height:     '18px',
                  borderRadius: '5px',
                  background: i === 0 ? 'var(--blue)' : 'var(--s3)',
                  border:     '0.5px solid var(--border)',
                  flexShrink: 0,
                }} />
                <span style={{ fontSize: '13px', color: i === 0 ? 'var(--text)' : 'var(--muted)' }}>{item}</span>
              </div>
            ))}
          </div>
        </div>

        {/* ── (b) Footer badge strip ── */}
        <div style={{
          display:      'flex',
          alignItems:   'center',
          gap:          '8px',
          padding:      '8px 14px',
          borderRadius: '8px',
          background:   'var(--s2)',
          border:       '0.5px solid var(--border)',
          flexWrap:     'wrap',
        }}>
          {/* Branch pill */}
          <span style={{
            fontFamily:   'var(--mono)',
            fontSize:     '10px',
            color:        'var(--muted)',
            padding:      '2px 8px',
            borderRadius: '4px',
            background:   'var(--s3)',
            border:       '0.5px solid var(--border)',
            letterSpacing: '0.04em',
          }}>
            ⎇ via/20260426-82c2eeac
          </span>
          {/* Nen staleness pill */}
          <span style={{
            fontFamily:   'var(--mono)',
            fontSize:     '10px',
            color:        '#DA7757',
            padding:      '2px 8px',
            borderRadius: '4px',
            background:   'rgba(218,119,87,0.12)',
            border:       '0.5px solid rgba(218,119,87,0.3)',
          }}>
            nen stale 4m
          </span>
          {/* Scheduler overdue pill */}
          <span style={{
            fontFamily:   'var(--mono)',
            fontSize:     '10px',
            color:        'var(--red)',
            padding:      '2px 8px',
            borderRadius: '4px',
            background:   'var(--red-soft)',
            border:       '0.5px solid rgba(239,68,68,0.3)',
          }}>
            2 jobs overdue
          </span>
          <span style={{ marginLeft: 'auto', fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--ghost)', fontVariantNumeric: 'tabular-nums' }}>
            {new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
          </span>
        </div>
      </div>
    </SceneShell>
  )
}

// ─── 3.12 · Error / retry ─────────────────────────────────────────────────────

export function ErrorRetryScene() {
  const [inputValue, setInputValue]   = useState('')
  const [inputError, setInputError]   = useState<string | null>(null)
  const [submitted, setSubmitted]     = useState(false)

  const handleSubmit = () => {
    if (!inputValue.trim()) {
      setInputError('Mission description cannot be empty.')
      return
    }
    if (inputValue.trim().length < 8) {
      setInputError('Description too short — minimum 8 characters.')
      return
    }
    setInputError(null)
    setSubmitted(true)
    setTimeout(() => setSubmitted(false), 1800)
  }

  return (
    <SceneShell
      id="3.12-error-retry"
      title="error / retry — three flavors"
      hint="(i) composer validation  ·  (ii) runtime tool failure  ·  (iii) diff apply failure"
    >
      <div style={{ display: 'flex', flexDirection: 'column', gap: '16px', width: '820px' }}>

        {/* ── (i) Composer validation error ── */}
        <div style={{
          borderRadius: '10px',
          background:   'var(--s1)',
          border:       '0.5px solid var(--border)',
          overflow:     'hidden',
        }}>
          <div style={{
            padding:      '10px 14px',
            borderBottom: '0.5px solid var(--border-soft)',
            background:   'var(--s2)',
            display:      'flex',
            alignItems:   'center',
            gap:          '8px',
          }}>
            <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--faint)', letterSpacing: '0.1em', textTransform: 'uppercase' }}>i</span>
            <span style={{ fontFamily: 'var(--mono)', fontSize: '10.5px', color: 'var(--muted)' }}>Composer — input validation</span>
          </div>
          <div style={{ padding: '14px 16px', display: 'flex', flexDirection: 'column', gap: '8px' }}>
            <div style={{
              display:      'flex',
              alignItems:   'center',
              gap:          '10px',
              padding:      '9px 13px',
              borderRadius: '8px',
              background:   'var(--s0)',
              border:       `0.5px solid ${inputError ? 'var(--red)' : 'var(--border)'}`,
              boxShadow:    inputError ? '0 0 0 2px rgba(239,68,68,0.12)' : 'none',
              transition:   'border-color 0.15s, box-shadow 0.15s',
            }}>
              <input
                value={inputValue}
                onChange={e => { setInputValue(e.target.value); if (inputError) setInputError(null) }}
                onKeyDown={e => { if (e.key === 'Enter') handleSubmit() }}
                placeholder="Describe the mission…"
                style={{
                  flex:       1,
                  background: 'transparent',
                  border:     'none',
                  outline:    'none',
                  fontFamily: 'var(--sans)',
                  fontSize:   '14px',
                  color:      'var(--text)',
                }}
              />
              <button
                type="button"
                onClick={handleSubmit}
                style={{
                  fontFamily:   'var(--mono)',
                  fontSize:     '11px',
                  color:        submitted ? 'var(--green)' : 'var(--accent)',
                  background:   submitted ? 'var(--green-soft)' : 'var(--accent-soft)',
                  border:       `0.5px solid ${submitted ? 'rgba(34,197,94,0.35)' : 'var(--accent-rim)'}`,
                  borderRadius: '5px',
                  padding:      '4px 10px',
                  cursor:       'pointer',
                  transition:   'all 0.15s',
                }}
              >
                {submitted ? '✓' : '↵ Run'}
              </button>
            </div>
            {inputError && (
              <div style={{
                display:      'flex',
                alignItems:   'center',
                gap:          '6px',
                fontFamily:   'var(--mono)',
                fontSize:     '11px',
                color:        'var(--red)',
              }}>
                <span>⚠</span>
                <span>{inputError}</span>
              </div>
            )}
          </div>
        </div>

        {/* ── (ii) Runtime tool/phase failure ── */}
        <div style={{
          borderRadius: '10px',
          background:   'var(--s1)',
          border:       '0.5px solid var(--border)',
          overflow:     'hidden',
        }}>
          <div style={{
            padding:      '10px 14px',
            borderBottom: '0.5px solid var(--border-soft)',
            background:   'var(--s2)',
            display:      'flex',
            alignItems:   'center',
            gap:          '8px',
          }}>
            <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--faint)', letterSpacing: '0.1em', textTransform: 'uppercase' }}>ii</span>
            <span style={{ fontFamily: 'var(--mono)', fontSize: '10.5px', color: 'var(--muted)' }}>Runtime — tool / phase failure</span>
          </div>
          <div style={{ padding: '14px 16px', display: 'flex', flexDirection: 'column', gap: '8px' }}>
            {/* Successful phase */}
            <div style={{
              display:      'flex',
              alignItems:   'center',
              gap:          '10px',
              padding:      '8px 12px',
              borderRadius: '7px',
              background:   'var(--s0)',
              border:       '0.5px solid var(--border)',
              borderLeft:   '3px solid var(--green)',
            }}>
              <span style={{ fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--green)' }}>✓</span>
              <span style={{ fontFamily: 'var(--mono)', fontSize: '12px', color: 'var(--text)', flex: 1 }}>senior-frontend-engineer</span>
              <span style={{ fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--faint)' }}>phase-6 · 4m 12s</span>
            </div>
            {/* Failed phase */}
            <div style={{
              borderRadius: '7px',
              border:       '0.5px solid rgba(239,68,68,0.4)',
              borderLeft:   '3px solid var(--red)',
              overflow:     'hidden',
              background:   'var(--s0)',
            }}>
              <div style={{
                display:      'flex',
                alignItems:   'center',
                gap:          '10px',
                padding:      '8px 12px',
                background:   'rgba(239,68,68,0.05)',
                borderBottom: '0.5px solid rgba(239,68,68,0.2)',
              }}>
                <span style={{ fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--red)' }}>✗</span>
                <span style={{ fontFamily: 'var(--mono)', fontSize: '12px', color: 'var(--text)', flex: 1 }}>senior-frontend-engineer</span>
                <span style={{
                  fontFamily:   'var(--mono)',
                  fontSize:     '10px',
                  color:        'var(--red)',
                  padding:      '1px 6px',
                  borderRadius: '4px',
                  background:   'var(--red-soft)',
                  border:       '0.5px solid rgba(239,68,68,0.35)',
                }}>
                  GATE FAILED
                </span>
              </div>
              <div style={{ padding: '8px 12px', fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--red)', lineHeight: 1.6 }}>
                npm run build exited 1 — TypeScript error in src/scenarios/Scenes.tsx:42<br />
                <span style={{ color: 'var(--faint)' }}>Type 'string' is not assignable to type 'ReactNode'.</span>
              </div>
            </div>
          </div>
        </div>

        {/* ── (iii) Diff apply failure artboard ── */}
        <div style={{
          borderRadius: '10px',
          background:   'var(--s1)',
          border:       '0.5px solid var(--border)',
          overflow:     'hidden',
        }}>
          <div style={{
            padding:      '10px 14px',
            borderBottom: '0.5px solid var(--border-soft)',
            background:   'var(--s2)',
            display:      'flex',
            alignItems:   'center',
            gap:          '8px',
          }}>
            <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--faint)', letterSpacing: '0.1em', textTransform: 'uppercase' }}>iii</span>
            <span style={{ fontFamily: 'var(--mono)', fontSize: '10.5px', color: 'var(--muted)' }}>Diff — apply failure artboard</span>
          </div>
          <div style={{ padding: '14px 16px' }}>
            <div style={{
              borderRadius:  '8px',
              border:        '0.5px solid rgba(239,68,68,0.4)',
              borderLeft:    '3px solid var(--red)',
              overflow:      'hidden',
            }}>
              {/* Error header */}
              <div style={{
                display:      'flex',
                alignItems:   'center',
                gap:          '8px',
                padding:      '8px 12px',
                background:   'rgba(239,68,68,0.07)',
                borderBottom: '0.5px solid rgba(239,68,68,0.2)',
              }}>
                <span style={{ fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--red)' }}>✗  Apply failed</span>
                <span style={{ fontFamily: 'var(--mono)', fontSize: '10.5px', color: 'var(--faint)', flex: 1 }}>
                  — expected context not found in target file
                </span>
                <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--faint)' }}>
                  src/components/PaletteShell.tsx:68
                </span>
              </div>
              {/* Expected vs found */}
              <div style={{
                padding:    '10px 14px',
                background: 'var(--s0)',
                fontFamily: 'var(--mono)',
                fontSize:   '11.5px',
                lineHeight: 1.7,
              }}>
                <div style={{ marginBottom: '4px' }}>
                  <span style={{ color: 'var(--ghost)' }}>expected  </span>
                  <span style={{ color: 'var(--red)' }}>hints, onQueryChange, onResultSelect, onSubmit, onEsc,</span>
                </div>
                <div>
                  <span style={{ color: 'var(--ghost)' }}>found     </span>
                  <span style={{ color: 'var(--green)' }}>hints, warmBleed, onQueryChange, onResultSelect, onSubmit,</span>
                </div>
              </div>
              {/* Retry footer */}
              <div style={{
                display:      'flex',
                alignItems:   'center',
                gap:          '8px',
                padding:      '8px 12px',
                borderTop:    '0.5px solid rgba(239,68,68,0.15)',
                background:   'rgba(239,68,68,0.04)',
              }}>
                <span className="K rem">r</span>
                <span style={{ fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--faint)' }}>
                  retry apply — re-run patch with fuzzy context matching
                </span>
              </div>
            </div>
          </div>
        </div>

      </div>
    </SceneShell>
  )
}

// ─── 99 · Plugin shell (closer) ───────────────────────────────────────────────

function RefTable({
  rows,
  cols,
}: {
  rows: string[][]
  cols: string[]
}) {
  return (
    <div style={{ overflowX: 'auto' }}>
      <table style={{ width: '100%', borderCollapse: 'collapse', fontFamily: 'var(--mono)', fontSize: '11.5px' }}>
        <thead>
          <tr>
            {cols.map(c => (
              <th key={c} style={{
                padding:       '7px 12px',
                textAlign:     'left',
                color:         'var(--ghost)',
                fontSize:      '10px',
                letterSpacing: '0.12em',
                textTransform: 'uppercase',
                borderBottom:  '0.5px solid var(--border)',
                whiteSpace:    'nowrap',
              }}>
                {c}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr key={i} style={{ borderBottom: '0.5px solid var(--border-soft)' }}>
              {row.map((cell, j) => (
                <td key={j} style={{
                  padding:    '7px 12px',
                  color:      j === 0 ? 'var(--text)' : 'var(--muted)',
                  whiteSpace: 'nowrap',
                }}>
                  {cell}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

function SectionCard({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div style={{
      borderRadius: '10px',
      background:   'var(--s1)',
      border:       '0.5px solid var(--border)',
      overflow:     'hidden',
    }}>
      <div style={{
        padding:      '10px 16px',
        borderBottom: '0.5px solid var(--border-soft)',
        background:   'var(--s2)',
        fontFamily:   'var(--mono)',
        fontSize:     '10.5px',
        color:        'var(--muted)',
        letterSpacing: '0.1em',
        textTransform: 'uppercase',
      }}>
        {title}
      </div>
      {children}
    </div>
  )
}

export function PluginShellScene() {
  return (
    <SceneShell
      id="99-plugin-shell"
      title="plugin shell — host contract reference"
      hint="embed points · verb → prefix map · host keymap"
    >
      <div style={{ display: 'flex', flexDirection: 'column', gap: '14px', width: '820px' }}>

        {/* Four embed points */}
        <div data-tour-anchor="plugin-shell-table">
        <SectionCard title="Embed points">
          <RefTable
            cols={['Point', 'Surface', 'Renders', 'Refresh']}
            rows={[
              ['A · palette-row',     'L1 Palette result',    'icon + label + match score badge',              'on query change'],
              ['B · detail-pane',     'L2 Detail side panel', 'Component[] — Markdown · List · Divider · Image', 'on select'],
              ['C · sidebar-widget',  'L3 Canvas rail',       'title + body + progress bar',                   'refresh_secs countdown'],
              ['D · inline-chat',     'L3 Canvas main',       'AgentTurn · ToolCallBeat · CodeDiff · FileRef', 'streaming SSE'],
            ]}
          />
        </SectionCard>
        </div>

        {/* Verb → prefix map */}
        <SectionCard title="Verb → palette prefix map">
          <RefTable
            cols={['Plugin', 'Verbs', 'Palette prefix', 'Example query']}
            rows={[
              ['tracker',    'list · create · update · close',        'tracker:',  'tracker: list open P0'],
              ['mission',    'run · spawn · status · stop · resume',  'mission:',  'mission: spawn audit nen'],
              ['engage',     'scan · draft · post · approve',         'engage:',   'engage: draft linkedin post'],
              ['obsidian',   'read · write · search · capture',       'obs:',      'obs: capture meeting notes'],
              ['scheduler',  'list · add · remove · pause',           'sched:',    'sched: list failed jobs'],
              ['scout',      'query · report · alert',                'scout:',    'scout: report ai-tools'],
            ]}
          />
        </SectionCard>

        {/* Host keymap */}
        <div data-tour-anchor="keymap-card">
        <SectionCard title="Host keymap">
          <div style={{ padding: '14px 16px' }}>
            <div style={{
              display:             'grid',
              gridTemplateColumns: 'auto 1fr',
              gap:                 '10px 16px',
              alignItems:          'center',
            }}>
              {([
                ['⌥Space', 'Push-to-talk — open voice overlay (hold) / commit on release'],
                ['⌘N',     'New conversation — reset palette and open fresh composer'],
                ['⌘T',     'New tab — open a parallel canvas alongside current mission'],
                ['⌘E',     'Expand / collapse — toggle palette ↔ canvas (L1 ↔ L3)'],
                ['⌘K',     'Command palette — jump to global action search'],
                ['⌘⇧E',   'Export — snapshot current canvas as markdown artifact'],
                ['Esc',    'Step back — follow Esc Ladder: canvas → palette → pill → idle'],
              ] as [string, string][]).map(([key, desc]) => (
                <>
                  <span key={`k-${key}`} className="K acc" style={{ justifySelf: 'start', whiteSpace: 'nowrap' }}>{key}</span>
                  <span key={`d-${key}`} style={{ fontFamily: 'var(--sans)', fontSize: '13px', color: 'var(--muted)' }}>{desc}</span>
                </>
              ))}
            </div>
          </div>
        </SectionCard>
        </div>

        {/* Plugin registration quick-ref */}
        <SectionCard title="Plugin registration (plugin.json excerpt)">
          <div style={{ padding: '12px 16px' }}>
            <pre style={{
              margin:     0,
              fontFamily: 'var(--mono)',
              fontSize:   '11.5px',
              color:      'var(--muted)',
              lineHeight: 1.7,
              overflowX:  'auto',
            }}>{`{
  "name": "tracker",
  "prefix": "tracker:",
  "embed_points": ["palette-row", "sidebar-widget"],
  "refresh_secs": 30,
  "verbs": ["list", "create", "update", "close"],
  "icon": "T",
  "color": "#3B82F6"
}`}</pre>
          </div>
        </SectionCard>

      </div>
    </SceneShell>
  )
}

// ─── 3.9b · Diff viewer (HERO) ────────────────────────────────────────────────

type HunkStatus = 'pending' | 'accepted' | 'rejected' | 'applied' | 'failed'

interface DiffViewState {
  statuses:       Record<string, HunkStatus>
  cursor:         { fileIndex: number; hunkIndex: number }
  undoHunkId:     string | null
  sessionRetried: string[]
  commitToast:    { applied: number; failed: number } | null
}

type DiffViewAction =
  | { type: 'ACCEPT';           hunkId: string }
  | { type: 'REJECT';           hunkId: string }
  | { type: 'ACCEPT_FILE';      fileIndex: number }
  | { type: 'ACCEPT_FILE_FULL'; fileIndex: number }
  | { type: 'ACCEPT_ALL' }
  | { type: 'COMMIT' }
  | { type: 'UNDO' }
  | { type: 'RETRY';            hunkId: string }
  | { type: 'CURSOR';           fileIndex: number; hunkIndex: number }
  | { type: 'DISMISS_TOAST' }

function diffReducer(state: DiffViewState, action: DiffViewAction): DiffViewState {
  switch (action.type) {
    case 'ACCEPT': {
      if (state.statuses[action.hunkId] !== 'pending') return state
      return { ...state, statuses: { ...state.statuses, [action.hunkId]: 'accepted' } }
    }
    case 'REJECT': {
      if (state.statuses[action.hunkId] !== 'pending') return state
      return { ...state, statuses: { ...state.statuses, [action.hunkId]: 'rejected' } }
    }
    case 'ACCEPT_FILE': {
      const file = CHANGED_FILES[action.fileIndex]
      if (!file) return state
      const next = { ...state.statuses }
      file.hunks.forEach(h => { if (next[h.id] === 'pending') next[h.id] = 'accepted' })
      return { ...state, statuses: next }
    }
    case 'ACCEPT_FILE_FULL': {
      const file = CHANGED_FILES[action.fileIndex]
      if (!file) return state
      const next = { ...state.statuses }
      file.hunks.forEach(h => {
        if (next[h.id] === 'pending' || next[h.id] === 'accepted') next[h.id] = 'accepted'
      })
      return { ...state, statuses: next }
    }
    case 'ACCEPT_ALL': {
      const next = { ...state.statuses }
      CHANGED_FILES.forEach(f => f.hunks.forEach(h => {
        if (next[h.id] === 'pending' || next[h.id] === 'accepted') next[h.id] = 'accepted'
      }))
      return { ...state, statuses: next }
    }
    case 'COMMIT': {
      const next      = { ...state.statuses }
      let applied     = 0
      let failed      = 0
      let undoHunkId: string | null = null
      CHANGED_FILES.forEach(f => f.hunks.forEach(h => {
        if (next[h.id] !== 'accepted') return
        const isForced = h.forcedFailOnApply && !state.sessionRetried.includes(h.id)
        if (isForced) {
          next[h.id] = 'failed'
          failed++
        } else {
          next[h.id] = 'applied'
          applied++
          undoHunkId = h.id
        }
      }))
      if (applied === 0 && failed === 0) return state
      return { ...state, statuses: next, undoHunkId, commitToast: { applied, failed } }
    }
    case 'UNDO': {
      if (!state.undoHunkId || state.statuses[state.undoHunkId] !== 'applied') return state
      return {
        ...state,
        statuses:    { ...state.statuses, [state.undoHunkId]: 'pending' },
        undoHunkId:  null,
        commitToast: null,
      }
    }
    case 'RETRY': {
      if (state.statuses[action.hunkId] !== 'failed') return state
      return {
        ...state,
        statuses:       { ...state.statuses, [action.hunkId]: 'pending' },
        sessionRetried: [...state.sessionRetried, action.hunkId],
      }
    }
    case 'CURSOR': {
      return { ...state, cursor: { fileIndex: action.fileIndex, hunkIndex: action.hunkIndex } }
    }
    case 'DISMISS_TOAST': {
      return { ...state, commitToast: null }
    }
  }
}

function initDiffState(): DiffViewState {
  const statuses: Record<string, HunkStatus> = {}
  CHANGED_FILES.forEach(f => f.hunks.forEach(h => { statuses[h.id] = 'pending' }))
  return { statuses, cursor: { fileIndex: 0, hunkIndex: 0 }, undoHunkId: null, sessionRetried: [], commitToast: null }
}

// ── HunkCard ──────────────────────────────────────────────────────────────────

function HunkCard({
  hunk,
  status,
  isActive,
  onAccept,
  onReject,
  onRetry,
  scrollRef,
}: {
  hunk:      Hunk
  status:    HunkStatus
  isActive:  boolean
  onAccept?: () => void
  onReject?: () => void
  onRetry?:  () => void
  scrollRef?: (el: HTMLDivElement | null) => void
}) {
  const borderColor =
    status === 'accepted' ? 'rgba(34,197,94,0.4)' :
    status === 'rejected' ? 'rgba(239,68,68,0.3)' :
    status === 'failed'   ? 'rgba(239,68,68,0.5)' :
    isActive              ? 'var(--accent-rim)'    :
    'var(--border)'

  const leftBorder =
    status === 'accepted' ? '3px solid var(--green)' :
    status === 'rejected' ? '3px solid rgba(239,68,68,0.4)' :
    status === 'failed'   ? '3px solid var(--red)' :
    isActive              ? '3px solid var(--accent)' :
    '3px solid transparent'

  return (
    <div
      ref={scrollRef}
      style={{
        borderRadius:  '8px',
        border:        `0.5px solid ${borderColor}`,
        borderLeft:    leftBorder,
        marginBottom:  '10px',
        overflow:      'hidden',
        transition:    'border-color 0.15s, border-left 0.15s',
        outline:       isActive && status === 'pending' ? '1px solid rgba(212,120,86,0.18)' : 'none',
        outlineOffset: '2px',
      }}
    >
      {/* Hunk header */}
      <div style={{
        display:        'flex',
        alignItems:     'center',
        padding:        '6px 10px',
        background:     'var(--s2)',
        borderBottom:   '0.5px solid var(--border-soft)',
        gap:            '8px',
      }}>
        <span style={{
          fontFamily:    'var(--mono)',
          fontSize:      '11px',
          color:         'var(--faint)',
          flex:          1,
          overflow:      'hidden',
          textOverflow:  'ellipsis',
          whiteSpace:    'nowrap',
        }}>
          {hunk.header}
          {hunk.forcedFailOnApply && (
            <span style={{ marginLeft: '8px', color: 'var(--red)', fontSize: '10px' }}>⚠ conflict</span>
          )}
        </span>
        <div style={{ display: 'flex', gap: '5px', alignItems: 'center', flexShrink: 0 }}>
          {status === 'pending' && (
            <>
              <button
                type="button"
                className="K add"
                title="Accept hunk (y)"
                onClick={onAccept}
                style={{ cursor: 'pointer', border: 'none' }}
              >
                y
              </button>
              <button
                type="button"
                className="K rem"
                title="Reject hunk (n)"
                onClick={onReject}
                style={{ cursor: 'pointer', border: 'none' }}
              >
                n
              </button>
            </>
          )}
          {status === 'accepted' && (
            <span className="K add" aria-label="Accepted">y</span>
          )}
          {status === 'rejected' && (
            <span className="K rem" aria-label="Rejected">n</span>
          )}
          {status === 'applied' && (
            <span style={{ fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--green)' }}>✓</span>
          )}
          {status === 'failed' && (
            <span style={{ fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--red)' }}>✗</span>
          )}
        </div>
      </div>

      {/* Hunk body */}
      {status === 'applied' ? (
        <div style={{
          padding:    '8px 14px',
          background: 'rgba(34,197,94,0.06)',
          fontFamily: 'var(--mono)',
          fontSize:   '12px',
          color:      'var(--green)',
          display:    'flex',
          alignItems: 'center',
          gap:        '8px',
        }}>
          <span aria-hidden="true">✓</span>
          <span>Applied</span>
        </div>
      ) : status === 'failed' ? (
        <div style={{ padding: '12px 14px', background: 'rgba(239,68,68,0.05)' }}>
          <div style={{
            fontFamily:   'var(--mono)',
            fontSize:     '11px',
            color:        'var(--red)',
            marginBottom: '10px',
            letterSpacing: '0.04em',
          }}>
            ✗  Apply failed — expected context not found in target file
          </div>
          <div style={{
            background:   'var(--s0)',
            borderRadius: '5px',
            padding:      '8px 10px',
            fontFamily:   'var(--mono)',
            fontSize:     '11px',
            marginBottom: '10px',
          }}>
            <div style={{ marginBottom: '3px' }}>
              <span style={{ color: 'var(--faint)' }}>expected  </span>
              <span style={{ color: 'var(--red)' }}>hints, onQueryChange, onResultSelect, onSubmit, onEsc,</span>
            </div>
            <div>
              <span style={{ color: 'var(--faint)' }}>found     </span>
              <span style={{ color: 'var(--green)' }}>hints, warmBleed, onQueryChange, onResultSelect, onSubmit,</span>
            </div>
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
            <button
              type="button"
              className="K"
              title="Retry hunk (r)"
              onClick={onRetry}
              style={{ cursor: 'pointer' }}
            >
              r
            </button>
            <span style={{ fontFamily: 'var(--mono)', fontSize: '11px', color: 'var(--faint)' }}>
              retry
            </span>
          </div>
        </div>
      ) : (
        <div style={{ opacity: status === 'rejected' ? 0.45 : 1, transition: 'opacity 0.15s' }}>
          {hunk.lines.map((line: HunkLine, idx: number) => (
            <div
              key={idx}
              style={{
                display:    'flex',
                background: line.type === 'add' ? 'var(--green-soft)' :
                            line.type === 'rem' ? 'var(--red-soft)'   : 'transparent',
                padding:    '2px 10px',
              }}
            >
              <span
                aria-hidden="true"
                style={{
                  fontFamily:  'var(--mono)',
                  fontSize:    '12px',
                  color:       line.type === 'add' ? 'var(--green)' :
                               line.type === 'rem' ? 'var(--red)'   : 'var(--ghost)',
                  marginRight: '10px',
                  userSelect:  'none',
                  flexShrink:  0,
                  width:       '10px',
                }}
              >
                {line.type === 'add' ? '+' : line.type === 'rem' ? '-' : ' '}
              </span>
              <span style={{
                fontFamily:  'var(--mono)',
                fontSize:    '12px',
                color:       line.type === 'ctx' ? 'var(--muted)' : 'var(--text)',
                whiteSpace:  'pre',
                overflow:    'hidden',
              }}>
                {line.content || ' '}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

// ── DiffViewerScene ───────────────────────────────────────────────────────────

export function DiffViewerScene() {
  const [state, dispatch] = useReducer(diffReducer, undefined, initDiffState)
  const activeHunkRef     = useRef<HTMLDivElement | null>(null)

  const flatHunks = CHANGED_FILES.flatMap((f, fi) =>
    f.hunks.map((_h, hi) => ({ fileIndex: fi, hunkIndex: hi }))
  )

  const flatIndex = flatHunks.findIndex(
    p => p.fileIndex === state.cursor.fileIndex && p.hunkIndex === state.cursor.hunkIndex
  )

  // Auto-scroll to cursor hunk
  useEffect(() => {
    activeHunkRef.current?.scrollIntoView({ behavior: 'smooth', block: 'nearest' })
  }, [state.cursor.fileIndex, state.cursor.hunkIndex])

  // Keyboard handler
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const fi = state.cursor.fileIndex
      const hi = state.cursor.hunkIndex
      const currentFile   = CHANGED_FILES[fi]
      const currentHunkId = currentFile?.hunks[hi]?.id

      if (e.metaKey && e.key === '[') {
        e.preventDefault()
        const prev = Math.max(fi - 1, 0)
        dispatch({ type: 'CURSOR', fileIndex: prev, hunkIndex: 0 })
      } else if (e.metaKey && e.key === ']') {
        e.preventDefault()
        const next = Math.min(fi + 1, CHANGED_FILES.length - 1)
        dispatch({ type: 'CURSOR', fileIndex: next, hunkIndex: 0 })
      } else if (e.metaKey && e.key === 'z') {
        e.preventDefault()
        dispatch({ type: 'UNDO' })
      } else if (e.key === 'y' && !e.metaKey && !e.shiftKey) {
        e.preventDefault()
        if (currentHunkId) dispatch({ type: 'ACCEPT', hunkId: currentHunkId })
      } else if (e.key === 'n' && !e.metaKey && !e.shiftKey) {
        e.preventDefault()
        if (currentHunkId) dispatch({ type: 'REJECT', hunkId: currentHunkId })
      } else if (e.key === 'j' && !e.metaKey && !e.shiftKey) {
        e.preventDefault()
        const next = flatHunks[Math.min(flatIndex + 1, flatHunks.length - 1)]
        if (next) dispatch({ type: 'CURSOR', fileIndex: next.fileIndex, hunkIndex: next.hunkIndex })
      } else if (e.key === 'k' && !e.metaKey && !e.shiftKey) {
        e.preventDefault()
        const prev = flatHunks[Math.max(flatIndex - 1, 0)]
        if (prev) dispatch({ type: 'CURSOR', fileIndex: prev.fileIndex, hunkIndex: prev.hunkIndex })
      } else if (e.key === 'a' && !e.metaKey && !e.shiftKey) {
        e.preventDefault()
        dispatch({ type: 'ACCEPT_FILE', fileIndex: fi })
      } else if (e.key === 'Y' && !e.metaKey) {
        e.preventDefault()
        dispatch({ type: 'ACCEPT_FILE_FULL', fileIndex: fi })
      } else if (e.key === 'A' && e.shiftKey && !e.metaKey) {
        e.preventDefault()
        dispatch({ type: 'ACCEPT_ALL' })
      } else if (e.key === 'c' && !e.metaKey && !e.shiftKey) {
        e.preventDefault()
        dispatch({ type: 'COMMIT' })
      } else if (e.key === 'r' && !e.metaKey && !e.shiftKey) {
        e.preventDefault()
        if (currentHunkId) dispatch({ type: 'RETRY', hunkId: currentHunkId })
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [state.cursor, flatIndex, flatHunks])

  // Compute status counts across all files
  const allStatuses = Object.values(state.statuses)
  const counts = {
    pending:  allStatuses.filter(s => s === 'pending').length,
    accepted: allStatuses.filter(s => s === 'accepted').length,
    rejected: allStatuses.filter(s => s === 'rejected').length,
    applied:  allStatuses.filter(s => s === 'applied').length,
    failed:   allStatuses.filter(s => s === 'failed').length,
  }

  const currentFile = CHANGED_FILES[state.cursor.fileIndex]

  // Rail items = file tabs
  const railItems: RailItem[] = CHANGED_FILES.map((f, i) => ({
    id:      f.path,
    label:   f.path.split('/').pop() ?? f.path,
    active:  i === state.cursor.fileIndex,
    meta:    `+${f.additions}${f.deletions ? ` -${f.deletions}` : ''}`,
    working: f.hunks.some(h => {
      const st = state.statuses[h.id]
      return st === 'pending' || st === 'accepted'
    }),
  }))

  const mainContent = (
    <div style={{ display: 'flex', flexDirection: 'column' }}>
      {/* Status bar + commit toast */}
      <div style={{
        display:        'flex',
        alignItems:     'center',
        justifyContent: 'space-between',
        marginBottom:   '14px',
        flexWrap:       'wrap',
        gap:            '8px',
      }}>
        <div style={{
          fontFamily:  'var(--mono)',
          fontSize:    '11px',
          display:     'flex',
          gap:         '12px',
          flexWrap:    'wrap',
        }}>
          {counts.accepted > 0 && (
            <span style={{ color: 'var(--green)' }}>{counts.accepted} accepted</span>
          )}
          {counts.rejected > 0 && (
            <span style={{ color: 'var(--red)' }}>{counts.rejected} rejected</span>
          )}
          {counts.applied > 0 && (
            <span style={{ color: 'var(--green)' }}>{counts.applied} applied</span>
          )}
          {counts.failed > 0 && (
            <span style={{ color: 'var(--red)' }}>{counts.failed} failed</span>
          )}
          <span style={{ color: 'var(--faint)' }}>{counts.pending} pending</span>
        </div>

        {state.commitToast && (
          <div style={{
            padding:      '4px 10px',
            borderRadius: '5px',
            background:   state.commitToast.failed > 0 ? 'var(--red-soft)' : 'var(--green-soft)',
            border:       `0.5px solid ${state.commitToast.failed > 0 ? 'rgba(239,68,68,0.35)' : 'rgba(34,197,94,0.35)'}`,
            fontFamily:   'var(--mono)',
            fontSize:     '11px',
            color:        state.commitToast.failed > 0 ? 'var(--red)' : 'var(--green)',
            letterSpacing: '0.06em',
            display:      'flex',
            alignItems:   'center',
            gap:          '8px',
          }}>
            {state.commitToast.failed > 0
              ? `✗ ${state.commitToast.failed} failed · ${state.commitToast.applied} applied`
              : `✓ Applied ${state.commitToast.applied}`}
            {state.undoHunkId && (
              <span style={{ color: 'var(--faint)', fontSize: '10px' }}>
                <span className="K nav">⌘Z</span> undo
              </span>
            )}
          </div>
        )}
      </div>

      {/* Current file heading */}
      <div style={{
        fontFamily:   'var(--mono)',
        fontSize:     '11px',
        color:        'var(--muted)',
        marginBottom: '10px',
        letterSpacing: '0.04em',
        display:      'flex',
        alignItems:   'center',
        gap:          '8px',
      }}>
        <span>{currentFile?.path}</span>
        <span style={{ color: 'var(--ghost)' }}>
          {state.cursor.fileIndex + 1}/{CHANGED_FILES.length}
        </span>
        <span style={{ marginLeft: 'auto', display: 'flex', gap: '6px' }}>
          <span className="K nav">⌘[</span>
          <span style={{ color: 'var(--ghost)', fontSize: '10px' }}>prev file</span>
          <span className="K nav">⌘]</span>
          <span style={{ color: 'var(--ghost)', fontSize: '10px' }}>next file</span>
        </span>
      </div>

      {/* Hunk cards */}
      {currentFile?.hunks.map((hunk, hi) => {
        const isActive = hi === state.cursor.hunkIndex
        const status   = state.statuses[hunk.id] ?? 'pending'
        return (
          <HunkCard
            key={hunk.id}
            hunk={hunk}
            status={status}
            isActive={isActive}
            scrollRef={isActive ? el => { activeHunkRef.current = el } : undefined}
            onAccept={() => dispatch({ type: 'ACCEPT', hunkId: hunk.id })}
            onReject={() => dispatch({ type: 'REJECT', hunkId: hunk.id })}
            onRetry={()  => dispatch({ type: 'RETRY',  hunkId: hunk.id })}
          />
        )
      })}
    </div>
  )

  return (
    <SceneShell
      id="3.9b-diff-viewer"
      title="diff viewer — HERO surface"
      hint="y=Accept  n=Reject  j/k=Navigate  a=Accept file  Y=Accept all in file  ⇧A=Accept all  c=Commit  ⌘Z=Undo  r=Retry"
    >
      <PaletteShell
        scale="canvas"
        breadcrumb={`review · ${currentFile?.path ?? ''}`}
        railItems={railItems}
        mainContent={mainContent}
        onResultSelect={id => {
          const fi = CHANGED_FILES.findIndex(f => f.path === id)
          if (fi >= 0) dispatch({ type: 'CURSOR', fileIndex: fi, hunkIndex: 0 })
        }}
        hints={[
          { keys: ['y'],         label: 'Accept' },
          { keys: ['n'],         label: 'Reject' },
          { keys: ['j', 'k'],    label: 'Navigate' },
          { keys: ['a'],         label: 'Accept file' },
          { keys: ['c'],         label: 'Commit' },
          { keys: ['⌘Z'],        label: 'Undo' },
          { keys: ['r'],         label: 'Retry' },
        ]}
      />
    </SceneShell>
  )
}

// ─── projects-tree ────────────────────────────────────────────────────────────

export function ProjectsTreeScene() {
  return (
    <SceneShell
      id="projects-tree"
      title="projects tree — T3-style sidebar with search, sort, expand"
      hint="j/k=navigate  o=expand/collapse  ↵=open  ⌘K=search"
    >
      <div style={{ display: 'flex', gap: '32px', alignItems: 'flex-start' }}>
        <ProjectsTree
          projects={MOCK_PROJECTS}
          onThreadSelect={(pId, tId) => console.log('open thread', pId, tId)}
          onProjectToggle={pId => console.log('toggle', pId)}
        />
        <div style={{
          display:       'flex',
          flexDirection: 'column',
          gap:           '10px',
          paddingTop:    '8px',
          fontFamily:    'var(--mono)',
          fontSize:      '11px',
          color:         'var(--faint)',
          maxWidth:      '260px',
          lineHeight:    '1.6',
        }}>
          <div style={{ color: 'var(--muted)', fontSize: '12px', fontFamily: 'var(--sans)' }}>
            280 px fixed width · two depth levels max
          </div>
          <div>j / k — move cursor</div>
          <div>o — expand / collapse project</div>
          <div>↵ — open thread or toggle project</div>
          <div>⌘K input — filter projects + threads</div>
          <div>↑ / ↓ glyph — toggle A→Z sort</div>
          <div>+ glyph — add project (stub)</div>
        </div>
      </div>
    </SceneShell>
  )
}

// ─── left-rail ────────────────────────────────────────────────────────────────

export function LeftRailScene() {
  return (
    <SceneShell
      id="left-rail"
      title="left rail — Claude-Code-style mode tabs + recents + pinned"
      hint="click mode tabs to swap body content"
    >
      <div style={{ display: 'flex', gap: '32px', alignItems: 'flex-start' }}>
        <LeftRail
          recents={MOCK_RECENTS}
          routines={MOCK_ROUTINES}
          onSessionNew={() => console.log('new session')}
          onRecentSelect={id => console.log('open recent', id)}
        />
        <div style={{
          display:       'flex',
          flexDirection: 'column',
          gap:           '10px',
          paddingTop:    '8px',
          fontFamily:    'var(--mono)',
          fontSize:      '11px',
          color:         'var(--faint)',
          maxWidth:      '280px',
          lineHeight:    '1.6',
        }}>
          <div style={{ color: 'var(--muted)', fontSize: '12px', fontFamily: 'var(--sans)' }}>
            240 px fixed width · three mode tabs
          </div>
          <div>chat / todo / code tabs at top</div>
          <div>active tab: --s2 bg + 2-px --accent underline</div>
          <div>+ New session · Routines · Customize · More ▾</div>
          <div>Pinned H6 + drag placeholder</div>
          <div>Recents H6 + {MOCK_RECENTS.length} mock items</div>
          <div>todo / code tabs show placeholder body</div>
        </div>
      </div>
    </SceneShell>
  )
}

// ─── right-rail ───────────────────────────────────────────────────────────────

export function RightRailScene() {
  return (
    <SceneShell
      id="right-rail"
      title="right rail — FilesPanel / TurnDiffInspector wrapper"
      hint="click Files / Diff tabs to switch panels"
    >
      <div style={{ display: 'flex', gap: '32px', alignItems: 'flex-start' }}>
        <RightRail initialMode="files" />
        <div style={{
          display:       'flex',
          flexDirection: 'column',
          gap:           '10px',
          paddingTop:    '8px',
          fontFamily:    'var(--mono)',
          fontSize:      '11px',
          color:         'var(--faint)',
          maxWidth:      '260px',
          lineHeight:    '1.6',
        }}>
          <div style={{ color: 'var(--muted)', fontSize: '12px', fontFamily: 'var(--sans)' }}>
            300 px · Files OR Diff mode
          </div>
          <div>Files tab — folder + file tree, filter input</div>
          <div>? prefix — content-search mode (accent border)</div>
          <div>Diff tab — TurnDiffInspector</div>
          <div>Turn chips — active in --accent rim</div>
          <div>Hunk lines — --green-soft / --red-soft bg</div>
          <div>Collapsed marker — click to expand context</div>
        </div>
      </div>
    </SceneShell>
  )
}

// ─── terminal-drawer ──────────────────────────────────────────────────────────

export function TerminalDrawerScene() {
  return (
    <SceneShell
      id="terminal-drawer"
      title="terminal drawer — bottom dock with live prompt + timestamp"
      hint="drawer auto-open · ⌘J to toggle · tab glyphs are visual only"
    >
      <div style={{
        width:        '820px',
        border:       '0.5px solid var(--border)',
        borderRadius: '10px',
        overflow:     'hidden',
        display:      'flex',
        flexDirection: 'column',
      }}>
        {/* Simulated editor area pushing up */}
        <div style={{
          background:  'var(--s1)',
          height:      '120px',
          display:     'flex',
          alignItems:  'center',
          justifyContent: 'center',
          color:       'var(--ghost)',
          fontFamily:  'var(--mono)',
          fontSize:    '11px',
          letterSpacing: '0.06em',
        }}>
          editor area (dock-push: terminal below ↓)
        </div>

        {/* Terminal drawer docked at bottom — pushes editor up */}
        <TerminalDrawer defaultOpen={true} />
      </div>

      <div style={{
        display:    'flex',
        flexDirection: 'column',
        gap:        '8px',
        fontFamily: 'var(--mono)',
        fontSize:   '11px',
        color:      'var(--faint)',
        maxWidth:   '820px',
        width:      '820px',
        lineHeight: '1.6',
      }}>
        <div style={{ color: 'var(--muted)', fontSize: '12px', fontFamily: 'var(--sans)' }}>
          260px dock · dock-push (not float) · ⌘J toggle
        </div>
        <div>header: Terminal label + ⌘J · split · new-tab · close-all · ×</div>
        <div>body: {'>'}= 10 lines · cmd in --accent-2 · stdout in --text · info in --muted</div>
        <div>last line: user@host path ⎇ branch : status {'>'} ▮ cursor (CSS blink)</div>
        <div>right-edge timestamp — live [HH:MM:SS] ticking each second</div>
      </div>
    </SceneShell>
  )
}

// ─── document-mode ────────────────────────────────────────────────────────────

export function DocumentModeScene() {
  return (
    <SceneShell
      id="document-mode"
      title="document mode — markdown turns + tool beats + commit summary"
      hint="full mock conversation: user → tool beats → assistant doc turns → commit card"
    >
      <div style={{
        width:     '680px',
        display:   'flex',
        flexDirection: 'column',
        gap:       '4px',
      }}>
        {MOCK_CONVERSATION.map((item, i) => {
          if (item.kind === 'user') {
            return <DocumentTurn key={i} role="user" content={item.text} />
          }
          if (item.kind === 'tool-beat') {
            return <ToolBeat key={i} summary={item.summary} body={item.body} />
          }
          if (item.kind === 'doc-turn') {
            return <DocumentTurn key={i} role="assistant" content={item.content} />
          }
          if (item.kind === 'commit-summary') {
            return (
              <CommitSummaryCard
                key={i}
                from={item.from}
                to={item.to}
                additions={item.additions}
                deletions={item.deletions}
              />
            )
          }
          return null
        })}
      </div>
    </SceneShell>
  )
}

// ─── tool-beats ───────────────────────────────────────────────────────────────

export function ToolBeatsScene() {
  return (
    <SceneShell
      id="tool-beats"
      title="tool beats — collapsible single-line verb rows"
      hint="click a row to expand the inline body"
    >
      <div style={{
        width:         '560px',
        background:    'var(--s1)',
        border:        '0.5px solid var(--border)',
        borderRadius:  '10px',
        padding:       '12px 16px',
        display:       'flex',
        flexDirection: 'column',
        gap:           '2px',
      }}>
        <ToolBeat summary="Ran orchestrator run ~/.alluka/missions/demo.md --dry-run" body="phase architect · phase scenario-tour · phase left-rail\nphase right-rail · phase terminal-drawer\n5 phases · estimated $4.20" />
        <ToolBeat summary="Recalled 3 memories" body="memory: project rename from via → nanika\nmemory: alluka naming is intentional\nmemory: nen architecture" />
        <ToolBeat summary="Read a file, ran a command" body="read: CLAUDE.md\nran: orchestrator hooks preflight" />
        <ToolBeat summary="Ran 4 commands, read 2 files, created 2 files" body="ran: orchestrator dream run --since 24h\nran: npm run build\nran: grep -r SceneShell src/\nran: git status\nread: src/scenarios/Scenes.tsx\nread: src/App.tsx\ncreated: src/components/DocumentTurn.tsx\ncreated: src/mocks/conversation.ts" />
        <ToolBeat summary="Ran npm run build" body="✓ built in 612ms · 0 errors · 0 warnings" />
      </div>

      <div style={{
        width:      '560px',
        fontFamily: 'var(--mono)',
        fontSize:   '11px',
        color:      'var(--faint)',
        lineHeight: '1.7',
      }}>
        <div style={{ color: 'var(--muted)', fontFamily: 'var(--sans)', fontSize: '12px', marginBottom: '4px' }}>verb → color mapping</div>
        <div><span style={{ color: 'var(--red)' }}>Ran</span> — --red</div>
        <div><span style={{ color: 'var(--accent)' }}>Recalled</span> — --accent</div>
        <div><span style={{ color: 'var(--text)' }}>Read</span> — --text (default)</div>
        <div>trailing <span style={{ color: 'var(--muted)' }}>›</span> — click to expand body in --s1</div>
      </div>
    </SceneShell>
  )
}

// ─── commit-summary ───────────────────────────────────────────────────────────

export function CommitSummaryScene() {
  return (
    <SceneShell
      id="commit-summary"
      title="commit summary card — branch chips + diff pill + PR button"
      hint="click Create PR ▾ to open the mock popover"
    >
      <div style={{ width: '640px', display: 'flex', flexDirection: 'column', gap: '16px' }}>
        {/* Hero card from mock conversation */}
        <CommitSummaryCard
          from="main"
          to="via/20260427-4cfbf67a/target-repo-nanika-status-active-mission"
          additions={147741}
          deletions={70564}
        />

        {/* Small example */}
        <CommitSummaryCard
          from="main"
          to="feat/tool-beat-component"
          additions={312}
          deletions={18}
        />
      </div>

      <div style={{
        width:      '640px',
        fontFamily: 'var(--mono)',
        fontSize:   '11px',
        color:      'var(--faint)',
        lineHeight: '1.7',
      }}>
        <div style={{ color: 'var(--muted)', fontFamily: 'var(--sans)', fontSize: '12px', marginBottom: '4px' }}>anatomy</div>
        <div>branch chips — <span style={{ color: 'var(--muted)' }}>from</span> ← <span style={{ color: 'var(--accent)' }}>to</span> · long names truncated</div>
        <div>diff pill — <span style={{ color: 'var(--green)' }}>+additions</span> <span style={{ color: 'var(--red)' }}>−deletions</span> in JetBrains Mono</div>
        <div>Create PR ▾ — warm-accent border · opens inline popover</div>
      </div>
    </SceneShell>
  )
}

// ─── top-action-bar ───────────────────────────────────────────────────────────

export function TopActionBarScene() {
  return (
    <SceneShell
      id="top-action-bar"
      title="top action bar — breadcrumb + commit + open popovers"
      hint="click Open ▾ or Commit & push ▾ to open mock popovers"
    >
      <div style={{
        width:        '820px',
        border:       '0.5px solid var(--border)',
        borderRadius: '10px',
        overflow:     'hidden',
      }}>
        <TopActionBar
          breadcrumbs={[
            { label: 'Identify current context' },
            { label: 'nanika' },
          ]}
        />
        {/* Simulated content area */}
        <div style={{
          background:     'var(--s0)',
          height:         '160px',
          display:        'flex',
          alignItems:     'center',
          justifyContent: 'center',
          color:          'var(--ghost)',
          fontFamily:     'var(--mono)',
          fontSize:       '11px',
          letterSpacing:  '0.06em',
        }}>
          content area
        </div>
      </div>

      <div style={{
        display:    'flex',
        flexDirection: 'column',
        gap:        '8px',
        fontFamily: 'var(--mono)',
        fontSize:   '11px',
        color:      'var(--faint)',
        maxWidth:   '820px',
        width:      '820px',
        lineHeight: '1.6',
      }}>
        <div style={{ color: 'var(--muted)', fontSize: '12px', fontFamily: 'var(--sans)' }}>
          breadcrumb chips left · actions right
        </div>
        <div>breadcrumb: chips › separated · last chip in --s3 bg</div>
        <div>+ Add action — terracotta (--accent) filled button</div>
        <div>Open ▾ / Commit {'&'} push ▾ — chips with popover on click</div>
        <div>⤢ expand · + new-tab icon buttons</div>
      </div>
    </SceneShell>
  )
}

// ─── composer-chips ───────────────────────────────────────────────────────────

export function ComposerChipsScene() {
  return (
    <SceneShell
      id="composer-chips"
      title="composer chips — model · reasoning · mode · permissions · tokens"
      hint="click first four chips to open mock popovers"
    >
      <div style={{
        width:        '640px',
        border:       '0.5px solid var(--border)',
        borderRadius: '10px',
        overflow:     'hidden',
      }}>
        {/* Simulated composer input area */}
        <div style={{
          background:  'var(--s0)',
          height:      '80px',
          padding:     '14px 16px',
          fontFamily:  'var(--mono)',
          fontSize:    '13px',
          color:       'var(--faint)',
        }}>
          composer input area
        </div>
        <ComposerChips />
      </div>

      <div style={{
        width:      '640px',
        fontFamily: 'var(--mono)',
        fontSize:   '11px',
        color:      'var(--faint)',
        lineHeight: '1.7',
      }}>
        <div style={{ color: 'var(--muted)', fontSize: '12px', fontFamily: 'var(--sans)', marginBottom: '4px' }}>chip anatomy</div>
        <div><span style={{ color: 'var(--muted)' }}>◐ Claude Opus 4.6 ▾</span> — model selector · caret</div>
        <div><span style={{ color: 'var(--muted)' }}>High · Normal · 200k ▾</span> — reasoning tier · window · mode</div>
        <div><span style={{ color: 'var(--muted)' }}>Build ▾</span> — mode selector</div>
        <div><span style={{ color: 'var(--muted)' }}>🔒 Full access ▾</span> — permissions gate</div>
        <div><span style={{ color: 'var(--faint)' }}>26</span> — token counter · plain muted · no caret · no popover</div>
      </div>
    </SceneShell>
  )
}

// ─── composite-canvas ─────────────────────────────────────────────────────────

export function CompositeCanvasScene() {
  const [termOpen, setTermOpen] = useState(false)
  const [railOpen, setRailOpen] = useState(false)

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.metaKey && e.key === 'j') {
        e.preventDefault()
        setTermOpen(v => !v)
      }
      if (e.metaKey && e.key === 'b') {
        e.preventDefault()
        setRailOpen(v => !v)
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [])

  const state: CompositeCanvasState = {
    ...stateDefault,
    terminalOpen:  termOpen,
    rightRailOpen: railOpen,
  }

  return <CompositeCanvas state={state} onCloseTerminal={() => setTermOpen(false)} />
}

// ─── composite-canvas-all-open ────────────────────────────────────────────────

export function CompositeCanvasAllOpenScene() {
  const [termOpen, setTermOpen] = useState(true)
  const [railOpen, setRailOpen] = useState(true)

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.metaKey && e.key === 'j') { e.preventDefault(); setTermOpen(v => !v) }
      if (e.metaKey && e.key === 'b') { e.preventDefault(); setRailOpen(v => !v) }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [])

  const state: CompositeCanvasState = {
    ...stateAllOpen,
    terminalOpen:  termOpen,
    rightRailOpen: railOpen,
  }
  return <CompositeCanvas state={state} onCloseTerminal={() => setTermOpen(false)} />
}

// ─── composite-canvas-leftrail ────────────────────────────────────────────────

export function CompositeCanvasLeftrailScene() {
  return <CompositeCanvas state={stateLeftrail} />
}

// ─── composite-canvas-empty ───────────────────────────────────────────────────

export function CompositeCanvasEmptyScene() {
  return <CompositeCanvas state={stateEmpty} />
}

// ─── composite-canvas-streaming ───────────────────────────────────────────────

const STREAMING_TEXT =
  'The CompositeCanvas substrate is a pure function of CompositeCanvasState. ' +
  'No per-variant branches live inside the substrate — only conditional slot mounts ' +
  'driven by nullable and boolean fields on the state object. ' +
  'The same one-substrate discipline that worked for PaletteShell in slice 1 ' +
  'applied one Esc-ladder rung deeper.'

export function CompositeCanvasStreamingScene() {
  const [text, setText]         = useState('')
  const [workingMs, setWorking] = useState(0)
  const [stopped, setStopped]   = useState(false)
  const charRef                 = useRef(0)

  useEffect(() => {
    if (stopped) return
    const id = setInterval(() => {
      charRef.current++
      setText(STREAMING_TEXT.slice(0, charRef.current))
      if (charRef.current >= STREAMING_TEXT.length) clearInterval(id)
    }, 30)
    return () => clearInterval(id)
  }, [stopped])

  useEffect(() => {
    if (stopped) return
    const id = setInterval(() => setWorking(ms => ms + 1000), 1000)
    return () => clearInterval(id)
  }, [stopped])

  const state: CompositeCanvasState = {
    ...stateStreaming,
    streamingTurn: stopped ? null : text,
    workingForMs:  stopped ? null : workingMs,
  }
  return <CompositeCanvas state={state} onStop={() => setStopped(true)} />
}

// ─── composite-canvas-mission ─────────────────────────────────────────────────

export function CompositeCanvasMissionScene() {
  const [elapsedMs, setElapsed] = useState(0)

  useEffect(() => {
    const id = setInterval(() => setElapsed(ms => ms + 1000), 1000)
    return () => clearInterval(id)
  }, [])

  const state: CompositeCanvasState = {
    ...stateMissionRunning,
    missionRun: stateMissionRunning.missionRun
      ? { ...stateMissionRunning.missionRun, elapsedMs }
      : null,
  }
  return <CompositeCanvas state={state} />
}

// ─── composer-footer ──────────────────────────────────────────────────────────

export function ComposerFooterScene() {
  return (
    <SceneShell
      id="composer-footer"
      title="composer footer — bypass · attach · mic · model+reasoning right"
      hint="click the right-side chip to open mock model popover"
    >
      <div style={{
        width:        '640px',
        border:       '0.5px solid var(--border)',
        borderRadius: '10px',
        overflow:     'hidden',
      }}>
        {/* Simulated composer input area */}
        <div style={{
          background:  'var(--s0)',
          height:      '80px',
          padding:     '14px 16px',
          fontFamily:  'var(--mono)',
          fontSize:    '13px',
          color:       'var(--faint)',
        }}>
          composer input area
        </div>
        <ComposerChips />
        <ComposerFooter />
      </div>

      <div style={{
        width:      '640px',
        fontFamily: 'var(--mono)',
        fontSize:   '11px',
        color:      'var(--faint)',
        lineHeight: '1.7',
      }}>
        <div style={{ color: 'var(--muted)', fontSize: '12px', fontFamily: 'var(--sans)', marginBottom: '4px' }}>footer anatomy</div>
        <div><span style={{ color: 'var(--accent)' }}>Bypass permissions</span> — warm terracotta --accent · no border</div>
        <div><span style={{ color: 'var(--muted)' }}>+</span> — attach glyph · icon button</div>
        <div><span style={{ color: 'var(--muted)' }}>mic</span> — voice input glyph · icon button</div>
        <div><span style={{ color: 'var(--faint)' }}>◐ Opus 4.7 1M · Extra high</span> — smaller model+reasoning chip · right-pinned · click opens popover</div>
      </div>
    </SceneShell>
  )
}

// ─── files-panel ──────────────────────────────────────────────────────────────

export function FilesPanelScene() {
  return (
    <SceneShell
      id="files-panel"
      title="files panel — folder + file tree with filter and content-search"
      hint="type to filter file names · prefix with `?` for content-search mode (accent border)"
    >
      <div style={{ display: 'flex', gap: '32px', alignItems: 'flex-start' }}>
        <FilesPanel
          files={MOCK_FILE_TREE}
          onSelect={path => console.log('open file', path)}
        />
        <div style={{
          display:       'flex',
          flexDirection: 'column',
          gap:           '10px',
          paddingTop:    '8px',
          fontFamily:    'var(--mono)',
          fontSize:      '11px',
          color:         'var(--faint)',
          maxWidth:      '260px',
          lineHeight:    '1.6',
        }}>
          <div style={{ color: 'var(--muted)', fontSize: '12px', fontFamily: 'var(--sans)' }}>
            300 px · folder + file tree
          </div>
          <div>Header — Files title + view toggle + ×</div>
          <div>Filter input — type to narrow names</div>
          <div>?prefix — content-search mode (accent border)</div>
          <div>Folders — accent-2 chevron · click to expand</div>
          <div>Files — extension badge in type color</div>
        </div>
      </div>
    </SceneShell>
  )
}

// ─── turn-diff-inspector ──────────────────────────────────────────────────────

export function TurnDiffInspectorScene() {
  return (
    <SceneShell
      id="turn-diff-inspector"
      title="turn diff inspector — per-turn diff with turn chips and hunk lines"
      hint="click turn chips to switch · view + layout glyphs at right · click marker to expand context"
    >
      <div style={{ display: 'flex', gap: '32px', alignItems: 'flex-start' }}>
        <TurnDiffInspector />
        <div style={{
          display:       'flex',
          flexDirection: 'column',
          gap:           '10px',
          paddingTop:    '8px',
          fontFamily:    'var(--mono)',
          fontSize:      '11px',
          color:         'var(--faint)',
          maxWidth:      '260px',
          lineHeight:    '1.6',
        }}>
          <div style={{ color: 'var(--muted)', fontSize: '12px', fontFamily: 'var(--sans)' }}>
            300 px · per-turn diff
          </div>
          <div>Header — ‹ All turns chip-strip › view + layout</div>
          <div>Active chip — accent rim outline</div>
          <div>File path — --accent mono</div>
          <div>±N chip — green-+ red-− in --s3 pill</div>
          <div>Hunk lines — --green-soft / --red-soft</div>
          <div>Collapsed marker — click to expand context</div>
        </div>
      </div>
    </SceneShell>
  )
}

// ─── file-viewer ──────────────────────────────────────────────────────────────

export function FileViewerScene() {
  return (
    <SceneShell
      id="file-viewer"
      title="file viewer — read-only file display with `/` search and n/N nav"
      hint="press / to open search · n next match · N previous · Esc closes"
    >
      <div style={{ width: '760px', display: 'flex', flexDirection: 'column', gap: '14px' }}>
        <FileViewer
          filename={MOCK_FILE_VIEWER.filename}
          language={MOCK_FILE_VIEWER.language}
          lines={MOCK_FILE_VIEWER.lines}
        />
        <div style={{
          fontFamily: 'var(--mono)',
          fontSize:   '11px',
          color:      'var(--faint)',
          lineHeight: '1.7',
        }}>
          <div style={{ color: 'var(--muted)', fontSize: '12px', fontFamily: 'var(--sans)', marginBottom: '4px' }}>
            760 px max · JetBrains Mono · gutter line numbers · 4-token highlight
          </div>
          <div>keyword in --accent · string in --green · number in --blue · comment in --muted</div>
          <div>search bar shows / n N Esc keycaps · match count ticks live</div>
        </div>
      </div>
    </SceneShell>
  )
}

// ─── composite-canvas-review-gate ────────────────────────────────────────────

export function CompositeCanvasReviewGateScene() {
  return <CompositeCanvas state={stateReviewGate} />
}

// ─── composite-canvas-error ───────────────────────────────────────────────────

export function CompositeCanvasErrorScene() {
  return <CompositeCanvas state={stateError} />
}

// ─── composite-canvas-voice ───────────────────────────────────────────────────

export function CompositeCanvasVoiceScene() {
  const [composerState, setComposerState] = useState<'recording-voice' | 'idle'>('recording-voice')

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setComposerState('idle')
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  const state: CompositeCanvasState = {
    ...stateVoice,
    composerState,
  }
  return <CompositeCanvas state={state} />
}

// ─── composite-canvas-files-and-viewer ───────────────────────────────────────

export function CompositeCanvasFilesAndViewerScene() {
  const [selectedFile, setSelectedFile] = useState<{ path: string } | null>(
    stateFilesAndViewer.selectedFile,
  )

  const state: CompositeCanvasState = {
    ...stateFilesAndViewer,
    selectedFile,
  }
  return (
    <CompositeCanvas
      state={state}
      onSelectFile={path => setSelectedFile({ path })}
    />
  )
}

// ─── composite-canvas-plugin-inline ──────────────────────────────────────────

export function CompositeCanvasPluginInlineScene() {
  return <CompositeCanvas state={statePluginInline} />
}

// ─── composite-canvas-notifications ──────────────────────────────────────────

export function CompositeCanvasNotificationsScene() {
  return <CompositeCanvas state={stateNotifications} />
}

// ─── action-palette ───────────────────────────────────────────────────────────

const MOCK_LIST_ITEMS = [
  { id: 'trk-573', title: 'TRK-573 · CodeDiff apply_hunk algorithm', subtitle: 'P0 · in_progress' },
  { id: 'trk-558', title: 'TRK-558 · Nanika → T3 Code parity',       subtitle: 'P1 · open' },
  { id: 'trk-575', title: 'TRK-575 · Tool-use: switch to --mcp-config', subtitle: 'P1 · open' },
]

export function ActionPaletteScene() {
  const [open, setOpen]         = useState(true)
  const [lastAction, setLastAction] = useState<string | null>(null)
  const [activeItem, setActiveItem] = useState(0)

  const handleAction = (actionId: string) => {
    setLastAction(actionId)
    setOpen(false)
  }

  return (
    <SceneShell
      id="action-palette"
      title="Action Palette"
      hint="j/k navigate verbs · Enter confirm · Esc dismiss"
    >
      <div style={{ width: '820px', display: 'flex', flexDirection: 'column', gap: '2px' }}>
        {/* Mock list context */}
        {MOCK_LIST_ITEMS.map((item, i) => (
          <div
            key={item.id}
            onClick={() => { setActiveItem(i); setOpen(true) }}
            style={{
              display:      'flex',
              alignItems:   'center',
              gap:          '12px',
              padding:      '11px 16px',
              borderRadius: '8px',
              background:   activeItem === i ? 'var(--s2)' : 'var(--s1)',
              border:       `0.5px solid ${activeItem === i ? 'var(--accent-rim)' : 'var(--border-soft)'}`,
              cursor:       'pointer',
              transition:   'background 0.08s',
            }}
          >
            <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--accent)', minWidth: '80px' }}>
              {item.id.toUpperCase()}
            </span>
            <span style={{ fontFamily: 'var(--sans)', fontSize: '13px', color: 'var(--text)', flex: 1 }}>
              {item.title}
            </span>
            <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--muted)' }}>
              {item.subtitle}
            </span>
            <span className="K nav" style={{ fontSize: '10px' }}>↵</span>
          </div>
        ))}

        {/* Feedback */}
        {lastAction && !open && (
          <div style={{
            marginTop:     '16px',
            padding:       '10px 16px',
            borderRadius:  '8px',
            background:    'var(--accent-soft)',
            border:        '0.5px solid var(--accent-rim)',
            fontFamily:    'var(--mono)',
            fontSize:      '11px',
            color:         'var(--accent-2)',
            display:       'flex',
            alignItems:    'center',
            gap:           '8px',
          }}>
            <span>✓</span>
            <span>Action fired: <strong>{lastAction}</strong></span>
            <button
              onClick={() => setOpen(true)}
              style={{
                marginLeft:  'auto',
                fontFamily:  'var(--mono)',
                fontSize:    '10px',
                color:       'var(--accent)',
                background:  'none',
                border:      'none',
                cursor:      'pointer',
              }}
            >
              reopen ↵
            </button>
          </div>
        )}

        {!open && !lastAction && (
          <div style={{
            marginTop:  '16px',
            fontFamily: 'var(--mono)',
            fontSize:   '11px',
            color:      'var(--faint)',
            textAlign:  'center',
          }}>
            Click a list item to open the palette
          </div>
        )}
      </div>

      {/* Palette overlay (fixed, covers entire viewport) */}
      <ActionPalette
        open={open}
        item={MOCK_ACTION_ITEM}
        actions={MOCK_ACTION_PALETTE_ACTIONS}
        onAction={handleAction}
        onDismiss={() => setOpen(false)}
      />
    </SceneShell>
  )
}

// ─── fan-view ─────────────────────────────────────────────────────────────────

const MOCK_FAN_THREADS: FanThread[] = [
  { id: 'th-1', title: 'Canvas states batch 2',    preview: 'Implementing F–K canvas variants' },
  { id: 'th-2', title: 'Slice 3 polish',           preview: 'Closing warnings from review' },
  { id: 'th-3', title: 'Refactor canvas substrate', preview: 'Extract CompositeCanvas component' },
  { id: 'th-4', title: 'Architect phase',           preview: 'UX decisions §2.4, §3.4–§3.13' },
  { id: 'th-5', title: 'Nanika → T3 parity',       preview: 'TRK-558 code parity work' },
  { id: 'th-6', title: 'Tool-use MCP switch',      preview: 'TRK-575 --mcp-config migration' },
]

export function FanViewScene() {
  const anchorRef                  = useRef<HTMLButtonElement>(null)
  const [anchorRect, setAnchorRect] = useState<DOMRect | null>(null)
  const [open, setOpen]             = useState(true)
  const [selected, setSelected]     = useState<FanThread | null>(null)

  useEffect(() => {
    const update = () => {
      if (anchorRef.current) setAnchorRect(anchorRef.current.getBoundingClientRect())
    }
    update()
    window.addEventListener('resize', update)
    return () => window.removeEventListener('resize', update)
  }, [])

  const handleSelect = (thread: FanThread) => {
    setSelected(thread)
    setOpen(false)
  }

  return (
    <SceneShell
      id="fan-view"
      title="Fan View — thread switcher"
      hint="click anchor to open · j/k navigate chips · Enter/click select · Esc dismiss"
    >
      <div style={{
        width:          '820px',
        display:        'flex',
        flexDirection:  'column',
        alignItems:     'center',
        gap:            '24px',
        paddingTop:     '80px',
        paddingBottom:  '120px',
      }}>
        {/* Selected thread feedback */}
        {selected && (
          <div style={{
            padding:       '10px 20px',
            borderRadius:  '8px',
            background:    'var(--accent-soft)',
            border:        '0.5px solid var(--accent-rim)',
            fontFamily:    'var(--mono)',
            fontSize:      '11px',
            color:         'var(--accent-2)',
            display:       'flex',
            alignItems:    'center',
            gap:           '8px',
          }}>
            <span>✓ Switched to:</span>
            <strong>{selected.title}</strong>
          </div>
        )}

        {/* Anchor pill — fan opens from here */}
        <button
          ref={anchorRef}
          onClick={() => { setSelected(null); setOpen(true) }}
          style={{
            display:       'flex',
            alignItems:    'center',
            gap:           '8px',
            padding:       '8px 20px',
            borderRadius:  '999px',
            background:    open ? 'var(--s3)' : 'var(--s2)',
            border:        `0.5px solid ${open ? 'var(--accent-rim)' : 'var(--border)'}`,
            cursor:        'pointer',
            fontFamily:    'var(--sans)',
            fontSize:      '13px',
            color:         open ? 'var(--text)' : 'var(--muted)',
            transition:    'background 0.12s, border-color 0.12s, color 0.12s',
          }}
        >
          <Icon name="Branch" size={14} />
          {selected ? selected.title : 'Switch thread'}
          <span style={{ fontFamily: 'var(--mono)', fontSize: '10px', color: 'var(--faint)' }}>▲</span>
        </button>

        <div style={{
          fontFamily:    'var(--mono)',
          fontSize:      '10px',
          color:         'var(--ghost)',
          letterSpacing: '0.06em',
        }}>
          {MOCK_FAN_THREADS.length} threads · max 7 visible in 60° arc
        </div>
      </div>

      <FanView
        open={open}
        threads={MOCK_FAN_THREADS}
        anchorRect={anchorRect}
        onSelect={handleSelect}
        onDismiss={() => setOpen(false)}
      />
    </SceneShell>
  )
}

// ─── pill-drag-snap ───────────────────────────────────────────────────────────

const SNAP_PERCENTS  = [40, 60]
const DRAG_THRESHOLD = 5   // px before overlay appears
const SNAP_THRESHOLD = 40  // px to snap to a guide

export function PillDragSnapScene() {
  const [posX, setPosX]           = useState(20) // horizontal % in container
  const [isDragging, setIsDragging] = useState(false)
  const containerRef              = useRef<HTMLDivElement>(null)
  const draggingRef               = useRef(false)
  const startRef                  = useRef({ startX: 0, posX: 20, originalX: 20 })
  const dragDistRef               = useRef(0)

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!draggingRef.current || !containerRef.current) return
      const rect = containerRef.current.getBoundingClientRect()
      const dx   = e.clientX - startRef.current.startX
      dragDistRef.current = Math.abs(dx)

      if (dragDistRef.current > DRAG_THRESHOLD) setIsDragging(true)

      const newX = Math.max(5, Math.min(92, startRef.current.posX + (dx / rect.width) * 100))
      setPosX(newX)
    }

    const onUp = () => {
      if (!draggingRef.current) return
      draggingRef.current   = false
      dragDistRef.current   = 0
      setIsDragging(false)
      document.body.style.cursor = ''

      if (!containerRef.current) return
      const rect = containerRef.current.getBoundingClientRect()

      setPosX(x => {
        const xPx = (x / 100) * rect.width
        for (const snap of SNAP_PERCENTS) {
          const snapPx = (snap / 100) * rect.width
          if (Math.abs(xPx - snapPx) < SNAP_THRESHOLD) return snap
        }
        return x
      })
    }

    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && draggingRef.current) {
        draggingRef.current   = false
        dragDistRef.current   = 0
        setIsDragging(false)
        document.body.style.cursor = ''
        setPosX(startRef.current.originalX)
      }
    }

    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup',   onUp)
    window.addEventListener('keydown',   onKey)
    return () => {
      window.removeEventListener('mousemove', onMove)
      window.removeEventListener('mouseup',   onUp)
      window.removeEventListener('keydown',   onKey)
    }
  }, [])

  return (
    <SceneShell
      id="pill-drag-snap"
      title="Pill Drag Snap"
      hint="drag pill horizontally · snap guides at 40% and 60% · Esc mid-drag cancels"
    >
      <div
        ref={containerRef}
        style={{
          position:     'relative',
          width:        '820px',
          height:       '480px',
          background:   'var(--s0)',
          borderRadius: '12px',
          border:       '0.5px solid var(--border)',
          overflow:     'hidden',
          userSelect:   'none',
        }}
      >
        {/* Snap guides (visible only while dragging > 5px) */}
        <PillDragOverlay active={isDragging} anchorPercents={SNAP_PERCENTS} />

        {/* Guide labels (always shown faintly) */}
        {SNAP_PERCENTS.map(pct => (
          <div
            key={pct}
            style={{
              position:      'absolute',
              left:          `${pct}%`,
              top:           0,
              bottom:        0,
              width:         '1px',
              background:    'var(--ghost)',
              opacity:       isDragging ? 0 : 0.35,
              pointerEvents: 'none',
              transition:    'opacity 0.15s ease',
            }}
          >
            <span style={{
              position:      'absolute',
              top:           '10px',
              left:          '6px',
              fontFamily:    'var(--mono)',
              fontSize:      '9px',
              color:         'var(--faint)',
              letterSpacing: '0.1em',
            }}>
              {pct}%
            </span>
          </div>
        ))}

        {/* Draggable pill */}
        <div
          data-pill-drag
          style={{
            position:  'absolute',
            left:      `${posX}%`,
            top:       '50%',
            transform: 'translate(-50%, -50%)',
            cursor:    isDragging ? 'grabbing' : 'grab',
            zIndex:    2,
          }}
          onMouseDown={e => {
            draggingRef.current = true
            startRef.current    = { startX: e.clientX, posX, originalX: posX }
            dragDistRef.current = 0
            document.body.style.cursor = 'grabbing'
            e.preventDefault()
          }}
        >
          <PaletteShell scale="pill" pillMode="idle" />
        </div>

        {/* Drag hint */}
        {!isDragging && (
          <div style={{
            position:   'absolute',
            bottom:     '16px',
            left:       0,
            right:      0,
            textAlign:  'center',
            fontFamily: 'var(--mono)',
            fontSize:   '10px',
            color:      'var(--ghost)',
            pointerEvents: 'none',
          }}>
            drag to reposition · snaps to 40% and 60% · Esc cancels mid-drag
          </div>
        )}
      </div>
    </SceneShell>
  )
}

// ─── S5.O · Transition L0 → L3 ────────────────────────────────────────────────

export function TransitionL0ToL3Scene() {
  return (
    <SceneShell
      id="transition-l0-to-l3"
      title="Transition L0 → L3"
      hint="space advance · esc reset · single PaletteShell substrate, scale prop morphs ≤ 220 ms"
    >
      <TransitionDemo />
    </SceneShell>
  )
}

// ─── Live Composite Canvas ────────────────────────────────────────────────────

export function LiveCompositeCanvasScene() {
  const params = new URLSearchParams(window.location.search)
  const threadId = params.get('thread') ?? undefined
  return <LiveCompositeCanvas threadId={threadId} />
}

// ─── Live Files Panel ─────────────────────────────────────────────────────────

export function LiveFilesPanelScene() {
  return <LiveFilesPanel />
}

// ─── Live Diff Panel ──────────────────────────────────────────────────────────

export function LiveDiffPanelScene() {
  return (
    <SceneShell
      id="live-diff-panel"
      title="live diff panel — real fs via Tauri diff commands"
      hint="Click a file tab to load hunks · Accept / Reject per hunk"
    >
      <LiveDiffPanel />
    </SceneShell>
  )
}

// ─── Live Turn Diff Inspector ─────────────────────────────────────────────────

export function LiveTurnDiffInspectorScene() {
  return (
    <SceneShell
      id="live-turn-diff-inspector"
      title="live turn diff inspector — real fs via Tauri diff commands"
      hint="Select a file chip to load hunks · Accept all / Reject all per file"
    >
      <LiveTurnDiffInspector />
    </SceneShell>
  )
}

// ─── S6 · Real-usage progressive demo ────────────────────────────────────────

export { RealUsageDemo as RealUsageDemoScene } from '../components/demo/RealUsageDemo'

// ─── Guided Tour scene ────────────────────────────────────────────────────────

export function TourScene() {
  const [activeScene, setActiveScene] = useState<TourSceneSpec>(
    TOUR_STEPS[0]?.scene ?? { kind: 'pill', state: {} }
  )

  return (
    <div style={{ position: 'fixed', inset: 0, background: 'var(--s0)' }}>
      <TourSceneRenderer scene={activeScene} />
      <Tour
        steps={TOUR_STEPS}
        onScene={setActiveScene}
        onExit={() => { window.location.hash = '#/scenarios' }}
      />
    </div>
  )
}

// ─── live-terminal-drawer ─────────────────────────────────────────────────────

export function LiveTerminalDrawerScene() {
  return <LiveTerminalDrawer />
}

// ─── live-mission-run-canvas ──────────────────────────────────────────────────

export function LiveMissionRunCanvasScene() {
  return <LiveMissionRunCanvas />
}

// ─── live-left-rail ───────────────────────────────────────────────────────────

export function LiveLeftRailScene() {
  return (
    <SceneShell
      id="live-left-rail"
      title="live left rail — projects · routines · pinned · live threads"
      hint="Wired to list_projects + list_routines + read_pins + useChat threads · whim://routines-changed"
    >
      <LiveLeftRail />
    </SceneShell>
  )
}

// ─── live-top-action-bar ──────────────────────────────────────────────────────

export function LiveTopActionBarScene() {
  return (
    <SceneShell
      id="live-top-action-bar"
      title="live top action bar — live git status breadcrumb"
      hint="Wired to get_repo_status · auto-refresh on whim://fs-changed (debounced 500 ms)"
    >
      <LiveTopActionBar />
    </SceneShell>
  )
}

// ─── live-commit-summary-card ─────────────────────────────────────────────────

export function LiveCommitSummaryCardScene() {
  return (
    <SceneShell
      id="live-commit-summary-card"
      title="live commit summary card — HEAD commit + PR metadata"
      hint="Wired to get_commit_summary + get_pr_metadata · createPr re-fetches PR after success"
    >
      <LiveCommitSummaryCard />
    </SceneShell>
  )
}

// ─── live-notifications-canvas ────────────────────────────────────────────────

export function LiveNotificationsCanvasScene() {
  return <LiveNotificationsCanvas />
}
