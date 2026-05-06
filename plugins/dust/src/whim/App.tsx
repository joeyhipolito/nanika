import { useState, useEffect, useRef, useCallback, type FC } from 'react'
import {
  WhatIsWhimScene,
  IdleScene,
  HoverScene,
  TypeScene,
  VoiceListeningScene,
  VoiceRecordingScene,
  TranscriptPreviewScene,
  MissionSpawnScene,
  MissionProgressScene,
  MultiMissionScene,
  ReviewGateScene,
  DiffViewerScene,
  PluginEmbedsScene,
  NotificationScene,
  ErrorRetryScene,
  PluginShellScene,
  ProjectsTreeScene,
  LeftRailScene,
  RightRailScene,
  FilesPanelScene,
  TurnDiffInspectorScene,
  FileViewerScene,
  TerminalDrawerScene,
  TopActionBarScene,
  DocumentModeScene,
  ToolBeatsScene,
  CommitSummaryScene,
  ComposerChipsScene,
  ComposerFooterScene,
  CompositeCanvasScene,
  CompositeCanvasAllOpenScene,
  CompositeCanvasLeftrailScene,
  CompositeCanvasEmptyScene,
  CompositeCanvasStreamingScene,
  CompositeCanvasMissionScene,
  CompositeCanvasReviewGateScene,
  CompositeCanvasErrorScene,
  CompositeCanvasVoiceScene,
  CompositeCanvasFilesAndViewerScene,
  CompositeCanvasPluginInlineScene,
  CompositeCanvasNotificationsScene,
  ActionPaletteScene,
  FanViewScene,
  PillDragSnapScene,
  TransitionL0ToL3Scene,
  TourScene,
  RealUsageDemoScene,
  LiveCompositeCanvasScene,
  LiveFilesPanelScene,
  LiveDiffPanelScene,
  LiveTurnDiffInspectorScene,
  LiveTerminalDrawerScene,
  LiveMissionRunCanvasScene,
  LiveLeftRailScene,
  LiveTopActionBarScene,
  LiveCommitSummaryCardScene,
  LiveNotificationsCanvasScene,
} from './scenarios/Scenes'

// ─── Scenario registry ────────────────────────────────────────────────────────

const SCENE_COMPONENTS: Record<string, FC> = {
  '00-what-is-whim':        WhatIsWhimScene,
  '3.1-idle':               IdleScene,
  '3.2-hover':              HoverScene,
  '3.3-type':               TypeScene,
  '3.4-voice-listening':    VoiceListeningScene,
  '3.5-voice-recording':    VoiceRecordingScene,
  '3.6-transcript-preview': TranscriptPreviewScene,
  '3.7-mission-spawn':      MissionSpawnScene,
  '3.8-mission-progress':   MissionProgressScene,
  '3.9-review-gate':        ReviewGateScene,
  '3.9b-diff-viewer':       DiffViewerScene,
  '3.10-plugin-embeds':     PluginEmbedsScene,
  '3.11-notification':      NotificationScene,
  '3.12-error-retry':       ErrorRetryScene,
  '3.13-multi-mission':     MultiMissionScene,
  '99-plugin-shell':        PluginShellScene,
  'projects-tree':          ProjectsTreeScene,
  'left-rail':              LeftRailScene,
  'right-rail':             RightRailScene,
  'files-panel':            FilesPanelScene,
  'turn-diff-inspector':    TurnDiffInspectorScene,
  'file-viewer':            FileViewerScene,
  'terminal-drawer':        TerminalDrawerScene,
  'top-action-bar':         TopActionBarScene,
  'document-mode':          DocumentModeScene,
  'tool-beats':             ToolBeatsScene,
  'commit-summary':         CommitSummaryScene,
  'composer-chips':         ComposerChipsScene,
  'composer-footer':        ComposerFooterScene,
  'composite-canvas':            CompositeCanvasScene,
  'composite-canvas-all-open':          CompositeCanvasAllOpenScene,
  'composite-canvas-leftrail':          CompositeCanvasLeftrailScene,
  'composite-canvas-empty':             CompositeCanvasEmptyScene,
  'composite-canvas-streaming':         CompositeCanvasStreamingScene,
  'composite-canvas-mission':           CompositeCanvasMissionScene,
  'composite-canvas-review-gate':       CompositeCanvasReviewGateScene,
  'composite-canvas-error':             CompositeCanvasErrorScene,
  'composite-canvas-voice':             CompositeCanvasVoiceScene,
  'composite-canvas-files-and-viewer':  CompositeCanvasFilesAndViewerScene,
  'composite-canvas-plugin-inline':     CompositeCanvasPluginInlineScene,
  'composite-canvas-notifications':     CompositeCanvasNotificationsScene,
  'action-palette':                     ActionPaletteScene,
  'fan-view':                           FanViewScene,
  'pill-drag-snap':                     PillDragSnapScene,
  'transition-l0-to-l3':                TransitionL0ToL3Scene,
  'tour':                               TourScene,
  'real-usage-demo':                    RealUsageDemoScene,
  'live-composite-canvas':              LiveCompositeCanvasScene,
  'live-files-panel':                   LiveFilesPanelScene,
  'live-diff-panel':                    LiveDiffPanelScene,
  'live-turn-diff-inspector':           LiveTurnDiffInspectorScene,
  'live-terminal-drawer':               LiveTerminalDrawerScene,
  'live-mission-run-canvas':            LiveMissionRunCanvasScene,
  'live-left-rail':                     LiveLeftRailScene,
  'live-top-action-bar':                LiveTopActionBarScene,
  'live-commit-summary-card':           LiveCommitSummaryCardScene,
  'live-notifications-canvas':          LiveNotificationsCanvasScene,
}

export const SCENARIO_LIST: { id: string; title: string; ref: string; desc: string }[] = [
  { id: '00-what-is-whim',        ref: '§00',   title: 'What is Whim?',          desc: 'Pill → Palette → Canvas explainer diagram' },
  { id: '3.1-idle',               ref: '§3.1',  title: 'Idle',                   desc: 'Edge-docked pill with drag-to-reposition + snap' },
  { id: '3.2-hover',              ref: '§3.2',  title: 'Hover',                  desc: 'Read-only input + mic + conversation count badge' },
  { id: '3.3-type',               ref: '§3.3',  title: 'Type',                   desc: 'Full palette with grouped results, no mode pills' },
  { id: '3.4-voice-listening',    ref: '§3.4',  title: 'Voice Listening',        desc: '5-bar waveform + scrolling transcript line' },
  { id: '3.5-voice-recording',    ref: '§3.5',  title: 'Voice Recording',        desc: '9-bin VoiceOverlay in composer, ⌥Space push-to-talk' },
  { id: '3.6-transcript-preview', ref: '§3.6',  title: 'Transcript Preview',     desc: 'Transcript pre-filled in composer for edit before Enter' },
  { id: '3.7-mission-spawn',      ref: '§3.7',  title: 'Mission Spawn',          desc: 'Type and ↵ to spawn a mock mission → switches to canvas' },
  { id: '3.8-mission-progress',   ref: '§3.8',  title: 'Mission Progress',       desc: 'Mission canvas L3 with stacked run log + live duration' },
  { id: '3.9-review-gate',        ref: '§3.9',  title: 'Review Gate',            desc: 'Changed-Files card + per-file summary + ⌘D to diff viewer' },
  { id: '3.9b-diff-viewer',       ref: '§3.9b', title: 'Diff Viewer',            desc: 'HERO — 4 files · 11 hunks · full hunk-FSM + 12-key handler' },
  { id: '3.10-plugin-embeds',     ref: '§3.10', title: 'Plugin Embeds',          desc: 'Four embed points: palette row · detail pane · sidebar · inline chat' },
  { id: '3.11-notification',      ref: '§3.11', title: 'Notification',           desc: 'Three tiers: ambient HUD · footer badge strip · terracotta banner' },
  { id: '3.12-error-retry',       ref: '§3.12', title: 'Error / Retry',          desc: 'Three flavors: composer validation · runtime failure · diff apply' },
  { id: '3.13-multi-mission',     ref: '§3.13', title: 'Multi-mission',          desc: 'Palette directory grouped Active · Recent · Scheduled' },
  { id: '99-plugin-shell',        ref: '§99',   title: 'Plugin Shell',           desc: 'Closer — embed table · verb→prefix map · host keymap' },
  { id: 'projects-tree',          ref: '§S2.1', title: 'Projects Tree',          desc: 'T3-style 280px sidebar: ⌘K search · sort · expand · j/k/o/↵ keyboard nav' },
  { id: 'left-rail',              ref: '§S2.2', title: 'Left Rail',              desc: 'Claude-Code-style rail: 3 mode tabs · pinned · recents · routines · more ▾' },
  { id: 'right-rail',             ref: '§S2.2b', title: 'Right Rail',            desc: 'Right-rail panel with Files and Diff modes · ⌘⇧F toggle' },
  { id: 'files-panel',            ref: '§S2.3', title: 'Files Panel',            desc: 'Right-rail files panel with filter + ?-prefix content-search mode (accent border)' },
  { id: 'turn-diff-inspector',    ref: '§S2.4', title: 'Turn Diff Inspector',    desc: 'Right-rail per-turn diff: turn chips, ±N pill, hunk lines, collapsed-context marker' },
  { id: 'file-viewer',            ref: '§S2.5', title: 'File Viewer',            desc: 'Read-only file viewer: gutter line numbers · / search · n/N nav · Esc close' },
  { id: 'terminal-drawer',       ref: '§S2.6', title: 'Terminal Drawer',        desc: 'Bottom-dock terminal with live prompt, ⌘J toggle, timestamp, tab glyphs' },
  { id: 'top-action-bar',        ref: '§S2.7', title: 'Top Action Bar',         desc: 'Breadcrumb chips › separator, + Add action, Open ▾ and Commit & push ▾ popovers' },
  { id: 'document-mode',         ref: '§S2.8', title: 'Document Mode',          desc: 'Full mock conversation: user bubble → tool beats → assistant markdown → commit card' },
  { id: 'tool-beats',            ref: '§S2.9', title: 'Tool Beats',             desc: 'Collapsible single-line rows: Ran (red) · Recalled (accent) · Read (text)' },
  { id: 'commit-summary',        ref: '§S2.10', title: 'Commit Summary',        desc: 'Branch swap chips from←to · +additions −deletions pill · Create PR ▾ popover' },
  { id: 'composer-chips',        ref: '§S2.11', title: 'Composer Chips',        desc: 'Model · reasoning · mode · permissions · token counter row above composer' },
  { id: 'composer-footer',       ref: '§S2.12', title: 'Composer Footer',       desc: 'Bypass permissions · attach · mic glyphs left · Opus 4.7 1M · Extra high right' },
  { id: 'composite-canvas',            ref: '§S2.13', title: 'Composite Canvas',          desc: 'HERO — full 1440×900 canvas: ProjectsTree + TopActionBar + convo + ⌘J terminal + ⌘B rail' },
  { id: 'composite-canvas-all-open',   ref: '§S3.A',  title: 'Canvas — All Open',         desc: 'Three columns (ProjectsTree + main + RightRail) + TerminalDrawer bottom dock' },
  { id: 'composite-canvas-leftrail',   ref: '§S3.B',  title: 'Canvas — Left Rail',        desc: 'Left-rail variant (240 px LeftRail) replacing ProjectsTree column' },
  { id: 'composite-canvas-empty',      ref: '§S3.C',  title: 'Canvas — Empty',            desc: 'Empty conversation state — clean slate with no turns' },
  { id: 'composite-canvas-streaming',  ref: '§S3.D',  title: 'Canvas — Streaming',        desc: 'Token-drip streaming turn + working-for counter + red stop button' },
  { id: 'composite-canvas-mission',          ref: '§S3.E',  title: 'Canvas — Mission Running',  desc: '3-phase run log: ✓ done · ○ live-counting · ○ pending' },
  { id: 'composite-canvas-review-gate',      ref: '§S4.F',  title: 'Canvas — Review Gate',      desc: 'Changed Files card embedded in conversation + View diff navigates to diff viewer' },
  { id: 'composite-canvas-error',            ref: '§S4.G',  title: 'Canvas — Error',            desc: 'Red border composer · inline error label · Failed ToolBeat with Alert icon' },
  { id: 'composite-canvas-voice',            ref: '§S4.H',  title: 'Canvas — Voice',            desc: '9-bin VoiceOverlay + transcript preview · Esc exits to idle' },
  { id: 'composite-canvas-files-and-viewer', ref: '§S4.I',  title: 'Canvas — Files + Viewer',   desc: 'FilesPanel (right rail) + FileViewer (main column) · state lift on file select' },
  { id: 'composite-canvas-plugin-inline',    ref: '§S4.J',  title: 'Canvas — Plugin Inline',    desc: 'tracker issue card injected inline after turn 2 — §3.10 embed contract' },
  { id: 'composite-canvas-notifications',    ref: '§S4.K',  title: 'Canvas — Notifications',    desc: 'All three tiers: ambient HUD (top-right) · banner (below top bar) · footer badges' },
  { id: 'action-palette', ref: '§S5.L', title: 'Action Palette', desc: 'Centered floating popover · item title + ≥3 verb actions with keycaps · j/k/Enter/Esc' },
  { id: 'fan-view',       ref: '§S5.M', title: 'Fan View',       desc: 'Radial thread switcher · 60° arc · 80 px radius · 36 px chips · j/k scroll · Esc dismiss' },
  { id: 'pill-drag-snap', ref: '§S5.N', title: 'Pill Drag Snap', desc: 'Horizontal pill drag · snap-line guides at 40% and 60% · Esc mid-drag cancels' },
  { id: 'transition-l0-to-l3', ref: '§S5.O', title: 'Transition L0 → L3', desc: 'Single PaletteShell substrate morphing through L0 → L1 → L2 → L3 · ▶ Next / space steps · ≤ 220 ms transitions · Esc returns to L0' },
  { id: 'tour', ref: '§S5', title: 'Guided Tour', desc: '26-step DIY tour across 7 chapters · → ← step · space autoplay · esc exit' },
  { id: 'real-usage-demo', ref: '§S6', title: 'Real-Usage Progressive Demo', desc: 'Watch every Whim feature unfold from the idle pill driven by simulated real-life actions · → ← step · space autoplay · esc exit' },
  { id: 'live-composite-canvas', ref: '§S7', title: 'Live Chat', desc: 'Live dust API connection — thread ID from ?thread= query param · real ConvItem stream' },
  { id: 'live-files-panel', ref: '§M4', title: 'Live Files Panel (real fs)', desc: 'Wired to dust list_directory + search_files + whim://fs-changed watcher · click a row to open in viewer' },
  { id: 'live-diff-panel',              ref: '§M5', title: 'Live Diff Panel (real fs)',              desc: 'Wired to dust list_changed_files + get_file_diff + reject_hunk · click file tab to load hunks' },
  { id: 'live-turn-diff-inspector',     ref: '§M6', title: 'Live Turn Diff Inspector (real fs)',     desc: 'Wired to dust list_changed_files + get_file_diff · per-turn diff chips with accept/reject IPC' },
  { id: 'live-terminal-drawer',         ref: '§M6', title: 'Live Terminal Drawer (mission log)',     desc: 'Wired to dust list_missions + start_mission_run_watcher · streams whim://mission-run-output, capped at 1000 lines' },
  { id: 'live-mission-run-canvas',      ref: '§M6', title: 'Live Mission Run Canvas',                desc: 'Full canvas substrate driven by useMissions · stacked run log with live elapsed counter from active mission checkpoint' },
  { id: 'live-left-rail',               ref: '§M7', title: 'Live Left Rail',                         desc: 'Wired to list_projects + list_routines + read_pins + useChat threads · whim://routines-changed atomic-replace' },
  { id: 'live-top-action-bar',          ref: '§M7', title: 'Live Top Action Bar',                    desc: 'Wired to get_repo_status · breadcrumbs from live branch · auto-refresh on whim://fs-changed (500 ms debounce)' },
  { id: 'live-commit-summary-card',     ref: '§M7', title: 'Live Commit Summary Card',               desc: 'Wired to get_commit_summary + get_pr_metadata · createPr re-fetches PR metadata on success' },
  { id: 'live-notifications-canvas',    ref: '§M7', title: 'Live Notifications Canvas',              desc: 'Wired to list_notifications + whim://notification (append) + whim://notification-update (replace) · capped at 500' },
]

// Demo sequence: the 7 scenarios walked programmatically
const DEMO_SEQUENCE = [
  '3.1-idle',
  '3.2-hover',
  '3.3-type',
  '3.7-mission-spawn',
  '3.8-mission-progress',
  '3.9-review-gate',
  '3.9b-diff-viewer',
]
const DEMO_STEP_MS = 5500

// ─── Hash router ───────────────────────────────────────────────────────────────

type Route =
  | { kind: 'index' }
  | { kind: 'scene'; id: string }

function parseHash(): Route {
  const h = window.location.hash
  const m = h.match(/^#\/s\/(.+)$/)
  if (m) return { kind: 'scene', id: m[1] }
  return { kind: 'index' }
}

function useHashRoute(): Route {
  const [route, setRoute] = useState<Route>(parseHash)
  useEffect(() => {
    const handler = () => setRoute(parseHash())
    window.addEventListener('hashchange', handler)
    return () => window.removeEventListener('hashchange', handler)
  }, [])
  return route
}

// ─── Demo mode ────────────────────────────────────────────────────────────────

interface DemoState {
  active: boolean
  paused: boolean
  stepIndex: number
}

function useDemoMode() {
  const [state, setState] = useState<DemoState>({ active: false, paused: false, stepIndex: 0 })
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  const clearTimer = () => {
    if (timerRef.current) { clearTimeout(timerRef.current); timerRef.current = null }
  }

  const exit = useCallback(() => {
    clearTimer()
    setState({ active: false, paused: false, stepIndex: 0 })
    window.location.hash = '#/scenarios'
  }, [])

  const start = useCallback(() => {
    clearTimer()
    setState({ active: true, paused: false, stepIndex: 0 })
    window.location.hash = `#/s/${DEMO_SEQUENCE[0]}`
  }, [])

  const togglePause = useCallback(() => {
    setState(s => ({ ...s, paused: !s.paused }))
  }, [])

  // Advance timer
  useEffect(() => {
    if (!state.active || state.paused) { clearTimer(); return }
    timerRef.current = setTimeout(() => {
      const next = state.stepIndex + 1
      if (next >= DEMO_SEQUENCE.length) { exit(); return }
      setState(s => ({ ...s, stepIndex: next }))
      window.location.hash = `#/s/${DEMO_SEQUENCE[next]}`
    }, DEMO_STEP_MS)
    return clearTimer
  }, [state.active, state.paused, state.stepIndex, exit])

  return { ...state, start, exit, togglePause }
}

// ─── Keymap overlay ───────────────────────────────────────────────────────────

function KeymapOverlay({ onClose }: { onClose: () => void }) {
  const rows: [string, string][] = [
    ['j / k',    'move selection down / up'],
    ['Enter',    'open selected scenario'],
    ['Space',    'start play-demo mode'],
    ['?',        'toggle this keymap overlay'],
    ['Esc',      'close overlay'],
    ['— within scenario —', ''],
    ['g h',      'return to index'],
    ['g j',      'next scenario'],
    ['g k',      'previous scenario'],
    ['— demo mode —', ''],
    ['Space',    'pause / resume'],
    ['Esc',      'exit demo'],
    ['— diff viewer —', ''],
    ['j / k',    'prev / next hunk'],
    ['a / d',    'accept / discard hunk'],
    ['⌘ D',      'open diff viewer from review gate'],
    ['r',        'retry failed diff apply'],
  ]

  return (
    <div
      onClick={onClose}
      style={{
        position:       'fixed',
        inset:          0,
        background:     'rgba(12,13,16,0.72)',
        backdropFilter: 'blur(6px)',
        display:        'flex',
        alignItems:     'center',
        justifyContent: 'center',
        zIndex:         9000,
      }}
    >
      <div
        onClick={e => e.stopPropagation()}
        style={{
          background:   'var(--s1)',
          border:       '0.5px solid var(--border)',
          borderRadius: '12px',
          padding:      '28px 32px',
          width:        '420px',
          maxHeight:    '80vh',
          overflowY:    'auto',
        }}
      >
        <div style={{
          fontFamily:    'var(--mono)',
          fontSize:      '10px',
          color:         'var(--accent)',
          letterSpacing: '0.1em',
          textTransform: 'uppercase',
          marginBottom:  '20px',
        }}>
          Keymap
        </div>
        <table style={{ width: '100%', borderCollapse: 'collapse' }}>
          <tbody>
            {rows.map(([key, action], i) => (
              action === '' ? (
                <tr key={i}>
                  <td colSpan={2} style={{
                    paddingTop:    '16px',
                    paddingBottom: '6px',
                    fontFamily:    'var(--mono)',
                    fontSize:      '10px',
                    color:         'var(--faint)',
                    letterSpacing: '0.08em',
                  }}>
                    {key}
                  </td>
                </tr>
              ) : (
                <tr key={i}>
                  <td style={{
                    paddingBottom: '8px',
                    paddingRight:  '24px',
                    fontFamily:    'var(--mono)',
                    fontSize:      '12px',
                    color:         'var(--accent-2)',
                    whiteSpace:    'nowrap',
                    verticalAlign: 'top',
                  }}>
                    {key}
                  </td>
                  <td style={{
                    paddingBottom: '8px',
                    fontFamily:    'var(--sans)',
                    fontSize:      '13px',
                    color:         'var(--muted)',
                    verticalAlign: 'top',
                  }}>
                    {action}
                  </td>
                </tr>
              )
            ))}
          </tbody>
        </table>
        <div style={{ marginTop: '20px', textAlign: 'right' }}>
          <button
            onClick={onClose}
            style={{
              fontFamily:    'var(--mono)',
              fontSize:      '11px',
              color:         'var(--faint)',
              background:    'none',
              border:        'none',
              cursor:        'pointer',
              letterSpacing: '0.06em',
            }}
          >
            Esc to close
          </button>
        </div>
      </div>
    </div>
  )
}

// ─── Demo overlay ─────────────────────────────────────────────────────────────

function DemoOverlay({
  scenarioId,
  stepIndex,
  paused,
}: {
  scenarioId: string
  stepIndex: number
  paused: boolean
}) {
  const progress = ((stepIndex + 1) / DEMO_SEQUENCE.length) * 100

  return (
    <div style={{
      position:     'fixed',
      bottom:       '24px',
      right:        '24px',
      background:   'var(--s2)',
      border:       '0.5px solid var(--border)',
      borderRadius: '10px',
      padding:      '12px 16px',
      zIndex:       8000,
      minWidth:     '220px',
      boxShadow:    '0 8px 32px rgba(0,0,0,0.5)',
    }}>
      <div style={{
        display:        'flex',
        alignItems:     'center',
        gap:            '8px',
        marginBottom:   '10px',
      }}>
        <span style={{
          fontFamily:    'var(--mono)',
          fontSize:      '9px',
          color:         paused ? 'var(--muted)' : 'var(--accent)',
          letterSpacing: '0.12em',
          textTransform: 'uppercase',
        }}>
          {paused ? 'PAUSED' : 'DEMO'}
        </span>
        <span style={{ color: 'var(--ghost)', fontFamily: 'var(--mono)', fontSize: '9px' }}>•</span>
        <span style={{
          fontFamily:    'var(--mono)',
          fontSize:      '11px',
          color:         'var(--text)',
          letterSpacing: '0.04em',
        }}>
          {scenarioId}
        </span>
      </div>

      {/* Progress bar */}
      <div style={{
        height:       '2px',
        background:   'var(--s3)',
        borderRadius: '1px',
        overflow:     'hidden',
        marginBottom: '10px',
      }}>
        <div style={{
          height:     '100%',
          width:      `${progress}%`,
          background: 'var(--accent)',
          transition: 'width 0.3s ease',
        }} />
      </div>

      <div style={{
        display:  'flex',
        gap:      '12px',
        fontSize: '11px',
        color:    'var(--faint)',
        fontFamily: 'var(--mono)',
      }}>
        <span>{stepIndex + 1} / {DEMO_SEQUENCE.length}</span>
        <span>·</span>
        <span>Space pause · Esc exit</span>
      </div>
    </div>
  )
}

// ─── Scenario index ───────────────────────────────────────────────────────────

function ScenarioIndex({ onStartDemo }: { onStartDemo: () => void }) {
  const [selected, setSelected] = useState(0)
  const [showKeymap, setShowKeymap] = useState(false)
  const rowRefs = useRef<(HTMLAnchorElement | null)[]>([])

  useEffect(() => {
    rowRefs.current[selected]?.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
  }, [selected])

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (showKeymap && e.key === 'Escape') { setShowKeymap(false); return }
      if (e.key === '?') { setShowKeymap(s => !s); return }
      if (showKeymap) return

      if (e.key === 'j') {
        e.preventDefault()
        setSelected(s => Math.min(s + 1, SCENARIO_LIST.length - 1))
      } else if (e.key === 'k') {
        e.preventDefault()
        setSelected(s => Math.max(s - 1, 0))
      } else if (e.key === 'Enter') {
        window.location.hash = `#/s/${SCENARIO_LIST[selected].id}`
      } else if (e.key === ' ') {
        e.preventDefault()
        onStartDemo()
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [selected, showKeymap, onStartDemo])

  return (
    <>
      {showKeymap && <KeymapOverlay onClose={() => setShowKeymap(false)} />}
      <main style={{
        display:       'flex',
        flexDirection: 'column',
        alignItems:    'center',
        gap:           '32px',
        padding:       '64px 40px 100px',
        fontFamily:    'var(--sans)',
      }}>
        {/* Header */}
        <div style={{ textAlign: 'center' }}>
          <h1 style={{
            fontSize:      '26px',
            fontWeight:    600,
            letterSpacing: '-0.02em',
            margin:        '0 0 8px',
            color:         'var(--text)',
          }}>
            Whim Scenarios
          </h1>
          <p style={{
            color:         'var(--muted)',
            fontFamily:    'var(--mono)',
            fontSize:      '11px',
            margin:        '0 0 6px',
            letterSpacing: '0.08em',
          }}>
            spine · 16 scenes
          </p>
          <p style={{
            color:         'var(--faint)',
            fontFamily:    'var(--mono)',
            fontSize:      '10px',
            margin:        0,
            letterSpacing: '0.06em',
          }}>
            j/k navigate · Enter open · Space demo · ? keymap
          </p>
        </div>

        {/* List */}
        <div style={{
          width:        '820px',
          border:       '0.5px solid var(--border)',
          borderRadius: '10px',
          overflow:     'hidden',
        }}>
          {SCENARIO_LIST.map((s, i) => (
            <a
              key={s.id}
              ref={el => { rowRefs.current[i] = el }}
              href={`#/s/${s.id}`}
              onClick={() => setSelected(i)}
              onMouseEnter={() => setSelected(i)}
              style={{
                display:          'grid',
                gridTemplateColumns: '72px 1fr 3fr',
                alignItems:       'center',
                gap:              '16px',
                padding:          '13px 20px',
                textDecoration:   'none',
                borderBottom:     i < SCENARIO_LIST.length - 1 ? '0.5px solid var(--border-soft)' : 'none',
                background:       selected === i ? 'var(--s2)' : 'var(--s1)',
                transition:       'background 0.08s ease',
                outline:          selected === i ? '1px solid var(--accent-rim)' : 'none',
                outlineOffset:    '-1px',
              }}
            >
              {/* UX ref */}
              <span style={{
                fontFamily:    'var(--mono)',
                fontSize:      '10px',
                color:         selected === i ? 'var(--accent)' : 'var(--faint)',
                letterSpacing: '0.08em',
              }}>
                {s.ref}
              </span>

              {/* Title */}
              <span style={{
                fontSize:   '13.5px',
                fontWeight: 600,
                color:      selected === i ? 'var(--text)' : 'var(--muted)',
                whiteSpace: 'nowrap',
              }}>
                {s.title}
              </span>

              {/* Desc */}
              <span style={{
                fontSize:  '12px',
                color:     'var(--faint)',
                lineHeight: 1.4,
              }}>
                {s.desc}
              </span>
            </a>
          ))}
        </div>

        {/* Footer hint */}
        <div style={{
          fontFamily:    'var(--mono)',
          fontSize:      '10px',
          color:         'var(--ghost)',
          letterSpacing: '0.06em',
          textAlign:     'center',
        }}>
          Press <span style={{ color: 'var(--accent-2)' }}>Space</span> to launch play-demo mode (7 scenes · ~40s)
        </div>
      </main>
    </>
  )
}

// ─── Scene view with vim-nav + demo overlay ───────────────────────────────────

interface DemoControls {
  active: boolean
  paused: boolean
  stepIndex: number
  exit: () => void
  togglePause: () => void
}

function SceneView({ id, demo }: { id: string; demo: DemoControls }) {
  const currentIndex = SCENARIO_LIST.findIndex(s => s.id === id)
  const gPressedRef = useRef(false)
  const gTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      // In demo mode only handle demo keys
      if (demo.active) {
        if (e.key === ' ') { e.preventDefault(); demo.togglePause() }
        else if (e.key === 'Escape') demo.exit()
        return
      }

      // Vim-style chord: g → h/j/k
      if (e.key === 'g') {
        gPressedRef.current = true
        if (gTimerRef.current) clearTimeout(gTimerRef.current)
        gTimerRef.current = setTimeout(() => { gPressedRef.current = false }, 1500)
      } else if (gPressedRef.current) {
        gPressedRef.current = false
        if (gTimerRef.current) { clearTimeout(gTimerRef.current); gTimerRef.current = null }

        if (e.key === 'h') {
          window.location.hash = '#/scenarios'
        } else if (e.key === 'j') {
          if (currentIndex >= 0 && currentIndex < SCENARIO_LIST.length - 1) {
            window.location.hash = `#/s/${SCENARIO_LIST[currentIndex + 1].id}`
          }
        } else if (e.key === 'k') {
          if (currentIndex > 0) {
            window.location.hash = `#/s/${SCENARIO_LIST[currentIndex - 1].id}`
          }
        }
      }
    }

    window.addEventListener('keydown', handler)
    return () => {
      window.removeEventListener('keydown', handler)
      if (gTimerRef.current) clearTimeout(gTimerRef.current)
    }
  }, [id, currentIndex, demo])

  const SceneComponent = SCENE_COMPONENTS[id]

  return (
    <div style={{ position: 'relative', minHeight: '100vh' }}>
      {SceneComponent ? (
        <SceneComponent />
      ) : (
        <main style={{
          display:       'flex',
          flexDirection: 'column',
          alignItems:    'center',
          padding:       '80px 40px',
          fontFamily:    'var(--sans)',
          gap:           '16px',
        }}>
          <p style={{ color: 'var(--red)', fontFamily: 'var(--mono)', fontSize: '13px' }}>
            Unknown scenario: {id}
          </p>
          <a href="#/scenarios" style={{ color: 'var(--accent)', fontFamily: 'var(--mono)', fontSize: '12px' }}>
            ← Back to scenarios
          </a>
        </main>
      )}

      {demo.active && (
        <DemoOverlay
          scenarioId={id}
          stepIndex={demo.stepIndex}
          paused={demo.paused}
        />
      )}

      {/* Vim nav hint (non-demo) */}
      {!demo.active && (
        <div style={{
          position:   'fixed',
          bottom:     '16px',
          right:      '16px',
          fontFamily: 'var(--mono)',
          fontSize:   '10px',
          color:      'var(--ghost)',
          letterSpacing: '0.06em',
          pointerEvents: 'none',
        }}>
          g h ← index · g j next · g k prev
        </div>
      )}
    </div>
  )
}

// ─── App ──────────────────────────────────────────────────────────────────────

export function App() {
  const route = useHashRoute()
  const demo = useDemoMode()

  // Redirect empty or bare # to #/scenarios
  useEffect(() => {
    const h = window.location.hash
    if (!h || h === '#' || h === '#/' || h === '#/scenarios' || h.startsWith('#/s/')) return
    window.location.hash = '#/scenarios'
  }, [])

  if (route.kind === 'scene') {
    return (
      <SceneView
        id={route.id}
        demo={{
          active:      demo.active,
          paused:      demo.paused,
          stepIndex:   demo.stepIndex,
          exit:        demo.exit,
          togglePause: demo.togglePause,
        }}
      />
    )
  }

  return <ScenarioIndex onStartDemo={demo.start} />
}
