import type React from 'react'
import { Rocket } from '@solar-icons/react-perf/category/astronomy/LineDuotone'
import { Chart2 } from '@solar-icons/react-perf/category/business/LineDuotone'
import { UsersGroupRounded } from '@solar-icons/react-perf/category/users/LineDuotone'
import { ShieldCheck } from '@solar-icons/react-perf/category/security/LineDuotone'
import { Bell } from '@solar-icons/react-perf/category/notifications/LineDuotone'
import { HeartPulse } from '@solar-icons/react-perf/category/medicine/LineDuotone'
import { ListCheck } from '@solar-icons/react-perf/category/list/LineDuotone'
import { MissionsPanel } from '../components/MissionsPanel'
import { MetricsPanel } from '../components/MetricsPanel'
import { PersonasPanel } from '../components/PersonasPanel'
import { NenPanel } from '../components/NenPanel'
import { EventsPanel } from '../components/EventsPanel'
import { SystemPanel } from '../components/SystemPanel'
import { TrackerPanel, PRIORITY_ORDER } from '../components/TrackerPanel'
import type { OrchestratorMission, Finding, PersonaResponse, MetricsResponse, TrackerItem } from '../types'
import { getTrackerItems } from '../lib/wails'

const BASE = '/api/orchestrator'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface ExtensionItem {
  id: string
  title: string
  subtitle?: string
}

export interface ModuleExtension {
  /** Short label shown as the preview heading (e.g. "Recent Missions") */
  label: string
  fetchItems: () => Promise<ExtensionItem[]>
}

export interface Module {
  id: string
  name: string
  description?: string
  icon: React.ComponentType<{ size?: number | string }>
  keywords: string[]
  // isConnected is optional so panels that don't need it can safely ignore it
  component: React.ComponentType<{ isConnected?: boolean }>
  /** Optional @ extension — shown in conversation mode when user mentions @moduleId */
  extension?: ModuleExtension
}

// ---------------------------------------------------------------------------
// Registry API
// ---------------------------------------------------------------------------

const registry = new Map<string, Module>()

export function registerModule(module: Module): void {
  registry.set(module.id, module)
}

export function listModules(): Module[] {
  return Array.from(registry.values())
}

export function getModule(id: string): Module | undefined {
  return registry.get(id)
}

// ---------------------------------------------------------------------------
// Core module definitions
// ---------------------------------------------------------------------------

registerModule({
  id: 'missions',
  name: 'Missions',
  description: 'Track and manage orchestrator missions',
  icon: Rocket,
  keywords: ['mission', 'missions', 'task', 'tasks', 'orchestrator', 'run', 'agent', 'dag', 'phase', 'cancel', 'running', 'in progress', 'failed', 'completed'],
  component: MissionsPanel,
  extension: {
    label: 'Recent Missions',
    fetchItems: async () => {
      try {
        const res = await fetch(`${BASE}/missions`)
        if (!res.ok) return []
        const data = await res.json() as OrchestratorMission[]
        return data.slice(0, 5).map(m => ({
          id: m.mission_id,
          title: m.task || m.mission_id,
          subtitle: m.status || 'unknown',
        }))
      } catch {
        return []
      }
    },
  },
})

registerModule({
  id: 'metrics',
  name: 'Metrics',
  description: 'Performance stats and mission analytics',
  icon: Chart2,
  keywords: ['metrics', 'stats', 'statistics', 'chart', 'charts', 'analytics', 'performance', 'success rate', 'duration', 'domain', 'persona', 'history'],
  component: MetricsPanel,
})

registerModule({
  id: 'personas',
  name: 'Personas',
  description: 'AI agent roles and active assignments',
  icon: UsersGroupRounded,
  keywords: ['persona', 'personas', 'agent', 'agents', 'role', 'roles', 'engineer', 'architect', 'reviewer', 'active', 'workers'],
  component: PersonasPanel,
  extension: {
    label: 'Personas',
    fetchItems: async () => {
      try {
        const res = await fetch(`${BASE}/personas`)
        if (!res.ok) return []
        const data = await res.json() as PersonaResponse[]
        return data.map(p => ({
          id: p.name,
          title: p.name,
          subtitle: p.currently_active ? 'active' : `${p.missions_assigned} missions`,
        }))
      } catch {
        return []
      }
    },
  },
})

registerModule({
  id: 'nen',
  name: 'Nen',
  description: 'Nen observability — findings, scanners, abilities, cost, and evals',
  icon: ShieldCheck,
  keywords: ['findings', 'nen', 'health', 'scanners', 'cost', 'evals', 'security', 'alerts', 'abilities', 'scan', 'scanner', 'vulnerability', 'severity'],
  component: NenPanel,
  extension: {
    label: 'Recent NEN Findings',
    fetchItems: async () => {
      try {
        const res = await fetch(`${BASE}/findings?limit=5`)
        if (!res.ok) return []
        const data = await res.json() as Finding[]
        return data.map(f => ({
          id: f.id,
          title: f.title,
          subtitle: f.severity,
        }))
      } catch {
        return []
      }
    },
  },
})

registerModule({
  id: 'events',
  name: 'Events',
  description: 'Live mission event stream and logs',
  icon: Bell,
  keywords: ['events', 'event', 'log', 'logs', 'activity', 'stream', 'notifications', 'webhook', 'phase', 'mission event', 'live', 'feed'],
  component: EventsPanel,
  extension: {
    label: 'Recent Activity',
    fetchItems: async () => {
      try {
        const res = await fetch(`${BASE}/metrics`)
        if (!res.ok) return []
        const data = await res.json() as MetricsResponse
        return (data.recent ?? []).slice(0, 5).map(m => ({
          id: m.workspace_id,
          title: m.task,
          subtitle: `${m.status} · ${m.domain}`,
        }))
      } catch {
        return []
      }
    },
  },
})

registerModule({
  id: 'system',
  name: 'System',
  description: 'System health — daemons, Nen abilities, scheduler, plugins, channels, and paths',
  icon: HeartPulse,
  keywords: ['system', 'health', 'status', 'daemon', 'plugins', 'nen', 'scheduler', 'channels', 'paths', 'heartbeat', 'monitor'],
  component: SystemPanel,
})

registerModule({
  id: 'tracker',
  name: 'Tracker',
  description: 'Local issue tracker — tasks, priorities, labels, and dependencies',
  icon: ListCheck,
  keywords: ['tracker', 'issues', 'tasks', 'bugs', 'backlog', 'priority', 'labels', 'kanban'],
  component: TrackerPanel,
  extension: {
    label: 'Open Issues',
    fetchItems: async () => {
      try {
        const items = await getTrackerItems()
        return (items as TrackerItem[])
          .filter(i => i.status === 'open')
          .sort((a, b) => {
            const pa = a.priority ? (PRIORITY_ORDER[a.priority] ?? 99) : 99
            const pb = b.priority ? (PRIORITY_ORDER[b.priority] ?? 99) : 99
            return pa - pb
          })
          .slice(0, 5)
          .map(i => ({
            id: i.id,
            title: i.title,
            subtitle: [i.priority, i.labels].filter(Boolean).join(' · ') || undefined,
          }))
      } catch {
        return []
      }
    },
  },
})


