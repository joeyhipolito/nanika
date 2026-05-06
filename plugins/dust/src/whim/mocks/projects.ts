export interface Thread {
  id: string
  title: string
  agoLabel: string
}

export interface Project {
  id: string
  name: string
  avatarInitials: string
  avatarColor: string
  threads: Thread[]
}

export const MOCK_PROJECTS: Project[] = [
  {
    id: 'nanika',
    name: 'nanika',
    avatarInitials: 'N',
    avatarColor: '#D47856',
    threads: [
      { id: 'thread-whim-palette',  title: 'whim palette · slice 2',       agoLabel: '2d ago' },
      { id: 'thread-trk-558',       title: 'TRK-558 code parity review',   agoLabel: '4d ago' },
      { id: 'thread-nen-daemon',    title: 'nen daemon gyo observer',       agoLabel: '6d ago' },
      { id: 'thread-dream-miner',   title: 'orchestrator dream mining',     agoLabel: '9d ago' },
    ],
  },
  {
    id: 'whim-web',
    name: 'whim-web',
    avatarInitials: 'W',
    avatarColor: '#3B82F6',
    threads: [
      { id: 'thread-scenes',     title: 'scene registry refactor',    agoLabel: '1d ago' },
      { id: 'thread-leftrail',   title: 'left rail + projects tree',  agoLabel: '3d ago' },
    ],
  },
]
