import { useState, useEffect, useCallback } from 'react'
import { checkHealth } from '../lib/wails'

export function useOrchestrator() {
  const [isConnected, setIsConnected] = useState(false)

  const doCheckHealth = useCallback(async () => {
    try {
      setIsConnected(await checkHealth())
    } catch {
      setIsConnected(false)
    }
  }, [])

  useEffect(() => {
    doCheckHealth()
    const interval = setInterval(doCheckHealth, 30_000)
    return () => clearInterval(interval)
  }, [doCheckHealth])

  return { isConnected }
}
