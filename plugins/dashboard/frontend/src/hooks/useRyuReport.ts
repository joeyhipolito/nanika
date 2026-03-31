import { useState, useCallback, useEffect } from 'react'
import type { RyuReport } from '../types'
import { getRyuReport } from '../lib/wails'

type RyuReportState = {
  data: RyuReport | null
  loading: boolean
  error: string | null
}

export function useRyuReport() {
  const [state, setState] = useState<RyuReportState>({
    data: null,
    loading: true,
    error: null,
  })

  const fetchReport = useCallback(async () => {
    setState(s => ({ ...s, loading: true, error: null }))
    try {
      const data = await getRyuReport()
      setState({ data, loading: false, error: null })
    } catch (err) {
      setState(s => ({
        ...s,
        loading: false,
        error: err instanceof Error ? err.message : 'Failed to fetch ryu report',
      }))
    }
  }, [])

  useEffect(() => {
    fetchReport()
  }, [fetchReport])

  return {
    data: state.data,
    loading: state.loading,
    error: state.error,
    refresh: fetchReport,
  }
}
