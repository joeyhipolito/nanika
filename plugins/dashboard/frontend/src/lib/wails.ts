// Wails runtime bridge — typed wrappers around window.go.main.App.*
//
// window.go is injected by the Wails runtime when running under `wails dev` or
// as a built app. Falls back to the HTTP API when running with standalone Vite
// (e.g. npm run dev without wails) so that local development still works.
//
// DECISION: Keep HTTP fallback so the existing start.sh + Vite workflow continues
// to work without requiring wails to be running.

import type {
  PluginInfo,
  PluginCapabilitiesEntry,
  OrchestratorMission,
  OrchestratorEvent,
  DAGResponse,
  PersonaResponse,
  PersonaDetailResponse,
  MetricsResponse,
  Finding,
  ScannerInfo,
  ScanResult,
  RunMissionResult,
  RunMissionOptions,
  ChannelStatus,
  MissionDetail,
  OrchestratorHealthResponse,
  PluginHealthResponse,
  RyuReport,
  KoResults,
  TrackerItem,
  TrackerStats,
} from '../types'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const wailsApp = (): any => (window as any)?.go?.main?.App

// wailsRuntime returns the Wails v2 runtime (window.runtime), injected by the
// Wails webview. Available for EventsOn/EventsOff/EventsEmit calls.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const wailsRuntime = (): any => (window as any)?.runtime

export function isWails(): boolean {
  return wailsApp() != null
}

// setInteractiveBounds tells the overlay which screen region should receive
// mouse events. x/y are AppKit screen coordinates (origin at bottom-left of
// primary display). Call whenever the palette or a module panel becomes visible.
export function setInteractiveBounds(x: number, y: number, w: number, h: number): void {
  if (isWails()) {
    void wailsApp().EnableInteraction(x, y, w, h)
  }
}

// setFullClickThrough re-enables full pass-through so mouse events go to apps
// beneath the overlay. Call when the palette and all module panels are dismissed.
export function setFullClickThrough(): void {
  if (isWails()) {
    void wailsApp().DisableInteraction()
  }
}

export async function listPlugins(): Promise<PluginInfo[]> {
  if (isWails()) {
    return wailsApp().ListPlugins()
  }
  const res = await fetch('/api/plugins')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<PluginInfo[]>
}

export async function getPluginUIBundle(name: string): Promise<string> {
  console.log(`[wails] getPluginUIBundle called for: ${name}, isWails:`, isWails())
  if (isWails()) {
    const result: string = await wailsApp().GetPluginUIBundle(name)
    console.log(`[wails] GetPluginUIBundle(${name}) returned, length:`, result?.length ?? 0)
    return result
  }
  const res = await fetch(`/api/plugins/${name}/bundle`)
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  const text = await res.text()
  console.log(`[wails] getPluginUIBundle HTTP fallback for ${name}, length:`, text?.length ?? 0)
  return text
}

export async function queryPluginStatus(name: string): Promise<Record<string, unknown>> {
  if (isWails()) {
    const json: string = await wailsApp().QueryPluginStatus(name)
    return JSON.parse(json) as Record<string, unknown>
  }
  const res = await fetch(`/api/plugins/${name}/status`)
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<Record<string, unknown>>
}

export async function queryPluginItems(name: string): Promise<Record<string, unknown>[]> {
  if (isWails()) {
    const json: string = await wailsApp().QueryPluginItems(name)
    const parsed = JSON.parse(json)
    // Some plugins (e.g. tracker) wrap items in an envelope: { items: [...], count: N }
    if (Array.isArray(parsed)) return parsed as Record<string, unknown>[]
    if (parsed && Array.isArray(parsed.items)) return parsed.items as Record<string, unknown>[]
    return []
  }
  const res = await fetch(`/api/plugins/${name}/items`)
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  const body = await res.json() as unknown
  if (Array.isArray(body)) return body as Record<string, unknown>[]
  if (body && typeof body === 'object' && Array.isArray((body as Record<string, unknown>).items)) {
    return (body as { items: Record<string, unknown>[] }).items
  }
  return []
}

export async function pluginAction(
  name: string,
  verb: string,
  id: string,
): Promise<Record<string, unknown>> {
  if (isWails()) {
    const json: string = await wailsApp().PluginAction(name, verb, id)
    return JSON.parse(json) as Record<string, unknown>
  }
  const res = await fetch(`/api/plugins/${name}/action`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ verb, ...(id ? { item_id: id } : {}) }),
  })
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<Record<string, unknown>>
}

// ── Missions ─────────────────────────────────────────────────────────────────

export async function listMissions(): Promise<OrchestratorMission[]> {
  if (isWails()) {
    const json: string = await wailsApp().ListMissions()
    return JSON.parse(json) as OrchestratorMission[]
  }
  const res = await fetch('/api/orchestrator/missions')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<OrchestratorMission[]>
}

export async function getMissionDetail(id: string): Promise<MissionDetail | null> {
  if (isWails()) {
    try {
      const json: string = await wailsApp().GetMissionDetail(id)
      return JSON.parse(json) as MissionDetail
    } catch {
      return null
    }
  }
  const res = await fetch(`/api/orchestrator/missions/${encodeURIComponent(id)}`)
  if (!res.ok) return null
  return res.json() as Promise<MissionDetail>
}

export async function getMissionEvents(id: string): Promise<OrchestratorEvent[]> {
  if (isWails()) {
    try {
      const json: string = await wailsApp().GetMission(id)
      return JSON.parse(json) as OrchestratorEvent[]
    } catch {
      return []
    }
  }
  const res = await fetch(`/api/orchestrator/missions/${encodeURIComponent(id)}/events`)
  if (!res.ok) return []
  return res.json() as Promise<OrchestratorEvent[]>
}

export async function getMissionDAG(id: string): Promise<DAGResponse | null> {
  if (isWails()) {
    try {
      const json: string = await wailsApp().GetMissionDAG(id)
      return JSON.parse(json) as DAGResponse
    } catch {
      return null
    }
  }
  const res = await fetch(`/api/orchestrator/missions/${encodeURIComponent(id)}/dag`)
  if (!res.ok) return null
  return res.json() as Promise<DAGResponse>
}

export async function cancelMission(id: string): Promise<Record<string, unknown>> {
  if (isWails()) {
    const json: string = await wailsApp().CancelMission(id)
    return JSON.parse(json) as Record<string, unknown>
  }
  const res = await fetch(`/api/orchestrator/missions/${encodeURIComponent(id)}/cancel`, {
    method: 'POST',
  })
  return res.json() as Promise<Record<string, unknown>>
}

export async function runMission(
  task: string,
  opts: RunMissionOptions = {},
): Promise<RunMissionResult | null> {
  if (isWails()) {
    try {
      // Pass opts as a JSON string so Go can unmarshal it into runMissionOptions.
      const json: string = await wailsApp().RunMission(task, JSON.stringify(opts))
      return JSON.parse(json) as RunMissionResult
    } catch {
      return null
    }
  }
  try {
    const res = await fetch('/api/orchestrator/missions/run', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ task, ...opts }),
    })
    if (!res.ok) return null
    return res.json() as Promise<RunMissionResult>
  } catch {
    return null
  }
}

// ── Personas ──────────────────────────────────────────────────────────────────

export async function listPersonas(): Promise<PersonaResponse[]> {
  if (isWails()) {
    const json: string = await wailsApp().ListPersonas()
    return JSON.parse(json) as PersonaResponse[]
  }
  const res = await fetch('/api/orchestrator/personas')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<PersonaResponse[]>
}

export async function getPersonaDetail(name: string): Promise<PersonaDetailResponse | null> {
  if (isWails()) {
    try {
      const json: string = await wailsApp().GetPersonaDetail(name)
      return JSON.parse(json) as PersonaDetailResponse
    } catch {
      return null
    }
  }
  const res = await fetch(`/api/orchestrator/personas/${encodeURIComponent(name)}`)
  if (!res.ok) return null
  return res.json() as Promise<PersonaDetailResponse>
}

export async function reloadPersonas(): Promise<boolean> {
  if (isWails()) {
    await wailsApp().ReloadPersonas()
    return true
  }
  const res = await fetch('/api/orchestrator/personas/reload', { method: 'POST' })
  return res.ok
}

// ── Metrics ───────────────────────────────────────────────────────────────────

export async function getMetrics(last = 10): Promise<MetricsResponse | null> {
  if (isWails()) {
    try {
      const json: string = await wailsApp().GetMetrics()
      return JSON.parse(json) as MetricsResponse
    } catch {
      return null
    }
  }
  const res = await fetch(`/api/orchestrator/metrics?last=${last}`)
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<MetricsResponse>
}

// ── Findings ──────────────────────────────────────────────────────────────────

export async function getFindings(qs?: string): Promise<Finding[]> {
  if (isWails()) {
    // Pull findings from the nen plugin directly via the standard query protocol.
    // shu query items returns { items: ComponentResult[], count: N, findings: FindingSummary[] }
    const raw: string = await wailsApp().QueryPluginItems('nen')
    const envelope = JSON.parse(raw) as { findings?: Array<Record<string, unknown>> }
    let findings: Finding[] = (envelope?.findings ?? []).map(f => ({
      id: (f.id as string) ?? '',
      ability: (f.ability as string) ?? '',
      category: (f.category as string) ?? '',
      severity: (f.severity as Finding['severity']) ?? 'info',
      title: (f.title as string) ?? '',
      description: '',
      scope: { kind: '', value: '' },
      evidence: [],
      source: (f.ability as string) ?? '',
      found_at: (f.found_at as string) ?? '',
      created_at: (f.found_at as string) ?? '',
    }))
    // Apply client-side filters (severity, limit) when provided.
    if (qs) {
      const params = new URLSearchParams(qs)
      const severity = params.get('severity')
      const limit = params.get('limit')
      if (severity) findings = findings.filter(f => f.severity === severity)
      if (limit) findings = findings.slice(0, parseInt(limit, 10))
    }
    return findings
  }
  const res = await fetch(`/api/orchestrator/findings${qs ? `?${qs}` : ''}`)
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<Finding[]>
}

// ── Scanners ──────────────────────────────────────────────────────────────────

export async function listScanners(): Promise<ScannerInfo[]> {
  if (isWails()) {
    const json: string = await wailsApp().ListScanners()
    return JSON.parse(json) as ScannerInfo[]
  }
  const res = await fetch('/api/orchestrator/scanners')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<ScannerInfo[]>
}

export async function nenScan(): Promise<ScanResult> {
  if (isWails()) {
    const json: string = await wailsApp().NenScan()
    return JSON.parse(json) as ScanResult
  }
  const res = await fetch('/api/orchestrator/nen/scan', { method: 'POST' })
  return res.json() as Promise<ScanResult>
}

export async function cleanup(): Promise<ScanResult> {
  if (isWails()) {
    const json: string = await wailsApp().Cleanup()
    return JSON.parse(json) as ScanResult
  }
  const res = await fetch('/api/orchestrator/cleanup', { method: 'POST' })
  return res.json() as Promise<ScanResult>
}

// ── Channels ──────────────────────────────────────────────────────────────────

export async function getChannelStatus(): Promise<ChannelStatus[]> {
  if (isWails()) {
    const json: string = await wailsApp().GetChannelStatus()
    return JSON.parse(json) as ChannelStatus[]
  }
  const res = await fetch('/api/orchestrator/channels')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<ChannelStatus[]>
}

// ── Health ────────────────────────────────────────────────────────────────────

export async function checkHealth(): Promise<boolean> {
  if (isWails()) return true
  try {
    const res = await fetch('/api/orchestrator/health')
    return res.ok
  } catch {
    return false
  }
}

export async function getOrchestratorHealth(): Promise<OrchestratorHealthResponse> {
  if (isWails()) {
    const json: string = await wailsApp().OrchestratorHealth()
    return JSON.parse(json) as OrchestratorHealthResponse
  }
  const res = await fetch('/api/orchestrator/health')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<OrchestratorHealthResponse>
}

export async function getNenHealth(): Promise<Record<string, unknown>> {
  if (isWails()) {
    const json: string = await wailsApp().NenHealth()
    return JSON.parse(json) as Record<string, unknown>
  }
  const res = await fetch('/api/orchestrator/nen-health')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<Record<string, unknown>>
}

export async function getSchedulerHealth(): Promise<Record<string, unknown>> {
  if (isWails()) {
    const json: string = await wailsApp().SchedulerHealth()
    return JSON.parse(json) as Record<string, unknown>
  }
  const res = await fetch('/api/orchestrator/scheduler-health')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<Record<string, unknown>>
}

export async function getPluginHealth(): Promise<PluginHealthResponse> {
  if (isWails()) {
    const json: string = await wailsApp().GetPluginHealth()
    return JSON.parse(json) as PluginHealthResponse
  }
  const res = await fetch('/api/orchestrator/plugin-health')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<PluginHealthResponse>
}

export async function getRyuReport(): Promise<RyuReport> {
  if (isWails()) {
    const json: string = await wailsApp().GetRyuReport()
    return JSON.parse(json) as RyuReport
  }
  const res = await fetch('/api/orchestrator/ryu-report')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<RyuReport>
}

export async function getKoResults(): Promise<KoResults> {
  if (isWails()) {
    const json: string = await wailsApp().GetKoResults()
    return JSON.parse(json) as KoResults
  }
  const res = await fetch('/api/orchestrator/ko-results')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<KoResults>
}

// ── Tracker ───────────────────────────────────────────────────────────────────

export async function getTrackerItems(): Promise<TrackerItem[]> {
  if (isWails()) {
    const json: string = await wailsApp().TrackerItems()
    return JSON.parse(json) as TrackerItem[]
  }
  const res = await fetch('/api/orchestrator/tracker-items')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<TrackerItem[]>
}

export async function getTrackerStats(): Promise<TrackerStats> {
  if (isWails()) {
    const json: string = await wailsApp().TrackerStats()
    return JSON.parse(json) as TrackerStats
  }
  const res = await fetch('/api/orchestrator/tracker-stats')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<TrackerStats>
}

export async function updateTrackerItem(req: {
  id: string
  status?: string
  priority?: string
  labels?: string
}): Promise<void> {
  if (isWails()) {
    await wailsApp().TrackerUpdate(JSON.stringify(req))
    return
  }
  const res = await fetch('/api/orchestrator/tracker-update', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(req),
  })
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
}

// ── Plugin capabilities ───────────────────────────────────────────────────────

export async function getPluginCapabilities(): Promise<PluginCapabilitiesEntry[]> {
  if (isWails()) {
    const json: string = await wailsApp().GetPluginCapabilities()
    return JSON.parse(json) as PluginCapabilitiesEntry[]
  }
  const res = await fetch('/api/orchestrator/plugin-capabilities')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<PluginCapabilitiesEntry[]>
}
