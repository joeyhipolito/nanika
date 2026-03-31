export type Message = { role: 'user' | 'assistant'; text: string }

export type MissionStatus = 'in_progress' | 'completed' | 'failed' | 'stalled' | 'cancelled'

// Shape returned by GET /api/orchestrator/missions
export type OrchestratorMission = {
  mission_id: string
  status: MissionStatus | ''
  task: string
  phases?: number
  event_count: number
  size_bytes: number
  modified_at: string
}

// Typed SSE event envelope — mirrors the Go Event struct
export type OrchestratorEvent = {
  id: string
  type: string
  timestamp: string
  sequence: number
  mission_id: string
  phase_id?: string
  worker_id?: string
  data?: Record<string, unknown>
}

// ---- Daemon API types ---------------------------------------------------

export type DAGNode = {
  id: string
  name: string
  persona: string
  skills: string[]
  status: string
  dependencies: string[]
}

export type DAGEdge = {
  from: string
  to: string
}

export type DAGResponse = {
  mission_id: string
  nodes: DAGNode[]
  edges: DAGEdge[]
}

export type PersonaResponse = {
  name: string
  when_to_use: string[]
  expertise: string[]
  color: string
  missions_assigned: number
  success_rate: number
  avg_duration_seconds: number
  currently_active: boolean
}

export type PersonaRecentMission = {
  workspace_id: string
  domain: string
  task: string
  status: string
  started_at: string
  duration_s: number
}

export type PersonaSuccessTrendPoint = {
  week: string
  total: number
  succeeded: number
  success_rate: number
}

export type PersonaDetailResponse = PersonaResponse & {
  recent_missions: PersonaRecentMission[]
  success_trend: PersonaSuccessTrendPoint[]
}

export type DomainStats = {
  total: number
  completed: number
  failed: number
  cancelled: number
}

export type PersonaStats = {
  phases: number
  completed: number
  failed: number
}

export type RecentMission = {
  workspace_id: string
  domain: string
  task: string
  status: string
  started_at: string
  duration_s: number
}

export type MetricsResponse = {
  total: number
  completed: number
  failed: number
  cancelled: number
  avg_duration_s: number
  by_domain: Record<string, DomainStats>
  by_persona: Record<string, PersonaStats>
  recent: RecentMission[]
}

export type FindingScope = {
  kind: string
  value: string
}

export type FindingEvidence = {
  kind: string
  raw: string
  source: string
  captured_at: string
}

export type FindingSeverity = 'critical' | 'high' | 'medium' | 'low' | 'info'

export type Finding = {
  id: string
  ability: string
  category: string
  severity: FindingSeverity
  title: string
  description: string
  scope: FindingScope
  evidence: FindingEvidence[]
  source: string
  found_at: string
  expires_at?: string
  superseded_by?: string
  created_at: string
}

export type ScannerInfo = {
  name: string
  path: string
  size_bytes: number
  mod_time: string
}

export type DashboardPage = 'missions' | 'conversations'

// ---- Chat / Conversations API (GET /api/orchestrator/chat) -----------------

export type BackendMessage = {
  role: 'user' | 'assistant'
  content: string
  created_at: string
}

export type ConversationSummary = {
  id: string
  message_count: number
  last_preview?: string
  last_message_at?: string
  created_at: string
}

export type ConversationDetail = {
  id: string
  messages: BackendMessage[]
  created_at: string
  updated_at: string
}

// ---- Mission detail (GET /api/orchestrator/missions/:id) -------------------

export type MissionDetail = {
  mission_id: string
  status?: string
  task?: string
  phases?: number
  event_count: number
  size_bytes: number
  modified_at: string
}

// ---- Run mission (POST /api/orchestrator/missions/run) ---------------------

export type RunMissionFlags = {
  no_review?: boolean
  no_git?: boolean
  sequential?: boolean
  model?: string
}

export type RunMissionOptions = {
  domain?: string
  flags?: RunMissionFlags
}

// Shape returned by POST /api/orchestrator/missions/run (202 Accepted).
// The real mission_id arrives later via SSE mission.started event.
export type RunMissionResult = {
  request_id: string
  status: 'accepted'
  task: string
}

// ---- Scan result (POST /api/orchestrator/nen/scan) -------------------------

export type ScanResult = {
  output: string
  stderr?: string
  error?: string
}

// ---- Plugin UI convention ---------------------------------------------------
//
// Plugins that supply a custom dashboard view must export a React component
// that satisfies this interface from their `ui/index.tsx` entry point.
// The Vite plugin scans plugin.json for `"ui": true` and auto-generates the
// import map at src/generated/plugin-ui-map.ts.
//
export interface PluginViewProps {
  /** Reflects the dashboard's WebSocket / channel connection state. */
  isConnected?: boolean
}

// ---- Plugin discovery (GET /api/plugins) ------------------------------------

export type PluginCapabilityEntity = {
  pattern: string
  mentionable: boolean
  description: string
}

export type PluginCapabilityCommand = {
  description: string
  args?: string[]
}

export type PluginCapabilities = {
  description: string
  triggers: string[]
  entities?: Record<string, PluginCapabilityEntity>
  commands: Record<string, PluginCapabilityCommand>
}

export type PluginInfo = {
  name: string
  version: string
  api_version: number
  description: string
  binary: string
  provides: string[]
  actions: Record<string, unknown>
  tags?: string[]
  icon?: string
  ui?: boolean
  capabilities?: PluginCapabilities | null
}

// ---- Plugin capabilities (GET /api/orchestrator/plugin-capabilities) --------

export type PluginCapabilitiesEntry = {
  name: string
  capabilities: PluginCapabilities | null
}

// ---- Channel health (GET /api/orchestrator/channels) -----------------------

export type ChannelStatus = {
  name: string
  platform: string
  configured: boolean
  active: boolean
  last_event_sent?: string // ISO timestamp, omitted when zero
  error_count: number
  last_error?: string
}

// ---- Decomposition findings (GET /api/orchestrator/decomposition-findings) -

// ---- Orchestrator/daemon health (GET /api/orchestrator/health) -------------

export type DaemonStatus = {
  name: string
  status: 'running' | 'stopped'
  pid: number
}

export type OrchestratorHealthResponse = {
  daemons: DaemonStatus[]
  timestamp: string
}

// ---- Plugin health (GET /api/orchestrator/plugin-health) -------------------

export type PluginDoctorResult = {
  name: string
  status: 'ok' | 'error' | 'unavailable'
  output?: unknown
  error?: string
}

export type PluginHealthResponse = {
  plugins: PluginDoctorResult[]
  cached_at: string
}

export type DecompositionFinding = {
  id: number
  workspace_id: string
  target_id: string
  finding_type: string
  phase_name: string
  detail: string
  decomp_source: string
  audit_score: number
  created_at: string
}

export type FindingCount = {
  finding_type: string
  count: number
}

export type FindingTrend = {
  period: string
  count: number
}

export type DecompositionFindingsResponse = {
  counts: FindingCount[]
  recent: DecompositionFinding[]
  daily_trends: FindingTrend[]
  weekly_trends: FindingTrend[]
}

// ---- Ryu report (GET /api/orchestrator/ryu-report) ---------------------------

export type RyuMission = {
  id: string
  task: string
  cost: number
  status: string
}

export type RyuReport = {
  today_spend: number
  week_spend: number
  top_missions: RyuMission[]
}

// ---- Ko results (GET /api/orchestrator/ko-results) ---------------------------

export type KoSuite = {
  name: string
  pass_rate: number
  total: number
  passed: number
  failed: number
  last_run_at: string
}

export type KoResults = {
  suites: KoSuite[]
}

// ---- Tracker types (tracker plugin) -----------------------------------------

export type TrackerStatus = 'open' | 'in-progress' | 'done' | 'cancelled'
export type TrackerPriority = 'P0' | 'P1' | 'P2' | 'P3'

export type TrackerItem = {
  id: string
  title: string
  description?: string
  status: TrackerStatus
  priority?: TrackerPriority
  labels?: string // comma-separated
  assignee?: string
  parent_id?: string
  created_at?: string
  updated_at?: string
}

export type TrackerStats = {
  total: number
  by_status: Record<string, number>
  by_priority: Record<string, number>
}
