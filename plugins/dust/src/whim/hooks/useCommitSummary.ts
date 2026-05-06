import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { CommitSummary, PrMeta } from '../types'

export interface UseCommitSummaryReturn {
  summary:   CommitSummary | null
  pr:        PrMeta | null
  createPr:  (title: string, body?: string, draft?: boolean) => Promise<void>
  loading:   boolean
  error:     string | null
}

export function useCommitSummary(repoRoot: string, commit: string): UseCommitSummaryReturn {
  const [summary, setSummary] = useState<CommitSummary | null>(null)
  const [pr,      setPr]      = useState<PrMeta | null>(null)
  const [loading, setLoading] = useState(true)
  const [error,   setError]   = useState<string | null>(null)

  // ── Fetch summary + PR metadata on mount / dep change ────────────────────

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    setError(null)

    invoke<CommitSummary>('get_commit_summary', { repoRoot, commit })
      .then(s => { if (!cancelled) setSummary(s) })
      .catch(err => { if (!cancelled) setError(String(err)) })

    // PR metadata is best-effort: branch may have no PR yet, or `gh` may be
    // missing. Don't surface those failures as fatal errors — leave `pr` null.
    invoke<PrMeta>('get_pr_metadata', { repoRoot, branch: commit })
      .then(p => { if (!cancelled) setPr(p) })
      .catch(() => { if (!cancelled) setPr(null) })
      .finally(() => { if (!cancelled) setLoading(false) })

    return () => { cancelled = true }
  }, [repoRoot, commit])

  // ── createPr — re-fetches PR metadata after success ──────────────────────

  const createPr = useCallback(
    async (title: string, body?: string, draft?: boolean) => {
      await invoke<string>('create_pr', { repoRoot, title, body, draft })
      // Re-fetch authoritative PR metadata for the branch.
      try {
        const p = await invoke<PrMeta>('get_pr_metadata', { repoRoot, branch: commit })
        setPr(p)
      } catch {
        setPr(null)
      }
    },
    [repoRoot, commit],
  )

  return { summary, pr, createPr, loading, error }
}
