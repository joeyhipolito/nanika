import { useState, useEffect } from 'react'
import { getPluginHealth } from '../lib/wails'
import type { PluginDoctorResult } from '../types'

export type PluginHealthState = {
  plugins: PluginDoctorResult[]
  cachedAt: string | null
  loading: boolean
  error: string | null
}

export function usePluginHealth(): PluginHealthState {
  const [state, setState] = useState<PluginHealthState>({
    plugins: [],
    cachedAt: null,
    loading: true,
    error: null,
  })

  useEffect(() => {
    let cancelled = false

    getPluginHealth()
      .then(data => {
        if (cancelled) return
        setState({
          plugins: data.plugins ?? [],
          cachedAt: data.cached_at ?? null,
          loading: false,
          error: null,
        })
      })
      .catch(err => {
        if (cancelled) return
        setState({ plugins: [], cachedAt: null, loading: false, error: String(err) })
      })

    return () => { cancelled = true }
  }, [])

  return state
}
