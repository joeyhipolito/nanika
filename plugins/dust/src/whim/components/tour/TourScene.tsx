import type { SceneSpec } from '../../tour/types'
import { PaletteShell, type ResultSection } from '../PaletteShell'
import { PillDragOverlay } from '../PillDragOverlay'
import { CompositeCanvas, type CompositeCanvasState } from '../CompositeCanvas'
import { PluginShellScene } from '../../scenarios/Scenes'
import { stateDefault, stateMissionRunning } from '../../mocks/canvasStates'
import { MOCK_SECTIONS } from '../../mocks/index'

// ─── Missions mock (for palette 'missions' group) ─────────────────────────────

const MOCK_MISSION_SECTIONS: ResultSection[] = [
  {
    label: 'Active',
    items: [
      { id: 'm-trk558',   name: 'Nanika → T3 Code parity',  meta: '2 workers · 4m' },
      { id: 'm-whim-web', name: 'whim-web tour substrate',   meta: '1 worker · 1m' },
    ],
  },
  {
    label: 'Recent',
    items: [
      { id: 'm-plugin',   name: 'Plugin shell scene',        meta: 'done · 8m ago' },
    ],
  },
  {
    label: 'Scheduled',
    items: [
      { id: 'm-scout',    name: 'daily-scout',               meta: '09:00' },
    ],
  },
]

// ─── Mock detail pane content (palette-detail step) ───────────────────────────

function DetailContent() {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '10px' }}>
      <div style={{
        fontFamily:    'var(--mono)',
        fontSize:      '11px',
        color:         'var(--accent)',
        letterSpacing: '0.08em',
      }}>
        tracker: run-mission
      </div>
      <p style={{ margin: 0, fontSize: '13px', color: 'var(--muted)', lineHeight: 1.6 }}>
        Spawns a new agent mission from the current conversation context. Picks up open P1/P0
        tickets and assigns a phase plan automatically.
      </p>
      <div style={{ display: 'flex', flexDirection: 'column', gap: '6px', marginTop: '4px' }}>
        {['--from-memory for context', '--no-review for research missions', '--dry-run to preview'].map(flag => (
          <div key={flag} style={{
            fontFamily:   'var(--mono)',
            fontSize:     '12px',
            color:        'var(--muted)',
            padding:      '4px 10px',
            background:   'var(--s2)',
            borderRadius: '5px',
            border:       '0.5px solid var(--border)',
          }}>
            {flag}
          </div>
        ))}
      </div>
    </div>
  )
}

// ─── Centering shell (pill + palette scenes) ──────────────────────────────────

function CenterStage({ children }: { children: React.ReactNode }) {
  return (
    <div style={{
      width:          '100vw',
      height:         '100vh',
      background:     'var(--s0)',
      display:        'flex',
      alignItems:     'center',
      justifyContent: 'center',
      position:       'relative',
    }}>
      {children}
    </div>
  )
}

// ─── Pill scene ───────────────────────────────────────────────────────────────

function PillScene({ state }: { state: Record<string, unknown> }) {
  const mode            = (state.mode as 'idle' | 'hover' | 'type' | 'voice') ?? 'idle'
  const showDragOverlay = state.dragOverlay === 'demo'

  return (
    <CenterStage>
      <PaletteShell
        scale="pill"
        pillMode={mode}
        recording={mode === 'voice'}
        convCount={mode === 'hover' ? 3 : undefined}
        placeholder={mode === 'hover' ? 'Search Whim…' : undefined}
      />
      {showDragOverlay && (
        <PillDragOverlay active anchorPercents={[40, 60]} />
      )}
    </CenterStage>
  )
}

// ─── Palette scene ────────────────────────────────────────────────────────────

function PaletteScene({ state }: { state: Record<string, unknown> }) {
  const query         = (state.query as string) ?? ''
  const composerState = state.composerState as string | undefined
  const recording     = composerState === 'recording-vhold'
  const group         = state.group as string | undefined
  const isMissions    = group === 'missions'
  const base          = isMissions ? MOCK_MISSION_SECTIONS : MOCK_SECTIONS

  const sections = query
    ? base
        .map(sec => ({
          ...sec,
          items: sec.items.filter(item =>
            typeof item.name === 'string'
              ? item.name.toLowerCase().includes(query.toLowerCase())
              : false
          ),
        }))
        .filter(sec => sec.items.length > 0)
    : base

  return (
    <CenterStage>
      {/* Invisible anchor for SpotlightRing on the c7-multi-mission step */}
      {isMissions && (
        <div
          data-tour-anchor="palette-group-active"
          style={{ position: 'absolute', top: 'calc(50% - 60px)', left: '50%', transform: 'translateX(-50%)', width: '820px', height: '1px', pointerEvents: 'none' }}
        />
      )}
      <PaletteShell
        scale="palette"
        query={query}
        sections={sections}
        recording={recording}
        placeholder={isMissions ? 'Search missions…' : 'Search actions, threads, projects…'}
        hints={[
          { keys: ['↑', '↓'], label: 'Navigate' },
          { keys: ['↵'],       label: 'Select' },
          { keys: ['Esc'],     label: 'Close' },
        ]}
      />
    </CenterStage>
  )
}

// ─── Palette-detail scene ─────────────────────────────────────────────────────

function PaletteDetailScene({ state }: { state: Record<string, unknown> }) {
  const query = (state.query as string) ?? ''

  return (
    <CenterStage>
      <PaletteShell
        scale="detail"
        query={query}
        sections={MOCK_SECTIONS}
        detailContent={<DetailContent />}
        breadcrumb="Run mission"
        hints={[{ keys: ['Esc'], label: 'Back to results' }]}
      />
    </CenterStage>
  )
}

// ─── Canvas state builder ─────────────────────────────────────────────────────

function buildCanvasState(raw: Record<string, unknown>): CompositeCanvasState {
  const { ambientHud, notifications: notifRaw, pluginInline: piRaw, ...rest } = raw

  let notifications: CompositeCanvasState['notifications'] = stateDefault.notifications
  if (ambientHud) {
    notifications = [
      { id: 'ctx-window',  kind: 'info', surface: 'hud', body: 'Context at 78%' },
      { id: 'working-for', kind: 'info', surface: 'hud', body: 'Working for 42s' },
    ]
  } else if (Array.isArray(notifRaw)) {
    notifications = notifRaw as CompositeCanvasState['notifications']
  }

  let pluginInline: CompositeCanvasState['pluginInline'] = stateDefault.pluginInline
  if (piRaw && typeof piRaw === 'object') {
    const pi = piRaw as Record<string, unknown>
    pluginInline = {
      prefix:         String(pi.prefix ?? ''),
      afterTurnIndex: Number(pi.afterTurnIndex ?? 0),
      body:           Array.isArray(pi.body) ? pi.body : [
        { kind: 'issue', id: 'TRK-573', title: 'CodeDiff: fix apply_hunk algorithm', status: 'in_progress', priority: 'P1' },
        { kind: 'issue', id: 'TRK-558', title: 'Nanika → T3 Code parity',            status: 'in_progress', priority: 'P0' },
        { kind: 'text',  content: '2 open issues · last synced 12s ago' },
      ],
    }
  }

  // Default missionRun to a running phase when the step asks for the run-log scene
  // (conversationFixture: 'short' without an explicit missionRun key) — keeps the
  // `runlog-phase-active` spotlight anchor resolvable for step c4-run-log.
  const wantsRunLog = rest.conversationFixture === 'short' && !('missionRun' in rest)
  const missionRun = wantsRunLog
    ? stateMissionRunning.missionRun
    : (rest as Partial<CompositeCanvasState>).missionRun ?? stateDefault.missionRun

  return {
    ...stateDefault,
    ...(rest as Partial<CompositeCanvasState>),
    missionRun,
    notifications,
    pluginInline,
  } as CompositeCanvasState
}

// ─── Canvas scene ─────────────────────────────────────────────────────────────

function CanvasScene({ state }: { state: Record<string, unknown> }) {
  return <CompositeCanvas state={buildCanvasState(state)} />
}

// ─── Canvas-diff scene ────────────────────────────────────────────────────────

// Renders the full-canvas layout with the right rail open in turn-diff mode.
// Invisible sentinel anchors `diff-hunk-active` over the right-rail diff column
// so SpotlightRing can resolve the c4-diff-viewer step.
function CanvasDiffScene() {
  const state: CompositeCanvasState = {
    ...stateDefault,
    rightRailOpen: true,
    rightRailMode: 'turn-diff',
  }
  return (
    <div style={{ position: 'relative', width: '100vw', height: '100vh' }}>
      <CompositeCanvas state={state} />
      <div
        data-tour-anchor="diff-hunk-active"
        style={{ position: 'absolute', top: '40%', right: '60px', width: '240px', height: '1px', pointerEvents: 'none' }}
      />
    </div>
  )
}

// ─── Overview scene ───────────────────────────────────────────────────────────

function OverviewScene({ state }: { state: Record<string, unknown> }) {
  const route = state.route as string | undefined

  if (route?.includes('plugin-shell') || route?.includes('99-plugin')) {
    return <PluginShellScene />
  }

  const isEscLadder = route?.includes('esc-ladder')

  return (
    <div style={{
      width:          '100vw',
      height:         '100vh',
      background:     'var(--s0)',
      display:        'flex',
      alignItems:     'center',
      justifyContent: 'center',
      fontFamily:     'var(--mono)',
      fontSize:       '13px',
      color:          'var(--faint)',
      letterSpacing:  '0.06em',
      position:       'relative',
    }}>
      {isEscLadder && (
        <div
          data-tour-anchor="esc-ladder-diagram"
          style={{ position: 'absolute', top: '50%', left: '50%', transform: 'translate(-50%, -50%)', width: '420px', height: '1px', pointerEvents: 'none' }}
        />
      )}
      {route ?? 'overview'}
    </div>
  )
}

// ─── Main export ──────────────────────────────────────────────────────────────

export function TourScene({ scene }: { scene: SceneSpec }) {
  switch (scene.kind) {
    case 'pill':           return <PillScene          state={scene.state} />
    case 'palette':        return <PaletteScene       state={scene.state} />
    case 'palette-detail': return <PaletteDetailScene state={scene.state} />
    case 'canvas':         return <CanvasScene        state={scene.state} />
    case 'canvas-diff':    return <CanvasDiffScene />
    case 'overview':       return <OverviewScene      state={scene.state} />
  }
}
