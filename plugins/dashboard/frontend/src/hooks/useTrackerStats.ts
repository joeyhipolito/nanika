import { useState, useCallback, useEffect } from 'react'
import type { TrackerStats } from '../types'
import { getTrackerStats } from '../lib/wails'

type StatsState = {
  stats: TrackerStats | null
  loading: boolean
  error: string | null
}

export function useTrackerStats() {
  const [state, setState] = useState<StatsState>({
    stats: null,
    loading: false,
    error: null,
  })

  const fetchStats = useCallback(async () => {
    setState(s => ({ ...s, loading: true, error: null }))
    try {
      const data = await getTrackerStats()
      setState({ stats: data, loading: false, error: null })
    } catch (err) {
      setState(s => ({
        ...s,
        loading: false,
        error: err instanceof Error ? err.message : 'Failed to fetch tracker stats',
      }))
    }
  }, [])

  useEffect(() => {
    void fetchStats()
  }, [fetchStats])

  return {
    stats: state.stats,
    loading: state.loading,
    error: state.error,
    refresh: fetchStats,
  }
}
