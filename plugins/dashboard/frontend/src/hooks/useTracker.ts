import { useState, useCallback, useEffect, useRef } from 'react'
import type { TrackerItem } from '../types'
import { getTrackerItems, updateTrackerItem } from '../lib/wails'

type TrackerState = {
  items: TrackerItem[]
  loading: boolean
  error: string | null
}

const REFRESH_INTERVAL_MS = 30_000

export function useTracker() {
  const [state, setState] = useState<TrackerState>({
    items: [],
    loading: true,
    error: null,
  })
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null)

  const fetchItems = useCallback(async () => {
    setState(s => ({ ...s, loading: true, error: null }))
    try {
      const data = await getTrackerItems()
      setState({ items: data ?? [], loading: false, error: null })
    } catch (err) {
      setState(s => ({
        ...s,
        loading: false,
        error: err instanceof Error ? err.message : 'Failed to fetch tracker items',
      }))
    }
  }, [])

  // Optimistically apply changes, call the API, then re-fetch to sync server state.
  const updateItem = useCallback(async (
    id: string,
    changes: { status?: string; priority?: string; labels?: string },
  ) => {
    setState(s => ({
      ...s,
      items: s.items.map(item => item.id === id ? { ...item, ...changes } : item),
    }))
    try {
      await updateTrackerItem({ id, ...changes })
    } finally {
      // Re-fetch regardless of success/failure to sync authoritative server state.
      void fetchItems()
    }
  }, [fetchItems])

  useEffect(() => {
    void fetchItems()
    timerRef.current = setInterval(() => { void fetchItems() }, REFRESH_INTERVAL_MS)
    return () => {
      if (timerRef.current != null) clearInterval(timerRef.current)
    }
  }, [fetchItems])

  return {
    items: state.items,
    loading: state.loading,
    error: state.error,
    refresh: fetchItems,
    updateItem,
  }
}
