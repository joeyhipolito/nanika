import * as React from 'react'
import { useMemo, useState } from 'react'
import type {
  BadgeVariant,
  CodeDiffComponent,
  Component,
  DiffLine,
  Hunk,
  KVPair,
  ListItem,
  TableColumn,
  TextStyle,
  ToolCallBeatComponent,
  ToolCallStatus,
} from './types'

export const CODE_DIFF_ACCEPT_OP = 'code_diff.accept_hunk'

export type ActionHandler = (action: string, id?: string) => void | Promise<void>
export type OpenFileHandler = (path: string, basename: string, line?: number) => void

// ---------------------------------------------------------------------------
// Variant renderers
// ---------------------------------------------------------------------------

function textColor(style?: TextStyle): React.CSSProperties {
  if (!style?.color) return {}
  const { r, g, b } = style.color
  return { color: `rgb(${r},${g},${b})` }
}

function textClassName(style?: TextStyle): string {
  const parts: string[] = ['text-sm leading-relaxed']
  if (style?.bold) parts.push('font-semibold')
  if (style?.italic) parts.push('italic')
  if (style?.underline) parts.push('underline')
  return parts.join(' ')
}

function RenderText({ content, style }: { content: string; style?: TextStyle }) {
  return (
    <p
      className={textClassName(style)}
      style={{ color: 'var(--text-primary)', ...textColor(style) }}
    >
      {content}
    </p>
  )
}

function RenderListItemRow({ item }: { item: ListItem }) {
  return (
    <div
      className="flex items-start gap-2 rounded px-2 py-1.5"
      style={{ opacity: item.disabled ? 0.4 : 1 }}
    >
      <span
        aria-hidden="true"
        className="mt-[6px] h-1.5 w-1.5 shrink-0 rounded-full"
        style={{ background: 'var(--text-secondary)' }}
      />
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm" style={{ color: 'var(--text-primary)' }}>
          {item.label}
        </p>
        {item.description && (
          <p className="truncate text-[11px]" style={{ color: 'var(--text-secondary)' }}>
            {item.description}
          </p>
        )}
      </div>
    </div>
  )
}

function RenderList({ items, title }: { items: ListItem[]; title?: string }) {
  return (
    <div className="flex flex-col gap-0.5">
      {title && (
        <p
          className="mb-1 px-2 text-[10px] font-semibold uppercase tracking-wider"
          style={{ color: 'var(--text-secondary)' }}
        >
          {title}
        </p>
      )}
      {items.length === 0 ? (
        <p className="px-2 py-3 text-xs" style={{ color: 'var(--text-secondary)' }}>
          No items
        </p>
      ) : (
        items.map(item => <RenderListItemRow key={item.id} item={item} />)
      )}
    </div>
  )
}

function RenderMarkdown({ content }: { content: string }) {
  return (
    <pre
      className="overflow-x-auto whitespace-pre-wrap rounded-md p-3 text-xs leading-relaxed"
      style={{
        background: 'var(--mic-bg)',
        border: '1px solid var(--pill-border)',
        color: 'var(--text-primary)',
        fontFamily: 'inherit',
      }}
    >
      {content}
    </pre>
  )
}

function RenderTable({ columns, rows }: { columns: TableColumn[]; rows: string[][] }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-xs" style={{ borderCollapse: 'collapse' }}>
        <thead>
          <tr style={{ borderBottom: '1px solid var(--border)' }}>
            {columns.map((col, i) => (
              <th
                key={i}
                className="px-2 py-1.5 text-left font-semibold"
                style={{
                  color: 'var(--text-secondary)',
                  width: col.width ? `${col.width}px` : undefined,
                }}
              >
                {col.header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, ri) => (
            <tr
              key={ri}
              style={{ borderBottom: '1px solid var(--border)' }}
              onMouseEnter={e => {
                ;(e.currentTarget as HTMLElement).style.background = 'var(--hover-bg)'
              }}
              onMouseLeave={e => {
                ;(e.currentTarget as HTMLElement).style.background = 'transparent'
              }}
            >
              {row.map((cell, ci) => (
                <td
                  key={ci}
                  className="px-2 py-1.5"
                  style={{ color: 'var(--text-primary)' }}
                >
                  {cell}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

function RenderKeyValue({ pairs }: { pairs: KVPair[] }) {
  return (
    <div className="flex flex-col gap-0.5">
      {pairs.map((pair, i) => (
        <div key={i} className="flex items-baseline gap-2 px-1 py-0.5">
          <span
            className="w-28 shrink-0 truncate text-[11px]"
            style={{ color: 'var(--text-secondary)' }}
          >
            {pair.label}
          </span>
          <span
            className="min-w-0 flex-1 truncate text-xs font-medium"
            style={{
              color: pair.value_color
                ? `rgb(${pair.value_color.r},${pair.value_color.g},${pair.value_color.b})`
                : 'var(--text-primary)',
            }}
          >
            {pair.value}
          </span>
        </div>
      ))}
    </div>
  )
}

function badgeBg(variant: BadgeVariant | undefined, color?: { r: number; g: number; b: number }): React.CSSProperties {
  const rgb = color ? `${color.r},${color.g},${color.b}` : '255,255,255'
  switch (variant) {
    case 'filled':
      return { background: `rgb(${rgb})`, color: '#000' }
    case 'outline':
      return {
        background: 'transparent',
        border: `1px solid rgb(${rgb})`,
        color: `rgb(${rgb})`,
      }
    case 'subtle':
      return { background: `rgba(${rgb},0.15)`, color: `rgb(${rgb})` }
    default:
      return {
        background: 'var(--mic-bg)',
        border: '1px solid var(--pill-border)',
        color: 'var(--text-secondary)',
      }
  }
}

function RenderBadge({
  label,
  color,
  variant,
}: {
  label: string
  color?: { r: number; g: number; b: number }
  variant?: BadgeVariant
}) {
  return (
    <span
      className="inline-block rounded px-1.5 py-0.5 text-[10px] font-mono"
      style={badgeBg(variant, color)}
    >
      {label}
    </span>
  )
}

function RenderProgress({
  value,
  max,
  label,
  color,
}: {
  value: number
  max: number
  label?: string
  color?: { r: number; g: number; b: number }
}) {
  const pct = max > 0 ? Math.min(100, (value / max) * 100) : 0
  const barColor = color
    ? `rgb(${color.r},${color.g},${color.b})`
    : 'var(--color-accent)'

  return (
    <div className="flex flex-col gap-1">
      {label && (
        <div className="flex items-center justify-between text-[11px]">
          <span style={{ color: 'var(--text-secondary)' }}>{label}</span>
          <span style={{ color: 'var(--text-secondary)' }}>{Math.round(pct)}%</span>
        </div>
      )}
      <div
        className="h-1.5 w-full overflow-hidden rounded-full"
        style={{ background: 'var(--mic-bg)' }}
      >
        <div
          className="h-full rounded-full transition-all"
          style={{ width: `${pct}%`, background: barColor }}
        />
      </div>
    </div>
  )
}

// Intaglio brand tokens
const TERRACOTTA = '#DA7757'
const CREAM = '#F2EAD7'

// Blink keyframe injected once per document.
const BLINK_CSS =
  '@keyframes dust-blink{0%,100%{opacity:1}50%{opacity:0}}.dust-caret{animation:dust-blink 1s step-end infinite;user-select:none}'

function BlinkCaret() {
  return (
    <>
      <style>{BLINK_CSS}</style>
      <span className="dust-caret ml-0.5 font-mono" aria-hidden="true">
        ▋
      </span>
    </>
  )
}

function RenderAgentTurn({
  role,
  content,
  streaming,
}: {
  role: string
  content: string
  streaming?: boolean
}) {
  const isUser = role === 'user'

  return (
    <div
      className="flex gap-2"
      style={{ flexDirection: isUser ? 'row-reverse' : 'row' }}
    >
      {/* Intaglio role tick */}
      <span
        data-role={isUser ? 'user-tick' : 'assistant-tick'}
        aria-hidden="true"
        className="mt-1 h-3 w-0.5 shrink-0 rounded-full"
        style={{ background: isUser ? TERRACOTTA : 'var(--text-secondary)' }}
      />
      {/* Bubble */}
      <div
        className="max-w-[85%] rounded-lg px-3 py-2 text-sm leading-relaxed"
        style={
          isUser
            ? {
                background: 'var(--mic-bg)',
                border: `1px solid ${TERRACOTTA}44`,
                color: 'var(--text-primary)',
              }
            : {
                background: CREAM,
                color: '#2C1A0E',
              }
        }
      >
        <pre
          className="whitespace-pre-wrap font-[inherit] text-[inherit]"
          style={{ margin: 0 }}
        >
          {content}
          {streaming && <BlinkCaret />}
        </pre>
      </div>
    </div>
  )
}

// ── FileRef hover + expand support ───────────────────────────────────────────

type FileStat = { size: number; mtime_rfc3339: string; language?: string }
type PreviewLine = { number: number; text: string }
type PreviewSlice = { lines: PreviewLine[]; truncated: boolean }

// Avoid a hard import cycle on the Tauri invoke helper — consume it lazily.
async function tauriInvoke<T>(cmd: string, args: Record<string, unknown>): Promise<T> {
  const mod = await import('@tauri-apps/api/core')
  return mod.invoke<T>(cmd, args)
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

function RenderFileRef({
  path,
  basename,
  line,
  onOpenFile,
}: {
  path: string
  basename: string
  line?: number
  onOpenFile?: OpenFileHandler
}) {
  const [stat, setStat] = React.useState<FileStat | null>(null)
  const [statLoaded, setStatLoaded] = React.useState(false)
  const [hovering, setHovering] = React.useState(false)
  const [expanded, setExpanded] = React.useState(false)
  const [preview, setPreview] = React.useState<PreviewSlice | null>(null)
  const [previewError, setPreviewError] = React.useState<string | null>(null)

  const loadStat = React.useCallback(() => {
    if (statLoaded) return
    setStatLoaded(true)
    tauriInvoke<FileStat>('stat_file', { path })
      .then(s => setStat(s))
      .catch(() => setStat(null))
  }, [path, statLoaded])

  const toggleExpand = React.useCallback(() => {
    const next = !expanded
    setExpanded(next)
    if (next && !preview && !previewError) {
      tauriInvoke<PreviewSlice>('preview_file_slice', { path, line, context: 10 })
        .then(p => setPreview(p))
        .catch((e: unknown) =>
          setPreviewError(e instanceof Error ? e.message : String(e)),
        )
    }
  }, [expanded, preview, previewError, path, line])

  return (
    <span style={{ position: 'relative', display: 'inline-block' }}>
      <span
        onMouseEnter={() => {
          setHovering(true)
          loadStat()
        }}
        onMouseLeave={() => setHovering(false)}
        style={{ display: 'inline-flex', alignItems: 'center', gap: 2 }}
      >
        <button
          type="button"
          className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-mono transition-colors"
          style={{
            background: 'var(--mic-bg)',
            border: '1px solid var(--pill-border)',
            color: 'var(--text-secondary)',
            cursor: onOpenFile ? 'pointer' : 'default',
          }}
          title={`${path}${line ? `:${line}` : ''} — ⌘⇧E to edit`}
          onClick={() => onOpenFile?.(path, basename, line)}
          onKeyDown={e => {
            if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key === 'E') {
              e.preventDefault()
              onOpenFile?.(path, basename, line)
            }
          }}
        >
          <svg aria-hidden="true" width="10" height="10" viewBox="0 0 16 16" fill="none">
            <rect x="2" y="2" width="8" height="12" rx="1.5" stroke="currentColor" strokeWidth="1.5" />
            <line x1="5" y1="6" x2="9" y2="6" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
            <line x1="5" y1="9" x2="8" y2="9" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          </svg>
          <span>{basename}</span>
          {line != null && <span style={{ opacity: 0.6 }}>:{line}</span>}
        </button>
        <button
          type="button"
          onClick={toggleExpand}
          aria-label={expanded ? 'Collapse preview' : 'Expand preview'}
          style={{
            background: 'transparent',
            border: 'none',
            cursor: 'pointer',
            color: 'var(--text-secondary)',
            fontSize: 11,
            padding: '0 2px',
            fontFamily: 'monospace',
          }}
        >
          {expanded ? '▾' : '▸'}
        </button>
      </span>

      {hovering && stat && (
        <span
          style={{
            position: 'absolute',
            top: '100%',
            left: 0,
            marginTop: 4,
            background: '#F2EAD7',
            border: '1px solid #2C1810',
            color: '#2C1810',
            fontFamily: 'monospace',
            fontSize: 10,
            padding: '4px 8px',
            borderRadius: 4,
            whiteSpace: 'nowrap',
            zIndex: 10,
            pointerEvents: 'none',
          }}
        >
          <div>{path}</div>
          <div style={{ opacity: 0.7 }}>
            {formatSize(stat.size)} · {stat.language ?? 'unknown'} · {stat.mtime_rfc3339}
          </div>
        </span>
      )}

      {expanded && (
        <pre
          style={{
            display: 'block',
            background: '#F2EAD7',
            border: '1px solid #2C1810',
            borderRadius: 4,
            padding: 8,
            marginTop: 4,
            fontFamily: 'monospace',
            fontSize: 11,
            lineHeight: 1.4,
            overflowX: 'auto',
          }}
        >
          {previewError ? (
            <span style={{ color: '#DA7757' }}>⚠ {previewError}</span>
          ) : preview ? (
            preview.lines.map(l => (
              <div key={l.number} style={{ whiteSpace: 'pre' }}>
                <span
                  style={{
                    opacity: line === l.number ? 1 : 0.4,
                    color: line === l.number ? '#DA7757' : 'inherit',
                    marginRight: 8,
                    userSelect: 'none',
                  }}
                >
                  {String(l.number).padStart(4, ' ')}
                </span>
                {l.text}
              </div>
            ))
          ) : (
            <span style={{ opacity: 0.6 }}>Loading preview…</span>
          )}
        </pre>
      )}
    </span>
  )
}

// ---------------------------------------------------------------------------
// CodeDiff
// ---------------------------------------------------------------------------

type DiffRow = {
  kind: DiffLine['kind']
  content: string
  oldNum: number | null
  newNum: number | null
}

function hunkRows(hunk: Hunk): DiffRow[] {
  const rows: DiffRow[] = []
  let oldCursor = hunk.old_start
  let newCursor = hunk.new_start
  for (const line of hunk.lines) {
    switch (line.kind) {
      case 'context':
        rows.push({ kind: 'context', content: line.content, oldNum: oldCursor, newNum: newCursor })
        oldCursor += 1
        newCursor += 1
        break
      case 'add':
        rows.push({ kind: 'add', content: line.content, oldNum: null, newNum: newCursor })
        newCursor += 1
        break
      case 'remove':
        rows.push({ kind: 'remove', content: line.content, oldNum: oldCursor, newNum: null })
        oldCursor += 1
        break
    }
  }
  return rows
}

function markerGlyph(kind: DiffLine['kind']): string {
  if (kind === 'add') return '+'
  if (kind === 'remove') return '\u2212' // − (U+2212 minus sign)
  return ' '
}

function lineBg(kind: DiffLine['kind']): string {
  if (kind === 'add') return 'var(--diff-add-bg)'
  if (kind === 'remove') return 'var(--diff-remove-bg)'
  return 'transparent'
}

function lineFg(kind: DiffLine['kind']): string {
  if (kind === 'add') return 'var(--intaglio-terracotta)'
  if (kind === 'remove') return 'var(--color-error)'
  return 'var(--text-primary)'
}

type HunkState = 'pending' | 'accepted' | 'rejected'

function DiffChip({
  label,
  tone,
  onClick,
  disabled,
  testid,
}: {
  label: string
  tone: 'accept' | 'reject' | 'accepted'
  onClick?: () => void
  disabled?: boolean
  testid?: string
}) {
  const palette = (() => {
    switch (tone) {
      case 'accept':
        return {
          color: 'var(--intaglio-terracotta)',
          border: '1px solid var(--intaglio-terracotta)',
          background: 'transparent',
        }
      case 'accepted':
        return {
          color: 'var(--intaglio-cream)',
          border: '1px solid var(--intaglio-terracotta)',
          background: 'rgba(218, 119, 87, 0.18)',
        }
      case 'reject':
        return {
          color: 'var(--text-secondary)',
          border: '1px solid var(--pill-border)',
          background: 'transparent',
        }
    }
  })()
  return (
    <button
      type="button"
      className="rounded-sm px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider transition-colors"
      style={{
        ...palette,
        cursor: disabled ? 'default' : 'pointer',
        opacity: disabled ? 0.65 : 1,
      }}
      onClick={onClick}
      disabled={disabled}
      data-testid={testid}
    >
      {label}
    </button>
  )
}

function RenderHunk({
  diffPath,
  hunk,
  state,
  onAccept,
  onReject,
}: {
  diffPath: string
  hunk: Hunk
  state: HunkState
  onAccept: () => void
  onReject: () => void
}) {
  const rows = useMemo(() => hunkRows(hunk), [hunk])
  const headerRange = `@@ -${hunk.old_start},${hunk.old_count} +${hunk.new_start},${hunk.new_count} @@`
  const isAccepted = state === 'accepted'
  return (
    <div
      className="flex flex-col"
      data-role="code-diff-hunk"
      data-hunk-id={hunk.id}
      data-hunk-state={state}
    >
      {/* Hunk header bar */}
      <div
        className="flex items-center justify-between gap-2 px-3 py-1 font-mono text-[11px]"
        style={{ background: 'var(--bg-elevated)' }}
      >
        <div className="min-w-0 truncate">
          <span style={{ color: 'var(--text-secondary)' }}>{headerRange}</span>
          {hunk.header && (
            <span style={{ color: 'var(--text-primary)' }}>{'  '}{hunk.header}</span>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-1" aria-label={`hunk-${hunk.id}-actions`}>
          {isAccepted ? (
            <DiffChip
              label="Accepted"
              tone="accepted"
              disabled
              testid={`chip-accepted-${hunk.id}`}
            />
          ) : (
            <>
              <DiffChip
                label="Accept"
                tone="accept"
                onClick={onAccept}
                disabled={state === 'rejected'}
                testid={`chip-accept-${hunk.id}`}
              />
              <DiffChip
                label="Reject"
                tone="reject"
                onClick={onReject}
                disabled={state === 'rejected'}
                testid={`chip-reject-${hunk.id}`}
              />
            </>
          )}
        </div>
      </div>
      {/* Hunk body */}
      <div
        role="table"
        aria-label={`diff-hunk-${hunk.id}`}
        data-role="diff-hunk-grid"
        data-path={diffPath}
        className="overflow-hidden font-mono text-[11px]"
        style={{ display: 'grid', gridTemplateColumns: '3.5rem 3.5rem 1.25rem 1fr' }}
      >
        {rows.map((row, i) => (
          <DiffRowCells key={i} row={row} />
        ))}
      </div>
    </div>
  )
}

function DiffRowCells({ row }: { row: DiffRow }) {
  const bg = lineBg(row.kind)
  const fg = lineFg(row.kind)
  return (
    <>
      <span
        role="cell"
        data-role="diff-gutter-old"
        className="select-none px-1 text-right tabular-nums"
        style={{ color: 'var(--diff-gutter-fg)', background: bg }}
      >
        {row.oldNum ?? ''}
      </span>
      <span
        role="cell"
        data-role="diff-gutter-new"
        className="select-none px-1 text-right tabular-nums"
        style={{ color: 'var(--diff-gutter-fg)', background: bg }}
      >
        {row.newNum ?? ''}
      </span>
      <span
        role="cell"
        data-role="diff-marker"
        className="select-none px-1 text-center"
        style={{ color: fg, background: bg }}
      >
        {markerGlyph(row.kind)}
      </span>
      <span
        role="cell"
        data-role="diff-content"
        className="overflow-x-auto whitespace-pre pr-2"
        style={{ color: fg, background: bg }}
      >
        {row.content}
      </span>
    </>
  )
}

function RenderCodeDiff({
  diff,
  onAction,
}: {
  diff: CodeDiffComponent
  onAction?: ActionHandler
}) {
  const [states, setStates] = useState<Record<string, HunkState>>({})

  const pendingIds = diff.hunks
    .map(h => h.id)
    .filter(id => (states[id] ?? 'pending') === 'pending')

  const handleAccept = (hunk: Hunk) => {
    setStates(prev => ({ ...prev, [hunk.id]: 'accepted' }))
    void onAction?.(CODE_DIFF_ACCEPT_OP, hunk.id)
  }
  const handleReject = (hunk: Hunk) => {
    setStates(prev => ({ ...prev, [hunk.id]: 'rejected' }))
  }

  const acceptAll = () => {
    for (const id of pendingIds) {
      const hunk = diff.hunks.find(h => h.id === id)
      if (hunk) handleAccept(hunk)
    }
  }
  const rejectAll = () => {
    setStates(prev => {
      const next = { ...prev }
      for (const id of pendingIds) next[id] = 'rejected'
      return next
    })
  }

  return (
    <div
      className="flex flex-col gap-2"
      data-role="code-diff"
      data-path={diff.path}
    >
      {/* File header chip */}
      <div
        className="flex flex-col gap-0.5 rounded px-3 py-2"
        style={{
          background: 'var(--mic-bg)',
          border: '1px solid var(--pill-border)',
        }}
      >
        <div className="flex items-center justify-between gap-2">
          <div className="flex items-center gap-2 min-w-0">
            <span
              className="font-semibold"
              style={{ color: 'var(--intaglio-cream)' }}
              data-role="code-diff-basename"
            >
              {diff.basename}
            </span>
          </div>
          <div className="flex items-center gap-2 text-[11px] shrink-0">
            <span style={{ color: 'var(--text-secondary)' }}>
              {diff.language ? `${diff.language} · ` : ''}
              {diff.hunks.length} {diff.hunks.length === 1 ? 'hunk' : 'hunks'}
            </span>
            <DiffChip
              label="Accept all"
              tone="accept"
              onClick={acceptAll}
              disabled={pendingIds.length === 0}
              testid="chip-accept-all"
            />
            <DiffChip
              label="Reject all"
              tone="reject"
              onClick={rejectAll}
              disabled={pendingIds.length === 0}
              testid="chip-reject-all"
            />
          </div>
        </div>
        <p
          className="truncate text-[11px]"
          style={{ color: 'var(--text-secondary)' }}
          data-role="code-diff-path"
        >
          {diff.path}
        </p>
      </div>
      {/* Hunks */}
      {diff.hunks.length === 0 ? (
        <p className="px-2 py-3 text-xs" style={{ color: 'var(--text-secondary)' }}>
          No hunks returned by plugin.
        </p>
      ) : (
        diff.hunks.map(hunk => (
          <RenderHunk
            key={hunk.id}
            diffPath={diff.path}
            hunk={hunk}
            state={states[hunk.id] ?? 'pending'}
            onAccept={() => handleAccept(hunk)}
            onReject={() => handleReject(hunk)}
          />
        ))
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// ToolCallBeat
// ---------------------------------------------------------------------------

function statusDotColor(status: ToolCallStatus): string {
  switch (status) {
    case 'running':
    case 'pending':
      return TERRACOTTA
    case 'ok':
      return 'var(--text-secondary)'
    case 'err':
      return 'var(--color-error)'
  }
}

function statusLabel(status: ToolCallStatus): string {
  switch (status) {
    case 'pending':
      return 'pending'
    case 'running':
      return 'running'
    case 'ok':
      return 'ok'
    case 'err':
      return 'error'
  }
}

function formatElapsed(started: number, finished?: number): string {
  const end = finished ?? Date.now()
  const ms = Math.max(0, end - started)
  if (ms < 1000) return `${ms}ms`
  return `${(ms / 1000).toFixed(1)}s`
}

function stringifyJson(value: unknown): string {
  if (value == null) return ''
  if (typeof value === 'string') return value
  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return String(value)
  }
}

function paramsAsPairs(params: unknown): KVPair[] | null {
  if (!params || typeof params !== 'object' || Array.isArray(params)) return null
  const entries = Object.entries(params as Record<string, unknown>)
  if (entries.length === 0) return null
  return entries.map(([k, v]) => ({
    label: k,
    value: typeof v === 'string' ? v : stringifyJson(v),
  }))
}

function RenderToolCallBeat({ beat }: { beat: ToolCallBeatComponent }) {
  const [expanded, setExpanded] = useState(false)
  const dot = statusDotColor(beat.status)
  const running = beat.status === 'running' || beat.status === 'pending'
  const elapsed = formatElapsed(beat.started_ms, beat.finished_ms)
  const pairs = paramsAsPairs(beat.params)
  const resultText = beat.result != null ? stringifyJson(beat.result) : ''
  const resultIsError = beat.status === 'err'

  return (
    <div
      data-role="tool-call-beat"
      data-tool-use-id={beat.tool_use_id}
      data-status={beat.status}
      className="flex flex-col gap-1 rounded px-2 py-1"
      style={{
        background: 'var(--mic-bg)',
        border: '1px solid var(--pill-border)',
      }}
    >
      <button
        type="button"
        aria-expanded={expanded}
        aria-label={`tool ${beat.name} ${statusLabel(beat.status)}`}
        onClick={() => setExpanded(v => !v)}
        className="flex items-center gap-2 text-left text-xs"
        style={{ background: 'transparent', cursor: 'pointer', padding: 0 }}
      >
        <span
          aria-hidden="true"
          data-role="tool-call-dot"
          className="h-2 w-2 shrink-0 rounded-full"
          style={{
            background: dot,
            animation: running ? 'dust-blink 1s step-end infinite' : undefined,
          }}
        />
        <span
          className="font-mono font-semibold"
          style={{ color: 'var(--text-primary)' }}
        >
          {beat.name}
        </span>
        <span
          className="ml-auto font-mono text-[10px]"
          style={{ color: 'var(--text-secondary)' }}
        >
          {elapsed}
        </span>
        <span aria-hidden="true" style={{ color: 'var(--text-secondary)' }}>
          {expanded ? '▾' : '▸'}
        </span>
      </button>
      {running && <style>{BLINK_CSS}</style>}
      {expanded && (
        <div className="flex flex-col gap-2 pt-1">
          {pairs ? (
            <div data-role="tool-call-params">
              <RenderKeyValue pairs={pairs} />
            </div>
          ) : beat.params != null ? (
            <pre
              data-role="tool-call-params"
              className="overflow-x-auto whitespace-pre-wrap rounded p-2 text-[11px]"
              style={{ background: 'var(--bg-elevated)', color: 'var(--text-primary)' }}
            >
              {stringifyJson(beat.params)}
            </pre>
          ) : null}
          {resultText && (
            <pre
              data-role="tool-call-result"
              className="overflow-x-auto whitespace-pre-wrap rounded p-2 text-[11px]"
              style={{
                background: resultIsError
                  ? 'rgba(220, 38, 38, 0.12)'
                  : 'var(--bg-elevated)',
                color: resultIsError ? 'var(--color-error)' : 'var(--text-primary)',
              }}
            >
              {resultText}
            </pre>
          )}
        </div>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Single-component dispatcher
// ---------------------------------------------------------------------------

function RenderOne({ component, onAction, onOpenFile }: { component: Component; onAction?: ActionHandler; onOpenFile?: OpenFileHandler }) {
  switch (component.type) {
    case 'text':
      return <RenderText content={component.content} style={component.style} />
    case 'list':
      return <RenderList items={component.items} title={component.title} />
    case 'markdown':
      return <RenderMarkdown content={component.content} />
    case 'divider':
      return <hr style={{ borderColor: 'var(--border)', margin: '8px 0' }} />
    case 'table':
      return <RenderTable columns={component.columns} rows={component.rows} />
    case 'key_value':
      return <RenderKeyValue pairs={component.pairs} />
    case 'badge':
      return (
        <RenderBadge label={component.label} color={component.color} variant={component.variant} />
      )
    case 'progress':
      return (
        <RenderProgress
          value={component.value}
          max={component.max}
          label={component.label}
          color={component.color}
        />
      )
    case 'file_ref':
      return (
        <RenderFileRef
          path={component.path}
          basename={component.basename ?? component.path.split('/').pop() ?? component.path}
          line={component.line}
          onOpenFile={onOpenFile}
        />
      )
    case 'agent_turn':
      return (
        <RenderAgentTurn
          role={component.role}
          content={component.content}
          streaming={component.streaming}
        />
      )
    case 'code_diff':
      return <RenderCodeDiff diff={component} onAction={onAction} />
    case 'tool_call_beat':
      return <RenderToolCallBeat beat={component} />
    default:
      // Unknown component from an older or newer host — render nothing.
      return null
  }
}

// ---------------------------------------------------------------------------
// Top-level dispatcher — renders a Vec<Component> tree
// ---------------------------------------------------------------------------

export type ComponentRendererProps = {
  components: Component[]
  onAction?: ActionHandler
  onOpenFile?: OpenFileHandler
}

export function ComponentRenderer({ components, onAction, onOpenFile }: ComponentRendererProps) {
  return (
    <div className="flex flex-col gap-2">
      {components.map((c, i) => (
        <RenderOne key={i} component={c} onAction={onAction} onOpenFile={onOpenFile} />
      ))}
    </div>
  )
}
