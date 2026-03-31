import { useState, useCallback, useEffect } from 'react'
import type { PersonaResponse } from '../types'
import { listPersonas, reloadPersonas as wailsReloadPersonas } from '../lib/wails'

type PersonasState = {
  personas: PersonaResponse[]
  loading: boolean
  error: string | null
  reloading: boolean
}

export function usePersonas() {
  const [state, setState] = useState<PersonasState>({
    personas: [],
    loading: false,
    error: null,
    reloading: false,
  })

  const fetchPersonas = useCallback(async () => {
    setState(s => ({ ...s, loading: true, error: null }))
    try {
      const data = await listPersonas()
      setState(s => ({ ...s, personas: data ?? [], loading: false, error: null }))
    } catch (err) {
      setState(s => ({
        ...s,
        loading: false,
        error: err instanceof Error ? err.message : 'Failed to fetch personas',
      }))
    }
  }, [])

  useEffect(() => {
    fetchPersonas()
  }, [fetchPersonas])

  const reload = useCallback(async (): Promise<boolean> => {
    setState(s => ({ ...s, reloading: true }))
    try {
      const ok = await wailsReloadPersonas()
      setState(s => ({ ...s, reloading: false }))
      if (ok) await fetchPersonas()
      return ok
    } catch {
      setState(s => ({ ...s, reloading: false }))
      return false
    }
  }, [fetchPersonas])

  return {
    personas: state.personas,
    loading: state.loading,
    error: state.error,
    reloading: state.reloading,
    refresh: fetchPersonas,
    reload,
  }
}
