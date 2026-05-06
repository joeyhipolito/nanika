import { useCallback, useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { FileEntry, FileMatch, SearchKind } from '../types'

export interface UseFilesViewer {
  content: string | null
  loading: boolean
  error:   string | null
}

export interface UseFilesReturn {
  tree:           FileEntry[]
  currentPath:    string
  setCurrentPath: (p: string) => void
  searchResults:  FileMatch[]
  search:         (needle: string, kind: SearchKind) => void
  viewer:         UseFilesViewer
  openFile:       (path: string) => void
  loading:        boolean
  error:          string | null
}

interface FsChangedPayload {
  path: string
  kind: 'created' | 'modified' | 'deleted'
}

function joinPath(root: string, sub: string): string {
  if (!sub) return root
  const trimmedRoot = root.endsWith('/') ? root.slice(0, -1) : root
  const trimmedSub  = sub.startsWith('/') ? sub.slice(1) : sub
  return `${trimmedRoot}/${trimmedSub}`
}

export function useFiles(repoRoot: string, opts?: { initialPath?: string }): UseFilesReturn {
  const [currentPath, setCurrentPathState] = useState<string>(opts?.initialPath ?? '')
  const [tree, setTree]                   = useState<FileEntry[]>([])
  const [searchResults, setSearchResults] = useState<FileMatch[]>([])
  const [viewer, setViewer]               = useState<UseFilesViewer>({ content: null, loading: false, error: null })
  const [loading, setLoading]             = useState(false)
  const [error, setError]                 = useState<string | null>(null)

  // Synchronous mirror of currentPath — the fs-changed listener reads this
  // without triggering a re-subscribe on every path change.
  const currentPathRef = useRef<string>(opts?.initialPath ?? '')
  const searchTimer    = useRef<ReturnType<typeof setTimeout> | null>(null)

  // ── Fetch the tree on currentPath change ─────────────────────────────────

  useEffect(() => {
    let cancelled = false
    const abs = joinPath(repoRoot, currentPath)
    setLoading(true)
    setError(null)

    invoke<FileEntry[]>('list_directory', { path: abs })
      .then(entries => {
        if (cancelled) return
        // Atomic replace — never mutate in place.
        setTree(entries)
        setLoading(false)
      })
      .catch(err => {
        if (cancelled) return
        setError(String(err))
        setLoading(false)
      })

    return () => { cancelled = true }
  }, [repoRoot, currentPath])

  // ── Start the watcher once per repoRoot ──────────────────────────────────
  // Backend replaces the prior watcher in AppState, so consecutive calls are
  // safe; the previous debounce thread tears down via mpsc disconnect.

  useEffect(() => {
    invoke('watch_repo', { path: repoRoot }).catch(err => {
      // Non-fatal: tree still works without live updates.
      // eslint-disable-next-line no-console
      console.error('watch_repo failed', err)
    })
  }, [repoRoot])

  // ── Single persistent fs-changed listener ────────────────────────────────

  useEffect(() => {
    let unlisten: (() => void) | null = null

    listen<FsChangedPayload>('whim://fs-changed', ({ payload }) => {
      const abs = joinPath(repoRoot, currentPathRef.current)
      // Only re-fetch when the change lands inside the directory we're showing.
      if (!payload.path.startsWith(abs)) return
      invoke<FileEntry[]>('list_directory', { path: abs })
        .then(entries => setTree(entries))           // atomic replace
        .catch(err => setError(String(err)))
    })
      .then(f => { unlisten = f })

    return () => { unlisten?.() }
  }, [repoRoot])

  // ── setCurrentPath ───────────────────────────────────────────────────────

  const setCurrentPath = useCallback((p: string) => {
    currentPathRef.current = p
    setCurrentPathState(p)
  }, [])

  // ── search (debounced 150 ms) ────────────────────────────────────────────

  const search = useCallback((needle: string, kind: SearchKind) => {
    if (searchTimer.current) clearTimeout(searchTimer.current)

    if (!needle) {
      setSearchResults([])
      return
    }

    searchTimer.current = setTimeout(() => {
      invoke<FileMatch[]>('search_files', { root: repoRoot, query: needle, kind })
        .then(matches => setSearchResults(matches))
        .catch(err => setError(String(err)))
    }, 150)
  }, [repoRoot])

  // ── openFile ─────────────────────────────────────────────────────────────

  const openFile = useCallback((path: string) => {
    setViewer({ content: null, loading: true, error: null })
    invoke<string>('read_file', { path })
      .then(content => setViewer({ content, loading: false, error: null }))
      .catch(err => setViewer({ content: null, loading: false, error: String(err) }))
  }, [])

  // ── Cleanup the debounce timer on unmount ────────────────────────────────

  useEffect(() => () => {
    if (searchTimer.current) clearTimeout(searchTimer.current)
  }, [])

  return {
    tree,
    currentPath,
    setCurrentPath,
    searchResults,
    search,
    viewer,
    openFile,
    loading,
    error,
  }
}
