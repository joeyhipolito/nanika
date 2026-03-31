import { useState, useCallback, useEffect } from 'react'
import type { DecompositionFindingsResponse } from '../types'
import { isWails } from '../lib/wails'

const BASE = '/api/orchestrator'

// emptyResponse is returned in Wails mode where no daemon API binding exists.
const emptyResponse: DecompositionFindingsResponse = {
  counts: [],
  recent: [],
  daily_trends: [],
  weekly_trends: [],
}

type DecompositionFindingsState = {
  data: DecompositionFindingsResponse | null
  loading: boolean
  error: string | null
}

export function useDecompositionFindings() {
  const [state, setState] = useState<DecompositionFindingsState>({
    data: null,
    loading: false,
    error: null,
  })

  const fetchFindings = useCallback(async () => {
    setState(s => ({ ...s, loading: true, error: null }))

    // In Wails mode there is no daemon API binding for decomposition findings.
    // Return empty data rather than making a failing HTTP request to the embedded asset server.
    if (isWails()) {
      setState({ data: emptyResponse, loading: false, error: null })
      return
    }

    try {
      const res = await fetch(`${BASE}/decomposition-findings`)
      if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
      const data = await res.json() as DecompositionFindingsResponse
      setState({ data, loading: false, error: null })
    } catch (err) {
      setState(s => ({
        ...s,
        loading: false,
        error: err instanceof Error ? err.message : 'Failed to fetch decomposition findings',
      }))
    }
  }, [])

  useEffect(() => {
    fetchFindings()
  }, [fetchFindings])

  return {
    data: state.data,
    loading: state.loading,
    error: state.error,
    refresh: fetchFindings,
  }
}
