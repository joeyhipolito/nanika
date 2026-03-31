import { useState, useMemo, useEffect, useRef } from 'react'
import { motion, AnimatePresence } from 'motion/react'
import { useEvents } from '../hooks/useEvents'
import { useMissions } from '../hooks/useMissions'
import type { OrchestratorEvent } from '../types'
import { neutral, status } from '../colors'
import { Button } from './ui/button'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from './ui/select'

function eventColor(type: string): string {
  if (type === 'mission.completed') return status.success
  if (type === 'mission.failed') return status.error
  if (type === 'mission.cancelled' || type === 'mission.stalled') return neutral.textSecondary
  if (type.startsWith('mission.')) return status.accent
  if (type.startsWith('phase.')) return status.info
  if (type.startsWith('nen.')) return status.warning
  return neutral.textSecondary
}

function formatTime(iso: string): string {
  return new Date(iso).toLocaleTimeString([], {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

function shortMissionId(id: string): string {
  const dash = id.lastIndexOf('-')
  return dash !== -1 ? id.slice(dash + 1) : id.slice(-8)
}

function shortType(type: string): string {
  const dot = type.indexOf('.')
  return dot > 0 ? type.slice(dot + 1) : type
}

function EventRow({ event }: { event: OrchestratorEvent }) {
  const color = eventColor(event.type)
  const dataKeys = event.data ? Object.keys(event.data).slice(0, 2) : []

  return (
    <motion.li
      className="event-row"
      initial={{ opacity: 0, x: -8 }}
      animate={{ opacity: 1, x: 0 }}
      transition={{ duration: 0.15 }}
      layout
    >
      <span className="event-row-time">{formatTime(event.timestamp)}</span>
      <span className="event-row-type" style={{ color }}>{shortType(event.type)}</span>
      <span className="event-row-mission">{shortMissionId(event.mission_id)}</span>
      {dataKeys.length > 0 && (
        <span className="event-row-data">
          {dataKeys.map(k => `${k}=${String(event.data![k]).slice(0, 20)}`).join(' ')}
        </span>
      )}
    </motion.li>
  )
}

// Radix Select doesn't support empty-string values; use a sentinel instead.
const ALL_MISSIONS = '__all__'

export function EventsPanel() {
  const [missionFilter, setMissionFilter] = useState<string>('')
  const { missions } = useMissions()

  // Subscribe scoped to a mission when filter is active, otherwise all events
  const { events, connected, clearEvents } = useEvents({
    missionId: missionFilter || undefined,
  })

  // Show "daemon not running" after 8 s without a successful connection.
  // Reset whenever the connection comes up.
  const [daemonDown, setDaemonDown] = useState(false)
  const daemonTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => {
    if (connected) {
      setDaemonDown(false)
      if (daemonTimerRef.current) {
        clearTimeout(daemonTimerRef.current)
        daemonTimerRef.current = null
      }
      return
    }
    daemonTimerRef.current = setTimeout(() => setDaemonDown(true), 8_000)
    return () => {
      if (daemonTimerRef.current) {
        clearTimeout(daemonTimerRef.current)
        daemonTimerRef.current = null
      }
    }
  }, [connected])

  const reversed = useMemo(() => [...events].reverse(), [events])

  const sortedMissions = useMemo(
    () => [...missions].sort((a, b) => new Date(b.modified_at).getTime() - new Date(a.modified_at).getTime()),
    [missions]
  )

  function handleSelectChange(value: string) {
    const next = value === ALL_MISSIONS ? '' : value
    setMissionFilter(next)
    clearEvents()
  }

  return (
    <div className="events-panel">
      <div className="events-toolbar">
        <span
          className={`events-conn-dot events-conn-dot--${connected ? 'on' : 'off'}`}
          title={connected ? 'SSE connected' : daemonDown ? 'Daemon not running' : 'Reconnecting…'}
          aria-label={connected ? 'SSE connected' : daemonDown ? 'Daemon not running' : 'Reconnecting…'}
        />
        <span className="events-count">
          {events.length} event{events.length !== 1 ? 's' : ''}
        </span>
        <Button
          variant="ghost"
          size="sm"
          onClick={clearEvents}
          aria-label="Clear event log"
          disabled={events.length === 0}
          className="h-auto py-0.5 px-1.5 text-xs"
          style={{ color: 'var(--text-secondary)' }}
        >
          clear
        </Button>
      </div>

      <div className="flex items-center gap-2 mb-2 flex-shrink-0">
        <label htmlFor="events-mission-filter" className="text-xs flex-shrink-0" style={{ color: 'var(--text-secondary)' }}>
          Mission
        </label>
        <Select
          value={missionFilter === '' ? ALL_MISSIONS : missionFilter}
          onValueChange={handleSelectChange}
        >
          <SelectTrigger
            id="events-mission-filter"
            className="h-7 text-xs flex-1"
            style={{ borderColor: 'var(--pill-border)', background: 'var(--mic-bg)', color: 'var(--text-primary)' }}
          >
            <SelectValue placeholder="All missions" />
          </SelectTrigger>
          <SelectContent style={{ background: 'var(--popover)', borderColor: 'var(--pill-border)' }}>
            <SelectItem value={ALL_MISSIONS}>All missions</SelectItem>
            {sortedMissions.map(m => (
              <SelectItem key={m.mission_id} value={m.mission_id}>
                {shortMissionId(m.mission_id)}{m.task ? ` — ${m.task.slice(0, 40)}${m.task.length > 40 ? '…' : ''}` : ''}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {missionFilter && (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => { setMissionFilter(''); clearEvents() }}
            aria-label="Clear mission filter"
            className="h-auto py-0.5 px-1.5 text-xs flex-shrink-0"
            style={{ color: 'var(--text-secondary)' }}
          >
            ✕
          </Button>
        )}
      </div>

      {reversed.length === 0 ? (
        <div className="panel-empty">
          {connected
            ? 'Waiting for events…'
            : daemonDown
              ? 'Events feed unavailable — daemon not running'
              : 'Connecting to event stream…'}
        </div>
      ) : (
        <ul className="events-list" role="list">
          <AnimatePresence initial={false}>
            {reversed.map(e => (
              <EventRow key={`${e.id}-${e.sequence ?? 0}`} event={e} />
            ))}
          </AnimatePresence>
        </ul>
      )}
    </div>
  )
}
