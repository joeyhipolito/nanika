import { useState, useEffect, useCallback } from 'react'
import { Badge } from './ui/badge'
import { Button } from './ui/button'
import { Card } from './ui/card'
import type { ChannelStatus } from '../types'
import { getChannelStatus } from '../lib/wails'

// Inline SVG icon for the channels module — no extra dependency.
export function ChannelsIcon({ size = 16 }: { size?: number | string }) {
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
      <path d="M22 16.92v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07A19.5 19.5 0 0 1 4.07 11.5 19.79 19.79 0 0 1 1 2.96 2 2 0 0 1 2.96 1h2.72a2 2 0 0 1 2 1.72c.127.96.36 1.903.7 2.81a2 2 0 0 1-.45 2.11L6.91 8.64a16 16 0 0 0 6.29 6.29l1.01-1.01a2 2 0 0 1 2.11-.45c.907.34 1.85.573 2.81.7A2 2 0 0 1 21 16.19v.73Z" />
    </svg>
  )
}

function formatRelativeTime(iso: string | undefined): string {
  if (!iso) return 'never'
  const ms = Date.now() - new Date(iso).getTime()
  if (ms < 0) return 'just now'
  if (ms < 60_000) return `${Math.floor(ms / 1000)}s ago`
  if (ms < 3_600_000) return `${Math.floor(ms / 60_000)}m ago`
  if (ms < 86_400_000) return `${Math.floor(ms / 3_600_000)}h ago`
  return `${Math.floor(ms / 86_400_000)}d ago`
}

interface ChannelCardProps {
  channel: ChannelStatus
}

function ChannelCard({ channel }: ChannelCardProps) {
  const isHealthy = channel.configured && channel.active && channel.error_count === 0
  const hasError = channel.error_count > 0
  const isUnconfigured = !channel.configured

  const dotColor = isUnconfigured
    ? 'var(--text-secondary)'
    : hasError
      ? 'var(--color-error)'
      : channel.active
        ? 'var(--color-success)'
        : 'var(--text-secondary)'

  const badgeStyle = isUnconfigured
    ? { color: 'var(--text-secondary)', borderColor: 'var(--pill-border)' }
    : hasError
      ? { color: 'var(--color-error)', borderColor: 'var(--color-error)' }
      : channel.active
        ? { color: 'var(--color-success)', borderColor: 'var(--color-success)' }
        : { color: 'var(--text-secondary)', borderColor: 'var(--pill-border)' }

  const statusLabel = isUnconfigured
    ? 'not configured'
    : hasError
      ? 'error'
      : channel.active
        ? 'active'
        : 'inactive'

  return (
    <Card
      className="flex flex-col gap-2 p-3"
      style={{
        background: 'var(--mic-bg)',
        borderColor: isHealthy
          ? 'color-mix(in srgb, var(--color-success) 40%, var(--pill-border))'
          : hasError
            ? 'color-mix(in srgb, var(--color-error) 30%, var(--pill-border))'
            : 'var(--pill-border)',
      }}
    >
      {/* Header row */}
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2 min-w-0">
          {/* Status dot */}
          <span
            className="flex-shrink-0 w-2 h-2 rounded-full"
            style={{ background: dotColor }}
            aria-hidden="true"
          />
          <span
            className="text-sm font-semibold truncate capitalize"
            style={{ color: 'var(--text-primary)' }}
          >
            {channel.name}
          </span>
          <Badge
            variant="outline"
            className="flex-shrink-0 text-[9px] uppercase tracking-wider"
            style={{ color: 'var(--text-secondary)', borderColor: 'var(--pill-border)' }}
          >
            {channel.platform}
          </Badge>
        </div>
        <Badge
          variant="outline"
          className="flex-shrink-0 text-[10px]"
          style={badgeStyle}
        >
          {statusLabel}
        </Badge>
      </div>

      {/* Stats row */}
      <div className="flex gap-3 text-[11px]" style={{ color: 'var(--text-secondary)' }}>
        <span>
          last sent:{' '}
          <span style={{ color: channel.last_event_sent ? 'var(--text-primary)' : 'var(--text-secondary)' }}>
            {formatRelativeTime(channel.last_event_sent)}
          </span>
        </span>
        {channel.error_count > 0 && (
          <span style={{ color: 'var(--color-error)' }}>
            {channel.error_count} error{channel.error_count !== 1 ? 's' : ''}
          </span>
        )}
      </div>

      {/* Last error detail */}
      {channel.last_error && (
        <p
          className="text-[10px] font-mono truncate"
          style={{ color: 'var(--color-error)' }}
          title={channel.last_error}
        >
          {channel.last_error}
        </p>
      )}
    </Card>
  )
}

export function ChannelStatusModule() {
  const [channels, setChannels] = useState<ChannelStatus[] | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      setChannels(await getChannelStatus())
      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    load()
    const id = setInterval(load, 15_000)
    return () => clearInterval(id)
  }, [load])

  if (loading) {
    return (
      <div className="flex flex-col gap-3 p-4 animate-pulse">
        {[1, 2].map(i => (
          <div key={i} className="h-20 rounded-lg" style={{ background: 'var(--mic-bg)' }} />
        ))}
      </div>
    )
  }

  if (error) {
    return (
      <div className="p-4">
        <Badge variant="destructive" className="mb-2">error</Badge>
        <p className="text-sm font-mono" style={{ color: 'var(--color-error)' }}>{error}</p>
        <Button variant="outline" size="sm" className="mt-3" onClick={load}>Retry</Button>
      </div>
    )
  }

  if (!channels || channels.length === 0) {
    return (
      <div className="p-4">
        <p className="text-sm" style={{ color: 'var(--text-secondary)' }}>
          No notification channels registered.
        </p>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-3 p-4">
      <div className="flex items-center justify-between">
        <p className="text-[11px] uppercase tracking-wider font-medium" style={{ color: 'var(--text-secondary)' }}>
          {channels.filter(c => c.active).length} of {channels.length} active
        </p>
        <Button
          variant="ghost"
          size="sm"
          onClick={load}
          aria-label="Refresh channel status"
          className="h-auto py-0.5 px-1.5 text-sm"
          style={{ color: 'var(--text-secondary)' }}
        >
          ↻
        </Button>
      </div>
      {channels.map(ch => (
        <ChannelCard key={ch.name} channel={ch} />
      ))}
    </div>
  )
}
