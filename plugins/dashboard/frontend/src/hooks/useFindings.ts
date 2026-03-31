import { useState, useCallback, useEffect } from 'react'
import type { Finding, FindingSeverity } from '../types'
import { getFindings } from '../lib/wails'

export type FindingsFilter = {
  ability?: string
  severity?: FindingSeverity
  limit?: number
}

type FindingsState = {
  findings: Finding[]
  loading: boolean
  error: string | null
}

export function useFindings(filter: FindingsFilter = {}) {
  const [state, setState] = useState<FindingsState>({
    findings: [],
    loading: false,
    error: null,
  })

  const fetchFindings = useCallback(async () => {
    setState(s => ({ ...s, loading: true, error: null }))
    try {
      const params = new URLSearchParams()
      if (filter.ability) params.set('ability', filter.ability)
      if (filter.severity) params.set('severity', filter.severity)
      if (filter.limit != null) params.set('limit', String(filter.limit))

      const data = await getFindings(params.toString() || undefined)
      setState({ findings: data ?? [], loading: false, error: null })
    } catch (err) {
      setState(s => ({
        ...s,
        loading: false,
        error: err instanceof Error ? err.message : 'Failed to fetch findings',
      }))
    }
  }, [filter.ability, filter.severity, filter.limit])

  useEffect(() => {
    fetchFindings()
  }, [fetchFindings])

  return {
    findings: state.findings,
    loading: state.loading,
    error: state.error,
    refresh: fetchFindings,
  }
}
