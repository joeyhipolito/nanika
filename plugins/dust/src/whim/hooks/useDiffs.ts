import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { CODE_DIFF_ACCEPT_OP } from '../protocol'
import type { ChangedFile, Hunk } from '../types'

// ─── Tauri command wrappers ───────────────────────────────────────────────────

export async function listChangedFiles(repoRoot: string): Promise<ChangedFile[]> {
  return invoke<ChangedFile[]>('list_changed_files', { repoRoot })
}

export async function getFileDiff(
  repoRoot: string,
  path: string,
  base?: string,
): Promise<Hunk[]> {
  return invoke<Hunk[]>('get_file_diff', { repoRoot, path, base: base ?? null })
}

export async function rejectHunkCmd(hunkId: string): Promise<void> {
  return invoke<void>('reject_hunk', { hunkId })
}

// ─── Hook ─────────────────────────────────────────────────────────────────────

export interface UseDiffsReturn {
  files:      ChangedFile[]
  loading:    boolean
  error:      string | null
  loadDiff:   (path: string, base?: string) => void
  acceptHunk: (hunkId: string) => void
  rejectHunk: (hunkId: string) => void
  refresh:    () => void
}

export function useDiffs(repoRoot: string): UseDiffsReturn {
  const [files, setFiles]     = useState<ChangedFile[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError]     = useState<string | null>(null)

  const refresh = useCallback(() => {
    setLoading(true)
    setError(null)
    listChangedFiles(repoRoot)
      .then(result => { setFiles(result); setLoading(false) })
      .catch(err   => { setError(String(err)); setLoading(false) })
  }, [repoRoot])

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    setError(null)
    listChangedFiles(repoRoot)
      .then(result => { if (!cancelled) { setFiles(result); setLoading(false) } })
      .catch(err   => { if (!cancelled) { setError(String(err)); setLoading(false) } })
    return () => { cancelled = true }
  }, [repoRoot])

  const loadDiff = useCallback((path: string, base?: string) => {
    getFileDiff(repoRoot, path, base)
      .then(hunks => {
        setFiles(prev => prev.map(f => f.path === path ? { ...f, hunks } : f))
      })
      .catch(err => setError(String(err)))
  }, [repoRoot])

  const acceptHunk = useCallback((hunkId: string) => {
    invoke('dispatch_action', {
      pluginId:     'chat',
      capabilityId: 'code_diff',
      actionId:     CODE_DIFF_ACCEPT_OP,
      params:       { hunk_id: hunkId },
    })
      .catch(err => setError(String(err)))
      .finally(() => refresh())
  }, [refresh])

  const rejectHunk = useCallback((hunkId: string) => {
    rejectHunkCmd(hunkId)
      .catch(err => setError(String(err)))
      .finally(() => refresh())
  }, [refresh])

  return { files, loading, error, loadDiff, acceptHunk, rejectHunk, refresh }
}
