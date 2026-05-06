export interface RecentItem {
  id: string
  title: string
  projectName: string
  agoLabel: string
}

export interface Routine {
  id: string
  label: string
  schedule: string
}

export const MOCK_RECENTS: RecentItem[] = [
  { id: 'r1', title: 'whim palette · slice 2 scenes',   projectName: 'whim-web',  agoLabel: '2h ago' },
  { id: 'r2', title: 'TRK-558 code parity review',      projectName: 'nanika',    agoLabel: '5h ago' },
  { id: 'r3', title: 'nen daemon gyo observer setup',    projectName: 'nanika',    agoLabel: '1d ago' },
  { id: 'r4', title: 'scheduler jobs triage',           projectName: 'nanika',    agoLabel: '2d ago' },
  { id: 'r5', title: 'orchestrator dream mining run',   projectName: 'nanika',    agoLabel: '3d ago' },
]

export const MOCK_ROUTINES: Routine[] = [
  { id: 'rt1', label: 'Daily tracker triage',     schedule: 'every day 9am' },
  { id: 'rt2', label: 'Weekly scout report',      schedule: 'every Monday 8am' },
  { id: 'rt3', label: 'Dream mining sweep',       schedule: 'every 6h' },
]
