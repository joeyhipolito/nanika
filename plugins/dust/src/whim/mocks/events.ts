// Mock mission event stream — drives 3.7-mission-spawn / 3.8-mission-progress
// timer-based phases with live duration counters.

export interface MissionPhase {
  persona: string
  phase: string
  durationMs: number
}

export const MISSION_PHASES: MissionPhase[] = [
  { persona: 'architect',                phase: 'plan-architecture', durationMs: 3000  },
  { persona: 'senior-frontend-engineer', phase: 'implement-canvas',  durationMs: 12000 },
  { persona: 'staff-code-reviewer',      phase: 'review',            durationMs: 6000  },
]

export const MISSION_TOTAL_MS = MISSION_PHASES.reduce((s, p) => s + p.durationMs, 0)

export interface RunLogPhase {
  id: string
  persona: string
  phase: string
  done: boolean
  live: boolean
  durationLabel?: string
}

export function buildRunLog(elapsedMs: number): RunLogPhase[] {
  let cum = 0
  return MISSION_PHASES.map((p, i) => {
    const start = cum
    const end   = cum + p.durationMs
    cum = end
    if (elapsedMs >= end) {
      return {
        id:            `p${i}`,
        persona:       p.persona,
        phase:         p.phase,
        done:          true,
        live:          false,
        durationLabel: `${Math.round(p.durationMs / 1000)}s`,
      }
    }
    if (elapsedMs >= start) {
      return {
        id:            `p${i}`,
        persona:       p.persona,
        phase:         p.phase,
        done:          false,
        live:          true,
        durationLabel: `${Math.floor((elapsedMs - start) / 1000)}s`,
      }
    }
    return { id: `p${i}`, persona: p.persona, phase: p.phase, done: false, live: false }
  })
}

export function formatElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000))
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  return `${m}m ${String(s % 60).padStart(2, '0')}s`
}

// ─── Multi-mission directory data (3.13) ──────────────────────────────────────

export interface ActiveMission {
  id: string
  phase: string
  startedMsAgo: number
  workers: number
}

export interface RecentMission {
  id: string
  phase: string
  duration: string
  workers: number
  finishedAgo: string
}

export interface ScheduledMission {
  id: string
  phase: string
  schedule: string
  workers: number
}

export const ACTIVE_MISSIONS: ActiveMission[] = [
  { id: 'whim-palette',   phase: 'implement-canvas', startedMsAgo: 134_000, workers: 3 },
  { id: 'tracker-parity', phase: 'staff-review',     startedMsAgo:  47_000, workers: 2 },
]

export const RECENT_MISSIONS: RecentMission[] = [
  { id: 'voice-overlay',  phase: 'merged', duration: '3m 22s', workers: 1, finishedAgo: '12m ago' },
  { id: 'whim-pill',      phase: 'merged', duration: '7m 04s', workers: 2, finishedAgo: '1h ago'  },
]

export const SCHEDULED_MISSIONS: ScheduledMission[] = [
  { id: 'tracker-sweep',  phase: 'queued', schedule: 'hourly',      workers: 1 },
  { id: 'dream-extract',  phase: 'queued', schedule: 'daily 04:00', workers: 1 },
]
