import { useState, useEffect, useCallback } from 'react'
import { Badge } from './ui/badge'
import { Button } from './ui/button'
import { Card } from './ui/card'
import type { PluginInfo } from '../types'
import { queryPluginStatus, queryPluginItems, pluginAction as wailsPluginAction, listPlugins } from '../lib/wails'

// ---------------------------------------------------------------------------
// PlugIcon — exported for use in the module registry
// ---------------------------------------------------------------------------

export function PlugIcon({ size = 16 }: { size?: number | string }) {
  const s = typeof size === 'string' ? parseInt(size, 10) || 16 : size
  return (
    <svg
      width={s}
      height={s}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M20.5 7.5H17V5a2 2 0 0 0-4 0v2.5h-3.5V9a2 2 0 0 1 0 4v1.5H13V17a2 2 0 0 0 4 0v-2.5h3.5V13a2 2 0 0 1 0-4V7.5Z" />
    </svg>
  )
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface PluginModuleProps {
  pluginName: string
  isConnected?: boolean
  /** Reason the custom UI bundle failed to load — shown as a banner in fallback mode */
  loadError?: string
}

type StatusData = Record<string, unknown>
type ItemRow = Record<string, unknown>

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatValue(v: unknown): string {
  if (v === null || v === undefined) return '—'
  if (typeof v === 'boolean') return v ? 'yes' : 'no'
  if (typeof v === 'string') return v || '—'
  if (typeof v === 'number') return String(v)
  return JSON.stringify(v)
}

/** Extract column names from the first item in a list. */
function inferColumns(items: ItemRow[]): string[] {
  if (!items.length) return []
  return Object.keys(items[0])
}

/** Detect if an action command template requires an item ID placeholder. */
function isPerItemAction(def: unknown): boolean {
  return /<[^>]+>/.test(JSON.stringify(def))
}

/** Normalize action key to a short verb: "action_run" → "run", "query action approve" → "approve". */
function actionVerb(key: string): string {
  if (key.startsWith('action_')) return key.slice(7)
  const parts = key.split(/[\s_]+/)
  return parts[parts.length - 1]
}

function statusColor(key: string, value: unknown): string {
  // Boolean-like values
  if (typeof value === 'boolean') return value ? 'var(--color-success)' : 'var(--color-error)'
  // Common status words
  if (typeof value === 'string') {
    const v = value.toLowerCase()
    if (v === 'running' || v === 'active' || v === 'ok') return 'var(--color-success)'
    if (v === 'stopped' || v === 'error' || v === 'failed') return 'var(--color-error)'
    if (v === 'pending' || v === 'waiting') return 'var(--color-warning)'
  }
  // Numeric count for pending/error keys
  if (typeof value === 'number' && (key.includes('error') || key.includes('fail'))) {
    return value > 0 ? 'var(--color-error)' : 'var(--text-secondary)'
  }
  return 'var(--text-secondary)'
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function StatusGrid({ status }: { status: StatusData }) {
  const entries = Object.entries(status)
  if (!entries.length) return null
  return (
    <dl className="grid grid-cols-2 gap-2 sm:grid-cols-3">
      {entries.map(([key, value]) => (
        <Card
          key={key}
          className="flex flex-col gap-0.5 p-3"
          style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
        >
          <dt className="text-[10px] uppercase tracking-wider" style={{ color: 'var(--text-secondary)' }}>
            {key.replace(/_/g, ' ')}
          </dt>
          <dd
            className="text-sm font-mono font-semibold truncate"
            style={{ color: statusColor(key, value) }}
          >
            {formatValue(value)}
          </dd>
        </Card>
      ))}
    </dl>
  )
}

interface ItemsTableProps {
  items: ItemRow[]
  perItemActions: { verb: string; label: string }[]
  onAction: (verb: string, itemId: string) => Promise<void>
  busy: string | null
}

function ItemsTable({ items, perItemActions, onAction, busy }: ItemsTableProps) {
  const cols = inferColumns(items)
  if (!cols.length) return <p style={{ color: 'var(--text-secondary)' }} className="text-sm">No items.</p>

  // Show actions column only if there are per-item actions
  const showActions = perItemActions.length > 0

  return (
    <div className="overflow-x-auto rounded-lg border" style={{ borderColor: 'var(--pill-border)' }}>
      <table className="w-full text-xs">
        <thead>
          <tr style={{ borderBottom: '1px solid var(--pill-border)', background: 'var(--mic-bg)' }}>
            {cols.map(col => (
              <th
                key={col}
                className="px-3 py-2 text-left font-medium uppercase tracking-wider"
                style={{ color: 'var(--text-secondary)' }}
              >
                {col.replace(/_/g, ' ')}
              </th>
            ))}
            {showActions && (
              <th className="px-3 py-2 text-left font-medium uppercase tracking-wider" style={{ color: 'var(--text-secondary)' }}>
                actions
              </th>
            )}
          </tr>
        </thead>
        <tbody>
          {items.map((item, i) => {
            const itemId = String(item.id ?? item.name ?? i)
            return (
              <tr
                key={itemId}
                style={{ borderBottom: i < items.length - 1 ? '1px solid var(--pill-border)' : undefined }}
              >
                {cols.map(col => (
                  <td key={col} className="px-3 py-2 font-mono" style={{ color: 'var(--text-primary)' }}>
                    <span className="block max-w-[200px] truncate" title={formatValue(item[col])}>
                      {formatValue(item[col])}
                    </span>
                  </td>
                ))}
                {showActions && (
                  <td className="px-3 py-2">
                    <div className="flex gap-1 flex-wrap">
                      {perItemActions.map(({ verb, label }) => (
                        <Button
                          key={verb}
                          size="sm"
                          variant="outline"
                          className="h-6 px-2 text-[10px]"
                          disabled={busy === `${verb}:${itemId}`}
                          onClick={() => onAction(verb, itemId)}
                        >
                          {busy === `${verb}:${itemId}` ? '…' : label}
                        </Button>
                      ))}
                    </div>
                  </td>
                )}
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}

// ---------------------------------------------------------------------------
// PluginModule
// ---------------------------------------------------------------------------

export function PluginModule({ pluginName, loadError }: PluginModuleProps) {
  const [pluginInfo, setPluginInfo] = useState<PluginInfo | null>(null)
  const [status, setStatus] = useState<StatusData | null>(null)
  const [items, setItems] = useState<ItemRow[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState<string | null>(null)
  const [actionFeedback, setActionFeedback] = useState<string | null>(null)

  const loadData = useCallback(async () => {
    try {
      const [plugins, statusData, itemsData] = await Promise.allSettled([
        listPlugins(),
        queryPluginStatus(pluginName),
        queryPluginItems(pluginName),
      ])

      if (plugins.status === 'fulfilled') {
        const found = (plugins.value as PluginInfo[]).find(p => p.name === pluginName)
        if (found) setPluginInfo(found)
      }
      if (statusData.status === 'fulfilled') {
        setStatus(statusData.value as StatusData)
      }
      if (itemsData.status === 'fulfilled') {
        const raw = itemsData.value
        setItems(Array.isArray(raw) ? raw as ItemRow[] : null)
      }
      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setLoading(false)
    }
  }, [pluginName])

  useEffect(() => {
    loadData()
    const id = setInterval(loadData, 30_000)
    return () => clearInterval(id)
  }, [loadData])

  const handleAction = useCallback(async (verb: string, itemId?: string) => {
    const busyKey = itemId ? `${verb}:${itemId}` : verb
    setBusy(busyKey)
    setActionFeedback(null)
    try {
      const data = await wailsPluginAction(pluginName, verb, itemId ?? '')
      const ok = data.ok !== false
      setActionFeedback(ok ? `${verb} succeeded` : `${verb} failed: ${data['error'] ?? 'unknown error'}`)
      if (ok) await loadData()
    } catch (err) {
      setActionFeedback(`${verb} error: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      setBusy(null)
      setTimeout(() => setActionFeedback(null), 4_000)
    }
  }, [pluginName, loadData])

  // Derive global vs per-item actions from plugin info
  const globalActions: { verb: string; label: string }[] = []
  const perItemActions: { verb: string; label: string }[] = []

  if (pluginInfo?.actions) {
    for (const [key, def] of Object.entries(pluginInfo.actions)) {
      const verb = actionVerb(key)
      const label = verb.charAt(0).toUpperCase() + verb.slice(1)
      if (isPerItemAction(def)) {
        perItemActions.push({ verb, label })
      } else {
        globalActions.push({ verb, label })
      }
    }
  }

  if (loading) {
    return (
      <div className="flex flex-col gap-4 p-4 animate-pulse">
        <div className="h-4 rounded w-1/3" style={{ background: 'var(--pill-border)' }} />
        <div className="grid grid-cols-3 gap-2">
          {[1, 2, 3].map(i => (
            <div key={i} className="h-14 rounded" style={{ background: 'var(--mic-bg)' }} />
          ))}
        </div>
        <div className="h-32 rounded" style={{ background: 'var(--mic-bg)' }} />
      </div>
    )
  }

  if (error) {
    return (
      <div className="p-4">
        <Badge variant="destructive" className="mb-2">error</Badge>
        <p className="text-sm font-mono" style={{ color: 'var(--color-error)' }}>{error}</p>
        <Button variant="outline" size="sm" className="mt-3" onClick={loadData}>Retry</Button>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-4 p-4">
      {/* Custom UI load failure banner */}
      {loadError && (
        <div
          className="rounded px-3 py-2 text-xs font-mono"
          style={{
            background: 'color-mix(in srgb, var(--color-error) 10%, transparent)',
            border: '1px solid color-mix(in srgb, var(--color-error) 30%, transparent)',
            color: 'var(--color-error)',
          }}
        >
          <span className="font-semibold">Custom UI failed to load</span>
          <br />
          {loadError}
        </div>
      )}
      {/* Action feedback */}
      {actionFeedback && (
        <p
          className="text-xs px-2 py-1 rounded"
          style={{
            background: actionFeedback.includes('failed') || actionFeedback.includes('error')
              ? 'color-mix(in srgb, var(--color-error) 12%, transparent)'
              : 'color-mix(in srgb, var(--color-success) 12%, transparent)',
            color: actionFeedback.includes('failed') || actionFeedback.includes('error')
              ? 'var(--color-error)'
              : 'var(--color-success)',
          }}
        >
          {actionFeedback}
        </p>
      )}

      {/* Status */}
      {status && Object.keys(status).length > 0 && (
        <section aria-label="Status">
          <h2
            className="text-[10px] uppercase tracking-wider mb-2 font-medium"
            style={{ color: 'var(--text-secondary)' }}
          >
            Status
          </h2>
          <StatusGrid status={status} />
        </section>
      )}

      {/* Items */}
      {items && (
        <section aria-label="Items">
          <div className="flex items-center justify-between mb-2">
            <h2
              className="text-[10px] uppercase tracking-wider font-medium"
              style={{ color: 'var(--text-secondary)' }}
            >
              Items ({items.length})
            </h2>
          </div>
          {items.length === 0
            ? <p className="text-sm" style={{ color: 'var(--text-secondary)' }}>No items.</p>
            : (
              <ItemsTable
                items={items}
                perItemActions={perItemActions}
                onAction={(verb, itemId) => handleAction(verb, itemId)}
                busy={busy}
              />
            )}
        </section>
      )}

      {/* Global actions + refresh */}
      <div className="flex items-center gap-2 flex-wrap">
        {globalActions.map(({ verb, label }) => (
          <Button
            key={verb}
            size="sm"
            variant="outline"
            disabled={busy === verb}
            onClick={() => handleAction(verb)}
          >
            {busy === verb ? '…' : label}
          </Button>
        ))}
        <Button
          size="sm"
          variant="outline"
          disabled={loading}
          onClick={loadData}
          className="ml-auto"
          aria-label="Refresh plugin data"
        >
          Refresh
        </Button>
      </div>
    </div>
  )
}
