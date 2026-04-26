// ---------------------------------------------------------------------------
// Component — discriminated union matching dust-core Component enum.
//
// Serde serialization: #[serde(tag = "type", rename_all = "snake_case")]
// so variants arrive as { type: "text" | "list" | "markdown" | "divider" }.
// ---------------------------------------------------------------------------

export type TextStyle = {
  bold?: boolean
  italic?: boolean
  underline?: boolean
  color?: { r: number; g: number; b: number }
}

export type ListItem = {
  id: string
  label: string
  description?: string
  icon?: string
  disabled?: boolean
}

export type TextComponent = {
  type: 'text'
  content: string
  style?: TextStyle
}

export type ListComponent = {
  type: 'list'
  items: ListItem[]
  title?: string
}

export type MarkdownComponent = {
  type: 'markdown'
  content: string
}

export type DividerComponent = {
  type: 'divider'
}

export type TableColumn = {
  header: string
  width?: number
}

export type KVPair = {
  label: string
  value: string
  value_color?: { r: number; g: number; b: number }
}

export type BadgeVariant = 'default' | 'outline' | 'filled' | 'subtle'

export type TableComponent = {
  type: 'table'
  columns: TableColumn[]
  rows: string[][]
}

export type KeyValueComponent = {
  type: 'key_value'
  pairs: KVPair[]
}

export type BadgeComponent = {
  type: 'badge'
  label: string
  color?: { r: number; g: number; b: number }
  variant?: BadgeVariant
}

export type ProgressComponent = {
  type: 'progress'
  value: number
  max: number
  label?: string
  color?: { r: number; g: number; b: number }
}

export type FileRefComponent = {
  type: 'file_ref'
  path: string
  /** Optional — wire schema omits when the emitting plugin wants the host
   *  to derive it (`basename(path)`). `RenderFileRef` normalises. */
  basename?: string
  line?: number
}

export type AgentTurnComponent = {
  type: 'agent_turn'
  /** "user" or "assistant" */
  role: string
  /** Message text; may be partial while streaming is true. */
  content: string
  /** True while the turn is still being streamed from the model. */
  streaming?: boolean
  /** Unix timestamp in milliseconds. */
  timestamp?: number
}

export type DiffLineKind = 'context' | 'add' | 'remove'

export type DiffLine = {
  kind: DiffLineKind
  content: string
}

export type Hunk = {
  id: string
  old_start: number
  old_count: number
  new_start: number
  new_count: number
  header?: string
  lines: DiffLine[]
}

export type CodeDiffComponent = {
  type: 'code_diff'
  path: string
  basename: string
  language?: string
  hunks: Hunk[]
}

export type ToolCallStatus = 'pending' | 'running' | 'ok' | 'err'

export type ToolCallBeatComponent = {
  type: 'tool_call_beat'
  tool_use_id: string
  name: string
  params?: unknown
  result?: unknown
  status: ToolCallStatus
  started_ms: number
  finished_ms?: number
}

export type Component =
  | TextComponent
  | ListComponent
  | MarkdownComponent
  | DividerComponent
  | TableComponent
  | KeyValueComponent
  | BadgeComponent
  | ProgressComponent
  | FileRefComponent
  | AgentTurnComponent
  | CodeDiffComponent
  | ToolCallBeatComponent

// ---------------------------------------------------------------------------
// PluginManifest — mirrors dust-core PluginManifest + Capability enum.
// ---------------------------------------------------------------------------

export type Capability =
  | { kind: 'widget'; refresh_secs: number }
  | { kind: 'command'; prefix: string }
  | { kind: 'scheduler' }

export type PluginManifest = {
  name: string
  version: string
  description: string
  capabilities: Capability[]
  icon?: string
}
