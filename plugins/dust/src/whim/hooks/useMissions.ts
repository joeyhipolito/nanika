import { useCallback, useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type {
  GateDecision,
  MissionDetail,
  MissionEventPayload,
  MissionSummary,
  PhaseDetail,
} from '../types'
import { useTerminalLog } from './useTerminalLog'

export interface UseMissionsReturn {
  missions:           MissionSummary[]
  activeMissionId:    string | null
  activeMission:      MissionDetail | null
  setActiveMissionId: (id: string | null) => void
  activePhaseId:      string | null
  setActivePhaseId:   (id: string | null) => void
  activePhase:        PhaseDetail | null
  runOutput:          string[]
  refresh:            () => void
  approveGate:        (gateId: string, decision: GateDecision) => Promise<void>
  cancelMission:      () => Promise<void>
  rerunPhase:         (phaseId: string) => Promise<void>
  loading:            boolean
  error:              string | null
}

const ACTIVE_STATUSES = new Set(['running', 'in_progress', 'pending'])

function pickActive(list: MissionSummary[]): string | null {
  const running = list.find(m => ACTIVE_STATUSES.has(m.status))
  return running?.id ?? list[0]?.id ?? null
}

export function useMissions(): UseMissionsReturn {
  const [missions, setMissions]               = useState<MissionSummary[]>([])
  const [activeMission, setActiveMission]     = useState<MissionDetail | null>(null)
  const [activeMissionId, setActiveIdState]   = useState<string | null>(null)
  const [activePhase, setActivePhase]         = useState<PhaseDetail | null>(null)
  const [activePhaseId, setActivePhaseIdState] = useState<string | null>(null)
  const [loading, setLoading]                 = useState(false)
  const [error, setError]                     = useState<string | null>(null)

  // Synchronous mirrors of the active ids — the mission-event listener reads
  // these without re-subscribing on every change.
  const activeIdRef      = useRef<string | null>(null)
  const activePhaseIdRef = useRef<string | null>(null)

  // ── Initial fetch + auto-pick active mission ──────────────────────────────

  const refresh = useCallback(() => {
    let cancelled = false
    setLoading(true)
    setError(null)
    invoke<MissionSummary[]>('list_missions')
      .then(list => {
        if (cancelled) return
        setMissions(list)
        // Auto-pick if none chosen yet.
        if (activeIdRef.current === null) {
          const next = pickActive(list)
          activeIdRef.current = next
          setActiveIdState(next)
        }
        setLoading(false)
      })
      .catch(err => {
        if (cancelled) return
        setError(String(err))
        setLoading(false)
      })
    return () => { cancelled = true }
  }, [])

  useEffect(() => {
    const cleanup = refresh()
    return cleanup
  }, [refresh])

  // ── Fetch detail + start watcher whenever activeMissionId changes ─────────

  useEffect(() => {
    if (!activeMissionId) {
      setActiveMission(null)
      return
    }
    let cancelled = false
    invoke<MissionDetail>('get_mission', { missionId: activeMissionId })
      .then(detail => { if (!cancelled) setActiveMission(detail) })
      .catch(err   => { if (!cancelled) setError(String(err)) })

    invoke('start_mission_run_watcher', { missionId: activeMissionId })
      .catch(err => {
        // Non-fatal — list/detail still work without live updates.
        // eslint-disable-next-line no-console
        console.error('start_mission_run_watcher failed', err)
      })

    return () => { cancelled = true }
  }, [activeMissionId])

  // ── Fetch phase detail whenever active mission/phase changes ──────────────

  useEffect(() => {
    if (!activeMissionId || !activePhaseId) {
      setActivePhase(null)
      return
    }
    let cancelled = false
    invoke<PhaseDetail>('get_phase', {
      missionId: activeMissionId,
      phaseId:   activePhaseId,
    })
      .then(detail => { if (!cancelled) setActivePhase(detail) })
      .catch(err   => { if (!cancelled) setError(String(err)) })
    return () => { cancelled = true }
  }, [activeMissionId, activePhaseId])

  // ── Single persistent mission-event listener ──────────────────────────────

  useEffect(() => {
    let cancelled = false
    let unlisten: (() => void) | null = null

    listen<MissionEventPayload>('whim://mission-event', ({ payload }) => {
      if (cancelled) return
      if (payload.mission_id !== activeIdRef.current) return
      // Re-fetch on any checkpoint change — atomic-replace activeMission and
      // refresh the list so summary counts stay current.
      invoke<MissionDetail>('get_mission', { missionId: payload.mission_id })
        .then(detail => { if (!cancelled) setActiveMission(detail) })
        .catch(err => { if (!cancelled) setError(String(err)) })
      invoke<MissionSummary[]>('list_missions')
        .then(list => { if (!cancelled) setMissions(list) })
        .catch(() => {})
      // Atomic-replace the active phase detail when one is selected.
      if (activePhaseIdRef.current) {
        invoke<PhaseDetail>('get_phase', {
          missionId: payload.mission_id,
          phaseId:   activePhaseIdRef.current,
        })
          .then(detail => { if (!cancelled) setActivePhase(detail) })
          .catch(() => {})
      }
    })
      .then(f => {
        // Unmount may have beat us — release immediately.
        if (cancelled) f()
        else unlisten = f
      })

    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [])

  // ── setters ───────────────────────────────────────────────────────────────

  const setActiveMissionId = useCallback((id: string | null) => {
    activeIdRef.current = id
    setActiveIdState(id)
  }, [])

  const setActivePhaseId = useCallback((id: string | null) => {
    activePhaseIdRef.current = id
    setActivePhaseIdState(id)
  }, [])

  // ── Action wrappers ───────────────────────────────────────────────────────

  const approveGate = useCallback(async (gateId: string, decision: GateDecision) => {
    if (!activeIdRef.current) throw new Error('no active mission')
    await invoke('mission_approve_gate', {
      missionId: activeIdRef.current,
      gateId,
      decision,
    })
  }, [])

  const cancelMission = useCallback(async () => {
    if (!activeIdRef.current) throw new Error('no active mission')
    await invoke('mission_cancel', { missionId: activeIdRef.current })
  }, [])

  const rerunPhase = useCallback(async (phaseId: string) => {
    if (!activeIdRef.current) throw new Error('no active mission')
    await invoke('phase_rerun', { missionId: activeIdRef.current, phaseId })
  }, [])

  // ── runOutput: per-phase log tail piggy-backs on useTerminalLog ───────────

  const { lines: runOutput } = useTerminalLog(
    activeMissionId,
    activePhaseId ?? undefined,
  )

  return {
    missions,
    activeMissionId,
    activeMission,
    setActiveMissionId,
    activePhaseId,
    setActivePhaseId,
    activePhase,
    runOutput,
    refresh,
    approveGate,
    cancelMission,
    rerunPhase,
    loading,
    error,
  }
}
