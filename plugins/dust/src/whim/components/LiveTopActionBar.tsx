import { useMemo } from 'react'
import { useGitRepo } from '../hooks/useGitRepo'
import { TopActionBar } from './TopActionBar'

const DEMO_REPO_ROOT = '/Users/joeyhipolito/nanika'

// LiveTopActionBar — derives breadcrumbs from the live git status and renders
// the existing TopActionBar surface. The current TopActionBar takes only
// `breadcrumbs` (no commit/push wiring); when it grows action props the
// useGitRepo `stageAll` / `commit` / `push` are already exposed here.

export interface LiveTopActionBarProps {
  repoRoot?: string
}

export function LiveTopActionBar({ repoRoot = DEMO_REPO_ROOT }: LiveTopActionBarProps) {
  const { status } = useGitRepo(repoRoot)

  const repoName = repoRoot.split('/').filter(Boolean).pop() ?? 'repo'

  const breadcrumbs = useMemo(() => {
    const branch = status?.branch ?? '…'
    const dirty  = status && (status.has_staged || status.has_unstaged) ? ' •' : ''
    return [
      { label: repoName },
      { label: `${branch}${dirty}` },
    ]
  }, [repoName, status])

  return <TopActionBar breadcrumbs={breadcrumbs} />
}
