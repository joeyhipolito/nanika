"use client"
import { useEffect, useMemo, useState } from 'react'
import { useMissions } from '../hooks/useMissions'
import { CompositeCanvas, type CompositeCanvasState } from './CompositeCanvas'
import type { PhaseSummary } from '../types'

type RenderedStatus = 'pending' | 'running' | 'done' | 'failed'

function mapStatus(s: string): RenderedStatus {
  if (s === 'completed') return 'done'
  if (s === 'running' || s === 'in_progress') return 'running'
  if (s === 'failed') return 'failed'
  return 'pending'
}

function pickActivePhase(phases: PhaseSummary[]): string | null {
  const running = phases.find(p => mapStatus(p.status) === 'running')
  return running?.phase ?? null
}

export function LiveMissionRunCanvas() {
  const { activeMission } = useMissions()
  const [now, setNow] = useState(() => Date.now())

  // Tick once per second only while a mission is actively running, so the
  // run-log duration counter stays live without burning cycles when idle.
  const isRunning = activeMission?.status === 'running' || activeMission?.status === 'in_progress'
  useEffect(() => {
    if (!isRunning) return
    const id = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(id)
  }, [isRunning])

  const liveState = useMemo<CompositeCanvasState>(() => {
    const base: CompositeCanvasState = {
      railVariant:         'projects-tree',
      terminalOpen:        false,
      rightRailOpen:       false,
      rightRailMode:       'files',
      selectedFile:        null,
      conversationFixture: 'empty',
      composerState:       'idle',
      errorMessage:        null,
      notifications:       [],
      missionRun:          null,
      pluginInline:        null,
      streamingTurn:       null,
      workingForMs:        null,
      reviewGate:          false,
      voiceTranscript:     null,
    }
    if (!activeMission) return base

    const startedMs = activeMission.started_at ? Date.parse(activeMission.started_at) : 0
    const elapsedMs = startedMs > 0 ? Math.max(0, now - startedMs) : 0

    return {
      ...base,
      missionRun: {
        missionId:     activeMission.id,
        title:         activeMission.slug,
        activePhaseId: pickActivePhase(activeMission.phases),
        elapsedMs,
        phases: activeMission.phases.map(p => ({
          id:         p.phase,
          persona:    p.persona,
          status:     mapStatus(p.status),
          expectedMs: 60_000,
        })),
      },
    }
  }, [activeMission, now])

  if (!activeMission) {
    return (
      <main
        role="status"
        style={{
          display:        'flex',
          alignItems:     'center',
          justifyContent: 'center',
          minHeight:      '100vh',
          background:     'var(--s0)',
          fontFamily:     'var(--mono)',
          fontSize:       '13px',
          color:          'var(--ghost)',
          letterSpacing:  '0.06em',
        }}
      >
        no active mission
      </main>
    )
  }

  return <CompositeCanvas state={liveState} />
}
