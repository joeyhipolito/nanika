import { useEffect, useState } from 'react'
import { getOrchestratorHealth, getChannelStatus } from '../lib/wails'
import { useNenHealth } from '../hooks/useNenHealth'
import { useSchedulerHealth } from '../hooks/useSchedulerHealth'
import { usePluginHealth } from '../hooks/usePluginHealth'
import type { DaemonStatus, ChannelStatus } from '../types'
import type { NenAbilityStatus } from '../hooks/useNenHealth'
import type { SchedulerJobStatus } from '../hooks/useSchedulerHealth'
import type { PluginDoctorResult } from '../types'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type HealthStatus = 'healthy' | 'degraded' | 'error' | 'checking'

interface HealthItem {
  name: string
  status: HealthStatus
  statusText: string
}

interface HealthSection {
  title: string
  items: HealthItem[]
  loading: boolean
  error: string | null
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function statusFromString(s: string): HealthStatus {
  if (s === 'running' || s === 'ok' || s === 'healthy') return 'healthy'
  if (s === 'degraded') return 'degraded'
  return 'error'
}

function sortItems(items: HealthItem[]): HealthItem[] {
  const order: Record<HealthStatus, number> = { error: 0, degraded: 1, checking: 2, healthy: 3 }
  return [...items].sort((a, b) => order[a.status] - order[b.status])
}

function countUnhealthy(items: HealthItem[]): number {
  return items.filter(i => i.status === 'error' || i.status === 'degraded').length
}

// ---------------------------------------------------------------------------
// StatusDot
// ---------------------------------------------------------------------------

interface StatusDotProps {
  status: HealthStatus
}

function StatusDot({ status }: StatusDotProps) {
  const colorMap: Record<HealthStatus, string> = {
    healthy: 'bg-green-500',
    degraded: 'bg-yellow-400',
    error: 'bg-red-500',
    checking: 'bg-gray-400 animate-pulse',
  }
  return (
    <span
      className={`inline-block h-2 w-2 rounded-full flex-shrink-0 ${colorMap[status]}`}
      aria-hidden="true"
    />
  )
}

// ---------------------------------------------------------------------------
// HealthRow
// ---------------------------------------------------------------------------

function HealthRow({ item }: { item: HealthItem }) {
  return (
    <li className="flex items-center gap-2 py-1 text-sm">
      <StatusDot status={item.status} />
      <span className="flex-1 text-[var(--text-primary)] truncate">{item.name}</span>
      <span className="text-[var(--text-secondary)] text-xs tabular-nums">{item.statusText}</span>
    </li>
  )
}

// ---------------------------------------------------------------------------
// SectionBlock
// ---------------------------------------------------------------------------

function SectionBlock({ section }: { section: HealthSection }) {
  const sorted = sortItems(section.items)
  const unhealthy = countUnhealthy(sorted)

  return (
    <section className="settings-section">
      <h3 className="settings-section-title">
        {section.title}
        {!section.loading && unhealthy > 0 && (
          <span className="ml-2 inline-flex items-center rounded-full bg-red-900/50 px-1.5 py-0.5 text-xs font-medium text-red-300">
            {unhealthy} {unhealthy === 1 ? 'issue' : 'issues'}
          </span>
        )}
      </h3>

      {section.loading && (
        <p className="panel-empty text-xs">checking…</p>
      )}
      {!section.loading && section.error && (
        <p className="panel-empty panel-empty--error text-xs">{section.error}</p>
      )}
      {!section.loading && !section.error && sorted.length === 0 && (
        <p className="panel-empty text-xs">No items.</p>
      )}
      {!section.loading && !section.error && sorted.length > 0 && (
        <ul className="divide-y divide-[var(--border)] -my-1" role="list">
          {sorted.map(item => (
            <HealthRow key={item.name} item={item} />
          ))}
        </ul>
      )}
    </section>
  )
}

// ---------------------------------------------------------------------------
// SummaryBadge
// ---------------------------------------------------------------------------

function SummaryBadge({ sections }: { sections: HealthSection[] }) {
  const allLoading = sections.every(s => s.loading)
  const totalUnhealthy = sections.reduce((sum, s) => sum + countUnhealthy(s.items), 0)
  const hasError = sections.some(s => s.error !== null)

  if (allLoading) {
    return (
      <div className="mb-4 rounded-lg border border-[var(--border)] px-3 py-2 text-xs text-[var(--text-secondary)]">
        Checking system health…
      </div>
    )
  }

  if (hasError || totalUnhealthy > 0) {
    return (
      <div className="mb-4 flex items-center gap-2 rounded-lg border border-red-800 bg-red-900/20 px-3 py-2 text-xs text-red-300">
        <span className="inline-block h-2 w-2 rounded-full bg-red-500 flex-shrink-0" aria-hidden="true" />
        {totalUnhealthy} {totalUnhealthy === 1 ? 'issue' : 'issues'} detected
        {hasError && ' — some checks failed'}
      </div>
    )
  }

  return (
    <div className="mb-4 flex items-center gap-2 rounded-lg border border-green-800 bg-green-900/20 px-3 py-2 text-xs text-green-300">
      <span className="inline-block h-2 w-2 rounded-full bg-green-500 flex-shrink-0" aria-hidden="true" />
      All systems healthy
    </div>
  )
}

// ---------------------------------------------------------------------------
// Daemons section — uses getOrchestratorHealth
// ---------------------------------------------------------------------------

function useDaemonHealth() {
  const [daemons, setDaemons] = useState<DaemonStatus[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    getOrchestratorHealth()
      .then(data => {
        if (cancelled) return
        setDaemons(data.daemons ?? [])
        setLoading(false)
      })
      .catch(err => {
        if (cancelled) return
        setError(String(err))
        setLoading(false)
      })
    return () => { cancelled = true }
  }, [])

  return { daemons, loading, error }
}

// ---------------------------------------------------------------------------
// Channels section — uses getChannelStatus
// ---------------------------------------------------------------------------

function useChannelHealth() {
  const [channels, setChannels] = useState<ChannelStatus[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    getChannelStatus()
      .then(data => {
        if (cancelled) return
        setChannels(data ?? [])
        setLoading(false)
      })
      .catch(err => {
        if (cancelled) return
        setError(String(err))
        setLoading(false)
      })
    return () => { cancelled = true }
  }, [])

  return { channels, loading, error }
}

// ---------------------------------------------------------------------------
// Static paths section
// ---------------------------------------------------------------------------

const PATHS: HealthItem[] = [
  { name: '~/.alluka', status: 'healthy', statusText: 'config root' },
  { name: '~/.alluka/missions', status: 'healthy', statusText: 'missions' },
  { name: '~/.alluka/workspaces', status: 'healthy', statusText: 'workspaces' },
  { name: '~/nanika/plugins', status: 'healthy', statusText: 'plugins source' },
]

// ---------------------------------------------------------------------------
// SystemPanel
// ---------------------------------------------------------------------------

interface SystemPanelProps {
  isConnected?: boolean
}

export function SystemPanel({ isConnected = false }: SystemPanelProps) {
  const daemonHealth = useDaemonHealth()
  const nenHealth = useNenHealth()
  const schedulerHealth = useSchedulerHealth()
  const pluginHealth = usePluginHealth()
  const channelHealth = useChannelHealth()

  // Map daemon data → HealthItems
  const daemonItems: HealthItem[] = daemonHealth.daemons.map((d: DaemonStatus) => ({
    name: d.name,
    status: statusFromString(d.status),
    statusText: d.status === 'running' ? `running (pid ${d.pid})` : d.status,
  }))
  // Include connection status as a synthetic daemon entry
  if (!daemonHealth.loading && !daemonHealth.error) {
    daemonItems.push({
      name: 'dashboard',
      status: isConnected ? 'healthy' : 'error',
      statusText: isConnected ? 'connected' : 'offline',
    })
  }

  // Map nen data → HealthItems
  const nenItems: HealthItem[] = nenHealth.abilities.map((a: NenAbilityStatus) => ({
    name: a.name,
    status: a.status,
    statusText: a.message,
  }))

  // Map scheduler data → HealthItems
  const schedulerItems: HealthItem[] = schedulerHealth.jobs.map((j: SchedulerJobStatus) => ({
    name: j.name,
    status: j.status,
    statusText: j.message,
  }))

  // Map plugin doctor results → HealthItems
  const pluginItems: HealthItem[] = pluginHealth.plugins.map((p: PluginDoctorResult) => ({
    name: p.name,
    status: p.status === 'ok' ? 'healthy' : p.status === 'unavailable' ? 'degraded' : 'error',
    statusText: p.error ?? p.status,
  }))

  // Map channel data → HealthItems
  const channelItems: HealthItem[] = channelHealth.channels.map((c: ChannelStatus) => {
    const status: HealthStatus =
      !c.configured ? 'degraded' :
      c.error_count > 0 ? 'error' :
      c.active ? 'healthy' : 'degraded'
    const statusText =
      !c.configured ? 'not configured' :
      c.last_error ? c.last_error :
      c.active ? 'active' : 'inactive'
    return { name: c.name, status, statusText }
  })

  const sections: HealthSection[] = [
    {
      title: 'Daemons',
      items: daemonItems,
      loading: daemonHealth.loading,
      error: daemonHealth.error,
    },
    {
      title: 'Nen Abilities',
      items: nenItems,
      loading: nenHealth.loading,
      error: nenHealth.error,
    },
    {
      title: 'Scheduler',
      items: schedulerItems,
      loading: schedulerHealth.loading,
      error: schedulerHealth.error,
    },
    {
      title: 'Plugins',
      items: pluginItems,
      loading: pluginHealth.loading,
      error: pluginHealth.error,
    },
    {
      title: 'Channels',
      items: channelItems,
      loading: channelHealth.loading,
      error: channelHealth.error,
    },
    {
      title: 'Paths',
      items: PATHS,
      loading: false,
      error: null,
    },
  ]

  return (
    <div className="settings-panel">
      <SummaryBadge sections={sections} />
      {sections.map(section => (
        <SectionBlock key={section.title} section={section} />
      ))}
    </div>
  )
}
