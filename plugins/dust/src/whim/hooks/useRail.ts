import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { Project, Routine, PinnedItem, ThreadMeta } from '../types'
import { useChat } from './useChat'

export interface UseRailReturn {
  projects:     Project[]
  routines:     Routine[]
  threads:      ThreadMeta[]
  pins:         PinnedItem[]
  pinThread:    (id: string, title: string) => void
  unpinThread:  (id: string) => void
  loading:      boolean
  error:        string | null
}

export function useRail(): UseRailReturn {
  const { threads } = useChat()
  const [projects, setProjects] = useState<Project[]>([])
  const [routines, setRoutines] = useState<Routine[]>([])
  const [pins,     setPins]     = useState<PinnedItem[]>([])
  const [loading,  setLoading]  = useState(true)
  const [error,    setError]    = useState<string | null>(null)

  // ── Initial fetch ────────────────────────────────────────────────────────

  useEffect(() => {
    let cancelled = false

    Promise.all([
      invoke<Project[]>('list_projects'),
      invoke<Routine[]>('list_routines'),
      invoke<PinnedItem[]>('read_pins'),
    ])
      .then(([proj, rout, p]) => {
        if (cancelled) return
        // Atomic replace — never mutate.
        setProjects(proj)
        setRoutines(rout)
        setPins(p)
        setLoading(false)
      })
      .catch(err => {
        if (cancelled) return
        setError(String(err))
        setLoading(false)
      })

    return () => { cancelled = true }
  }, [])

  // ── Routines watcher ─────────────────────────────────────────────────────
  // Atomic-replace on every event; no in-place mutation.

  useEffect(() => {
    let unlisten: (() => void) | null = null
    let cancelled = false

    listen('whim://routines-changed', () => {
      if (cancelled) return
      invoke<Routine[]>('list_routines')
        .then(rout => { if (!cancelled) setRoutines(rout) })
        .catch(err => { if (!cancelled) setError(String(err)) })
    })
      .then(f => {
        if (cancelled) { f(); return }
        unlisten = f
      })

    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [])

  // ── Pin mutators — local optimistic update + persist ─────────────────────

  const pinThread = useCallback((id: string, title: string) => {
    setPins(curr => {
      if (curr.some(p => p.id === id)) return curr
      const next = [...curr, { id, title }]
      invoke('write_pins', { pins: next }).catch(err => setError(String(err)))
      return next
    })
  }, [])

  const unpinThread = useCallback((id: string) => {
    setPins(curr => {
      if (!curr.some(p => p.id === id)) return curr
      const next = curr.filter(p => p.id !== id)
      invoke('write_pins', { pins: next }).catch(err => setError(String(err)))
      return next
    })
  }, [])

  return {
    projects,
    routines,
    threads,
    pins,
    pinThread,
    unpinThread,
    loading,
    error,
  }
}
