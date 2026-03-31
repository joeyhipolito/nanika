import { useState, useEffect } from 'react'
import { getSchedulerHealth } from '../lib/wails'

export type SchedulerJobStatus = {
  name: string
  status: 'healthy' | 'degraded' | 'error'
  message: string
}

export type SchedulerHealthState = {
  jobs: SchedulerJobStatus[]
  loading: boolean
  error: string | null
}

export function useSchedulerHealth(): SchedulerHealthState {
  const [state, setState] = useState<SchedulerHealthState>({
    jobs: [],
    loading: true,
    error: null,
  })

  useEffect(() => {
    let cancelled = false

    getSchedulerHealth()
      .then(raw => {
        if (cancelled) return
        // Normalise the backend shape into a flat list.
        // Expected: { status: string, jobs: Array<{ name, status, last_run?, error? }> }
        const rawJobs = Array.isArray((raw as Record<string, unknown>).jobs)
          ? ((raw as Record<string, unknown>).jobs as Record<string, unknown>[])
          : []

        if (rawJobs.length === 0) {
          // Fallback: treat the whole response as a single "daemon" entry.
          const overallStatus = (raw.status as string | undefined) ?? ''
          const status: SchedulerJobStatus['status'] =
            overallStatus === 'healthy' ? 'healthy' :
            overallStatus === 'degraded' ? 'degraded' :
            overallStatus ? 'error' : 'healthy'
          setState({
            jobs: [{ name: 'scheduler daemon', status, message: overallStatus || 'ok' }],
            loading: false,
            error: null,
          })
          return
        }

        const jobs: SchedulerJobStatus[] = rawJobs.map(j => {
          const statusRaw = (j.status as string | undefined) ?? ''
          const status: SchedulerJobStatus['status'] =
            statusRaw === 'healthy' ? 'healthy' :
            statusRaw === 'degraded' ? 'degraded' :
            statusRaw === 'ok' ? 'healthy' :
            statusRaw === 'error' ? 'error' :
            'error'
          return {
            name: (j.name as string | undefined) ?? 'unknown',
            status,
            message: (j.error as string | undefined) ?? (j.last_run as string | undefined) ?? (statusRaw || 'ok'),
          }
        })
        setState({ jobs, loading: false, error: null })
      })
      .catch(err => {
        if (cancelled) return
        setState({ jobs: [], loading: false, error: String(err) })
      })

    return () => { cancelled = true }
  }, [])

  return state
}
