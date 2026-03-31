import { useState, useCallback, useEffect } from 'react'
import type { ScannerInfo, ScanResult } from '../types'
import { listScanners, nenScan, cleanup as wailsCleanup } from '../lib/wails'

export type CleanupResult = {
  output: string
  stderr?: string
  error?: string
}

type ScannersState = {
  scanners: ScannerInfo[]
  loading: boolean
  error: string | null
}

export function useScanners() {
  const [state, setState] = useState<ScannersState>({
    scanners: [],
    loading: false,
    error: null,
  })

  const fetchScanners = useCallback(async () => {
    setState(s => ({ ...s, loading: true, error: null }))
    try {
      const data = await listScanners()
      setState({ scanners: data ?? [], loading: false, error: null })
    } catch (err) {
      setState(s => ({
        ...s,
        loading: false,
        error: err instanceof Error ? err.message : 'Failed to fetch scanners',
      }))
    }
  }, [])

  useEffect(() => {
    fetchScanners()
  }, [fetchScanners])

  const triggerScan = useCallback(async (): Promise<ScanResult> => {
    try {
      return await nenScan()
    } catch (err) {
      return { output: '', error: err instanceof Error ? err.message : 'Scan request failed' }
    }
  }, [])

  const triggerCleanup = useCallback(async (): Promise<CleanupResult> => {
    try {
      return await wailsCleanup()
    } catch (err) {
      return { output: '', error: err instanceof Error ? err.message : 'Cleanup request failed' }
    }
  }, [])

  return {
    scanners: state.scanners,
    loading: state.loading,
    error: state.error,
    refresh: fetchScanners,
    triggerScan,
    triggerCleanup,
  }
}
