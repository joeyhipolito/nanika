import { useState, useCallback, useEffect } from 'react'
import type { KoResults } from '../types'
import { getKoResults } from '../lib/wails'

type KoResultsState = {
  data: KoResults | null
  loading: boolean
  error: string | null
}

export function useKoResults() {
  const [state, setState] = useState<KoResultsState>({
    data: null,
    loading: true,
    error: null,
  })

  const fetchResults = useCallback(async () => {
    setState(s => ({ ...s, loading: true, error: null }))
    try {
      const data = await getKoResults()
      setState({ data, loading: false, error: null })
    } catch (err) {
      setState(s => ({
        ...s,
        loading: false,
        error: err instanceof Error ? err.message : 'Failed to fetch ko results',
      }))
    }
  }, [])

  useEffect(() => {
    fetchResults()
  }, [fetchResults])

  return {
    data: state.data,
    loading: state.loading,
    error: state.error,
    refresh: fetchResults,
  }
}
