import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { CODE_DIFF_ACCEPT_OP, CODE_DIFF_REJECT_OP } from '../protocol'
import type { ChangedFile, Hunk } from '../types'

export interface UseDiffReturn {
  changedFiles:  ChangedFile[]
  activePath:    string | null
  setActivePath: (path: string) => void
  hunks:         Hunk[]
  acceptHunk:    (hunkId: string) => void
  rejectHunk:    (hunkId: string) => void
  loading:       boolean
  error:         string | null
}

export function useDiff(repoRoot: string): UseDiffReturn {
  const [changedFiles, setChangedFiles] = useState<ChangedFile[]>([])
  const [activePath, setActivePathRaw]  = useState<string | null>(null)
  const [hunks, setHunks]               = useState<Hunk[]>([])
  const [loading, setLoading]           = useState(false)
  const [error, setError]               = useState<string | null>(null)

  const fetchFiles = useCallback(() => {
    setLoading(true)
    setError(null)
    let cancelled = false
    invoke<ChangedFile[]>('list_changed_files', { repoRoot })
      .then(result => { if (!cancelled) { setChangedFiles(result); setLoading(false) } })
      .catch(err   => { if (!cancelled) { setError(String(err));   setLoading(false) } })
    return () => { cancelled = true }
  }, [repoRoot])

  useEffect(() => fetchFiles(), [fetchFiles])

  const setActivePath = useCallback((path: string) => {
    setActivePathRaw(path)
    setError(null)
    invoke<Hunk[]>('get_file_diff', { repoRoot, path, base: null })
      .then(result => setHunks(result))
      .catch(err   => setError(String(err)))
  }, [repoRoot])

  const acceptHunk = useCallback((hunkId: string) => {
    invoke('dispatch_action', {
      pluginId:     'chat',
      capabilityId: 'code_diff',
      actionId:     CODE_DIFF_ACCEPT_OP,
      params:       { hunk_id: hunkId },
    })
      .catch(err => setError(String(err)))
      .finally(() => fetchFiles())
  }, [fetchFiles])

  const rejectHunk = useCallback((hunkId: string) => {
    invoke('dispatch_action', {
      pluginId:     'chat',
      capabilityId: 'code_diff',
      actionId:     CODE_DIFF_REJECT_OP,
      params:       { hunk_id: hunkId },
    })
      .catch(err => setError(String(err)))
      .finally(() => fetchFiles())
  }, [fetchFiles])

  return { changedFiles, activePath, setActivePath, hunks, acceptHunk, rejectHunk, loading, error }
}
