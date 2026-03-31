import { useState, useCallback, useEffect } from 'react'
import type {
  OrchestratorMission,
  OrchestratorEvent,
  DAGResponse,
  MissionDetail,
  RunMissionOptions,
  RunMissionResult,
} from '../types'
import {
  listMissions,
  getMissionDetail as wailsGetMissionDetail,
  getMissionEvents as wailsGetMissionEvents,
  getMissionDAG as wailsGetMissionDAG,
  cancelMission as wailsCancelMission,
  runMission as wailsRunMission,
} from '../lib/wails'

const BASE = '/api/orchestrator'

// Standalone function — shared by the hook and App-level actionBridge so the
// fetch logic lives in exactly one place.
export async function runMissionOnce(
  task: string,
  opts: RunMissionOptions = {},
): Promise<RunMissionResult | null> {
  return wailsRunMission(task, opts)
}

type MissionsState = {
  missions: OrchestratorMission[]
  loading: boolean
  error: string | null
}

type CancelResult =
  | { ok: true; action: string }
  | { ok: false; error: string; status?: number }

export function useMissions() {
  const [state, setState] = useState<MissionsState>({
    missions: [],
    loading: false,
    error: null,
  })

  const fetchMissions = useCallback(async () => {
    setState(s => ({ ...s, loading: true, error: null }))
    try {
      const data = await listMissions()
      setState({ missions: data ?? [], loading: false, error: null })
    } catch (err) {
      setState(s => ({
        ...s,
        loading: false,
        error: err instanceof Error ? err.message : 'Failed to fetch missions',
      }))
    }
  }, [])

  useEffect(() => {
    fetchMissions()
  }, [fetchMissions])

  const getMissionDetail = useCallback(async (id: string): Promise<MissionDetail | null> => {
    try {
      return await wailsGetMissionDetail(id)
    } catch {
      return null
    }
  }, [])

  const getMissionEvents = useCallback(async (id: string): Promise<OrchestratorEvent[]> => {
    try {
      return await wailsGetMissionEvents(id)
    } catch {
      return []
    }
  }, [])

  const streamMission = useCallback((id: string): EventSource => {
    return new EventSource(`${BASE}/missions/${encodeURIComponent(id)}/stream`)
  }, [])

  const runMission = useCallback(
    (task: string, opts?: RunMissionOptions) => runMissionOnce(task, opts),
    [],
  )

  const getMissionDAG = useCallback(async (id: string): Promise<DAGResponse | null> => {
    try {
      return await wailsGetMissionDAG(id)
    } catch {
      return null
    }
  }, [])

  const cancelMission = useCallback(async (id: string): Promise<CancelResult> => {
    try {
      const body = await wailsCancelMission(id)
      // Optimistically update the mission status in the list.
      setState(s => ({
        ...s,
        missions: s.missions.map(m =>
          m.mission_id === id ? { ...m, status: 'cancelled' } : m
        ),
      }))
      return { ok: true, action: (body['action'] as string) ?? 'cancelled' }
    } catch (err) {
      return {
        ok: false,
        error: err instanceof Error ? err.message : 'Cancel request failed',
      }
    }
  }, [])

  return {
    missions: state.missions,
    loading: state.loading,
    error: state.error,
    refresh: fetchMissions,
    getMissionDetail,
    getMissionEvents,
    streamMission,
    runMission,
    getMissionDAG,
    cancelMission,
  }
}
