import { useMemo } from 'react'
import { useRail } from '../hooks/useRail'
import { LeftRail } from './LeftRail'
import type { RecentItem, Routine as MockRoutine } from '../mocks/leftrail'
import type { Project, Routine, ThreadMeta } from '../types'

// LiveLeftRail — wires the M7 useRail hook to the existing LeftRail
// presentational component. Wire-shape data (Project/Routine/ThreadMeta)
// is adapted at this boundary to the mock RecentItem / Routine shape the
// LeftRail component already accepts, so the presentational surface keeps
// working unchanged in all 53 prior scenes.

function relativeAgo(ts: number): string {
  const ms = Date.now() - ts * 1000
  if (ms < 0)              return 'just now'
  if (ms < 60_000)         return 'just now'
  if (ms < 3_600_000)      return `${Math.floor(ms / 60_000)}m ago`
  if (ms < 86_400_000)     return `${Math.floor(ms / 3_600_000)}h ago`
  return `${Math.floor(ms / 86_400_000)}d ago`
}

function toRecent(thread: ThreadMeta, projectName: string): RecentItem {
  return {
    id:          thread.id,
    title:       thread.title,
    projectName,
    agoLabel:    relativeAgo(thread.updated_at || thread.created_at),
  }
}

function toMockRoutine(r: Routine): MockRoutine {
  return {
    id:       r.name,
    label:    r.name,
    schedule: r.schedule,
  }
}

export function LiveLeftRail() {
  const { projects, routines, threads } = useRail()

  const projectName = projects[0]?.name ?? 'project'

  const recents = useMemo<RecentItem[]>(
    () => threads.slice(0, 12).map(t => toRecent(t, projectName)),
    [threads, projectName],
  )

  const mockRoutines = useMemo<MockRoutine[]>(
    () => routines.map(toMockRoutine),
    [routines],
  )

  return <LeftRail recents={recents} routines={mockRoutines} />
}

export type { Project }
