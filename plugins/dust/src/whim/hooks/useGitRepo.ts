import { useCallback, useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { RepoStatus } from '../types'

export interface UseGitRepoReturn {
  status:          RepoStatus | null
  refresh:         () => void
  stageAll:        () => Promise<void>
  commit:          (message: string) => Promise<void>
  push:            () => Promise<void>
  revealInFinder:  (path: string) => Promise<void>
  openUrl:         (url: string) => Promise<void>
  loading:         boolean
  error:           string | null
}

export function useGitRepo(repoRoot: string): UseGitRepoReturn {
  const [status,  setStatus]  = useState<RepoStatus | null>(null)
  const [loading, setLoading] = useState(false)
  const [error,   setError]   = useState<string | null>(null)

  // Synchronous mirrors — debounce + listener don't need to bust on each render.
  const repoRootRef    = useRef(repoRoot)
  const debounceTimer  = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => { repoRootRef.current = repoRoot }, [repoRoot])

  const fetchStatus = useCallback(() => {
    setLoading(true)
    invoke<RepoStatus>('get_repo_status', { repoRoot: repoRootRef.current })
      .then(s => {
        setStatus(s)              // atomic replace
        setLoading(false)
        setError(null)
      })
      .catch(err => {
        setError(String(err))
        setLoading(false)
      })
  }, [])

  // ── Initial fetch ────────────────────────────────────────────────────────

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    invoke<RepoStatus>('get_repo_status', { repoRoot })
      .then(s => {
        if (cancelled) return
        setStatus(s)
        setLoading(false)
      })
      .catch(err => {
        if (cancelled) return
        setError(String(err))
        setLoading(false)
      })
    return () => { cancelled = true }
  }, [repoRoot])

  // ── fs-changed listener with 500 ms debounce ─────────────────────────────

  useEffect(() => {
    let unlisten: (() => void) | null = null
    let cancelled = false

    listen('whim://fs-changed', () => {
      if (cancelled) return
      if (debounceTimer.current) clearTimeout(debounceTimer.current)
      debounceTimer.current = setTimeout(() => {
        if (cancelled) return
        fetchStatus()
      }, 500)
    })
      .then(f => {
        if (cancelled) { f(); return }
        unlisten = f
      })

    return () => {
      cancelled = true
      if (debounceTimer.current) clearTimeout(debounceTimer.current)
      unlisten?.()
    }
  }, [fetchStatus])

  // ── Actions ──────────────────────────────────────────────────────────────

  const stageAll = useCallback(async () => {
    await invoke('git_stage_all', { repoRoot: repoRootRef.current })
    fetchStatus()
  }, [fetchStatus])

  const commit = useCallback(async (message: string) => {
    await invoke('git_commit', { repoRoot: repoRootRef.current, message })
    fetchStatus()
  }, [fetchStatus])

  const push = useCallback(async () => {
    // Backend gates on WHIM_ALLOW_PUSH and returns Err otherwise; surface that
    // straight to the caller so the UI can show the gate message.
    await invoke('git_push', { repoRoot: repoRootRef.current })
    fetchStatus()
  }, [fetchStatus])

  const revealInFinder = useCallback(async (path: string) => {
    await invoke('reveal_in_finder', { path })
  }, [])

  const openUrl = useCallback(async (url: string) => {
    await invoke('open_external_url', { url })
  }, [])

  return {
    status,
    refresh: fetchStatus,
    stageAll,
    commit,
    push,
    revealInFinder,
    openUrl,
    loading,
    error,
  }
}
