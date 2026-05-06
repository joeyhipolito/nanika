// ─── Scene spec — discriminated union by kind ─────────────────────────────────

export type PillSceneSpec = {
  kind: 'pill'
  state: Record<string, unknown>
}

export type PaletteSceneSpec = {
  kind: 'palette'
  state: Record<string, unknown>
}

export type PaletteDetailSceneSpec = {
  kind: 'palette-detail'
  state: Record<string, unknown>
}

export type CanvasSceneSpec = {
  kind: 'canvas'
  state: Record<string, unknown>
}

export type CanvasDiffSceneSpec = {
  kind: 'canvas-diff'
  state: Record<string, unknown>
}

export type OverviewSceneSpec = {
  kind: 'overview'
  state: Record<string, unknown>
}

export type SceneSpec =
  | PillSceneSpec
  | PaletteSceneSpec
  | PaletteDetailSceneSpec
  | CanvasSceneSpec
  | CanvasDiffSceneSpec
  | OverviewSceneSpec

// ─── Overlay kinds ────────────────────────────────────────────────────────────

export type OverlayKind =
  | 'action-palette'
  | 'fan-view'
  | 'pill-drag'
  | 'voice-overlay'

// ─── Chapter identifiers ──────────────────────────────────────────────────────

export type TourChapter =
  | 'at-rest'
  | 'summon'
  | 'compose-voice'
  | 'canvas'
  | 'plugins'
  | 'ambient-error'
  | 'wrap'

// ─── Key entry ────────────────────────────────────────────────────────────────

/** Owned keys (`→ ← space Esc j k`) are intercepted; demonstrated keys are visual-only. */
export type OwnedKey = '→' | '←' | 'space' | 'Esc' | 'j' | 'k'

export interface TourKeyEntry {
  key: string
  label: string
  /** When true, this key is intercepted by the Tour (preventDefault). When false, painted as inert keycap. */
  owned: boolean
}

// ─── Tour step ────────────────────────────────────────────────────────────────

export interface TourStep {
  /** Unique stable identifier (e.g. `c1-opener`). Used for deep-linking and `data-tour-step`. */
  id: string
  /** Chapter this step belongs to. */
  chapter: TourChapter
  /** Short working title shown in the callout heading. */
  title: string
  /** One-paragraph description rendered in `TourCallout`. */
  body: string
  /** Key hints for the callout row. Mix of owned (intercepted) and demonstrated (visual-only). */
  keys: TourKeyEntry[]
  /** Scene to drive into the substrate when this step is active. */
  scene: SceneSpec
  /** CSS selector resolving to a DOM element with `data-tour-anchor`. `SpotlightRing` uses this. */
  spotlightTarget: string
  /** Auto-advance duration in ms (default 5000, max 7000). */
  durationMs?: number
}

// ─── Tour scene (host callback shape) ────────────────────────────────────────

export interface TourScene {
  kind: SceneSpec['kind']
  /** Partial state merged into PaletteShell props or DEFAULT_COMPOSITE_CANVAS_STATE. */
  state: Record<string, unknown>
}
