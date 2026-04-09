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

export type Component =
  | TextComponent
  | ListComponent
  | MarkdownComponent
  | DividerComponent

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
