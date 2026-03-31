import { useState, useEffect } from 'react'
import { getNenHealth } from '../lib/wails'

export type NenAbilityStatus = {
  name: string
  status: 'healthy' | 'degraded' | 'error'
  message: string
}

export type NenHealthState = {
  abilities: NenAbilityStatus[]
  loading: boolean
  error: string | null
}

export function useNenHealth(): NenHealthState {
  const [state, setState] = useState<NenHealthState>({
    abilities: [],
    loading: true,
    error: null,
  })

  useEffect(() => {
    let cancelled = false

    getNenHealth()
      .then(raw => {
        if (cancelled) return
        // The Nen health endpoint returns a map of ability name -> status info.
        // We normalise whatever shape the backend sends into a flat list.
        const abilities: NenAbilityStatus[] = Object.entries(raw).map(([name, val]) => {
          if (typeof val === 'object' && val !== null) {
            const v = val as Record<string, unknown>
            const statusRaw = (v.status as string | undefined) ?? ''
            const status: NenAbilityStatus['status'] =
              statusRaw === 'healthy' ? 'healthy' :
              statusRaw === 'degraded' ? 'degraded' :
              'error'
            return {
              name,
              status,
              message: (v.message as string | undefined) ?? (statusRaw || 'unknown'),
            }
          }
          // Scalar value — treat truthy as healthy
          return {
            name,
            status: val ? 'healthy' : 'error',
            message: String(val),
          }
        })
        setState({ abilities, loading: false, error: null })
      })
      .catch(err => {
        if (cancelled) return
        setState({ abilities: [], loading: false, error: String(err) })
      })

    return () => { cancelled = true }
  }, [])

  return state
}
