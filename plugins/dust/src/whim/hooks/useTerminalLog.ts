import { useEffect, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import type { MissionRunOutputPayload } from '../types'

const MAX_LINES = 1000

export interface UseTerminalLogReturn {
  lines: string[]
}

export function useTerminalLog(
  missionId: string | null,
  phaseId?: string,
): UseTerminalLogReturn {
  const [lines, setLines] = useState<string[]>([])

  // Synchronous mirrors so the listener reads the current filter without
  // re-subscribing on every change.
  const missionIdRef = useRef<string | null>(missionId)
  const phaseIdRef   = useRef<string | undefined>(phaseId)

  useEffect(() => { missionIdRef.current = missionId }, [missionId])
  useEffect(() => { phaseIdRef.current   = phaseId   }, [phaseId])

  // Reset the buffer whenever the mission changes — old lines belong to a
  // different watcher session.
  useEffect(() => { setLines([]) }, [missionId])

  useEffect(() => {
    let cancelled = false
    let unlisten: (() => void) | null = null

    listen<MissionRunOutputPayload>('whim://mission-run-output', ({ payload }) => {
      if (cancelled) return
      if (missionIdRef.current === null) return
      if (payload.mission_id !== missionIdRef.current) return
      // Optional phase filter — match the phase id substring in the file path.
      // Worker dirs are named `<persona>-phase-<n>`; the line comes through with
      // the absolute file path, so substring match is the cheapest correct check.
      if (phaseIdRef.current && !payload.file.includes(phaseIdRef.current)) return

      setLines(prev => {
        const next = prev.length >= MAX_LINES
          ? [...prev.slice(prev.length - MAX_LINES + 1), payload.line]
          : [...prev, payload.line]
        return next
      })
    })
      .then(f => {
        // Unmount may have beat us — release immediately.
        if (cancelled) f()
        else unlisten = f
      })

    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [])

  return { lines }
}
