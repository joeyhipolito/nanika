import { useState, useCallback, useEffect } from 'react'
import type { MetricsResponse } from '../types'
import { getMetrics } from '../lib/wails'

type MetricsState = {
  metrics: MetricsResponse | null
  loading: boolean
  error: string | null
}

export function useMetrics(last = 10) {
  const [state, setState] = useState<MetricsState>({
    metrics: null,
    loading: false,
    error: null,
  })

  const fetchMetrics = useCallback(async () => {
    setState(s => ({ ...s, loading: true, error: null }))
    try {
      const data = await getMetrics(last)
      setState({ metrics: data, loading: false, error: null })
    } catch (err) {
      setState(s => ({
        ...s,
        loading: false,
        error: err instanceof Error ? err.message : 'Failed to fetch metrics',
      }))
    }
  }, [last])

  useEffect(() => {
    fetchMetrics()
  }, [fetchMetrics])

  return {
    metrics: state.metrics,
    loading: state.loading,
    error: state.error,
    refresh: fetchMetrics,
  }
}
