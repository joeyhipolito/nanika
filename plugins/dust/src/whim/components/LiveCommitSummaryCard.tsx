import { useMemo } from 'react'
import { useGitRepo } from '../hooks/useGitRepo'
import { useCommitSummary } from '../hooks/useCommitSummary'
import { CommitSummaryCard } from './CommitSummaryCard'

const DEMO_REPO_ROOT = '/Users/joeyhipolito/nanika'

// `git show --stat` ends with: " N files changed, A insertions(+), D deletions(-)"
// We extract A and D from the stats blob so the CommitSummaryCard's +/- pill
// reflects the live commit. Missing values default to 0 (e.g. merge commits).
const STAT_RE = /(\d+)\s+insertions?\(\+\)|(\d+)\s+deletions?\(-\)/g

function parseStats(stats: string): { additions: number; deletions: number } {
  let additions = 0
  let deletions = 0
  for (const m of stats.matchAll(STAT_RE)) {
    if (m[1]) additions = parseInt(m[1], 10)
    if (m[2]) deletions = parseInt(m[2], 10)
  }
  return { additions, deletions }
}

export interface LiveCommitSummaryCardProps {
  repoRoot?: string
  commit?:   string
}

export function LiveCommitSummaryCard({
  repoRoot = DEMO_REPO_ROOT,
  commit   = 'HEAD',
}: LiveCommitSummaryCardProps) {
  const { status } = useGitRepo(repoRoot)
  const { summary, loading, error } = useCommitSummary(repoRoot, commit)

  const { additions, deletions } = useMemo(
    () => (summary ? parseStats(summary.stats) : { additions: 0, deletions: 0 }),
    [summary],
  )

  if (loading && !summary) {
    return (
      <div style={{ padding: 12, fontFamily: 'var(--mono)', fontSize: 12, color: 'var(--ghost)' }}>
        loading commit…
      </div>
    )
  }

  if (error && !summary) {
    return (
      <div style={{ padding: 12, fontFamily: 'var(--mono)', fontSize: 12, color: 'var(--red)' }}>
        {error}
      </div>
    )
  }

  const branch    = status?.branch ?? 'unknown'
  const shortHash = summary ? summary.hash.slice(0, 7) : commit

  return (
    <CommitSummaryCard
      from={branch}
      to={shortHash}
      additions={additions}
      deletions={deletions}
    />
  )
}
