// ---------------------------------------------------------------------------
// Whim shared types — canonical source for ConvItem, Component, ChatEvent,
// ThreadMeta, and StoredMessage.
//
// Component is re-exported from the parent dust types layer so whim
// components don't reach two levels up into ../types.
// ---------------------------------------------------------------------------

export type { Component } from '../types'

// ─── Thread types (mirror chat plugin's Rust Thread/StoredMessage structs) ───

export type ThreadMeta = {
  id: string
  title: string
  created_at: number
  updated_at: number
}

export type StoredMessage = {
  id: string
  thread_id: string
  role: string
  content: string
  created_at: number
}

// ─── Chat event (dust://chat-event payload) ───────────────────────────────────

export type ChatEvent = {
  thread_id: string | null
  event_type: 'data_updated' | 'error'
  data: unknown
}

// ─── ConvItem — whim display union for conversation fixtures ─────────────────
// Moved from mocks/conversation.ts. CompositeCanvas fixtures and the legacy
// mock conversation use this type. LiveCompositeCanvas maps Component[] →
// ConvItem[] for the canvas state (Phase 2 concern).

export type ConvItem =
  | { kind: 'user'; text: string }
  | { kind: 'tool-beat'; summary: string; body?: string }
  | { kind: 'doc-turn'; content: string }
  | { kind: 'commit-summary'; from: string; to: string; additions: number; deletions: number }

// ─── Filesystem types (mirror dust Rust serde shapes for M4 fs commands) ────
// FileEntry / FileMatch match `lib.rs:547` and `lib.rs:556` byte-for-byte.
// SearchKind uses `#[serde(rename_all = "lowercase")]` so the wire form is the
// lowercase string literal — `'filename'` or `'content'`.

export type FileEntry = {
  path: string
  name: string
  /** `'file'` or `'dir'` */
  kind: string
  size: number
}

export type FileMatch = {
  path: string
  line: number | null
  snippet: string
  score: number
}

export type SearchKind = 'filename' | 'content'

// ─── Diff types (mirror Rust serde shapes for M5 diff commands) ─────────────
// HunkLine / Hunk / ChangedFile match lib.rs:723-744 byte-for-byte.

export type HunkLineType = 'add' | 'rem' | 'ctx'

export interface HunkLine {
  type:    HunkLineType
  content: string
}

export interface Hunk {
  id:                 string
  header:             string
  lines:              HunkLine[]
  forcedFailOnApply?: boolean
}

export interface ChangedFile {
  path:      string
  additions: number
  deletions: number
  why:       string
  hunks:     Hunk[]
}

// ─── Mission types (mirror Rust serde shapes from src-tauri/src/mission.rs) ─

export interface MissionSummary {
  id:              string
  slug:            string
  status:          string
  started_at:      string
  last_event_at:   string | null
  phase_count:     number
  phases_done:     number
  phases_failed:   number
}

export interface PhaseSummary {
  phase:    string
  status:   string
  persona:  string
  skills:   string[]
  depends:  string[]
}

// MissionDetail flattens MissionSummary at the top level (#[serde(flatten)] in Rust).
export interface MissionDetail extends MissionSummary {
  phases:          PhaseSummary[]
  workspace_root:  string
  repo_root:       string | null
}

export interface PhaseDetail {
  phase:            string
  status:           string
  persona:          string
  skills:           string[]
  depends:          string[]
  output_files:     string[]
  worker_log_tail:  string[]
}

// Rust uses #[serde(rename_all = "PascalCase")] — wire form is "Approve" | "Reject".
export type GateDecision = 'Approve' | 'Reject'

export interface MissionEventPayload {
  mission_id:  string
  checkpoint:  unknown
}

export interface MissionRunOutputPayload {
  mission_id:  string
  file:        string
  line:        string
}

// ─── M7 wire types — match Rust serde shapes from rail.rs / git.rs / ────────
// commit_summary.rs / notifications.rs.

export interface Project {
  name:      string
  repo_root: string
}

export interface Routine {
  name:     string
  schedule: string
  last_run: string | null
  status:   string
}

export interface PinnedItem {
  id:    string
  title: string
}

export interface FileChange {
  path:   string
  status: string
}

export interface RepoStatus {
  branch:       string
  changes:      FileChange[]
  has_staged:   boolean
  has_unstaged: boolean
}

export interface CommitSummary {
  hash:    string
  author:  string
  date:    string
  message: string
  stats:   string
}

export interface PrMeta {
  number:     number
  title:      string
  state:      string
  author:     string
  created_at: string
  url:        string
}

export interface CanvasNotification {
  id:        string
  title:     string
  message:   string
  timestamp: number
  read:      boolean
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

export type ActionResultEnvelope<T> = { success?: boolean; message?: string; data?: T }

export function unwrapData<T>(res: unknown): T | null {
  if (res == null) return null
  if (typeof res === 'object' && 'success' in (res as object)) {
    return (res as ActionResultEnvelope<T>).data ?? null
  }
  return res as T
}
