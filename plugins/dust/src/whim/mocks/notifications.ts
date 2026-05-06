// Canvas notification fixtures — drives the three notification surfaces in CompositeCanvas

export interface CanvasNotification {
  id:           string
  kind:         'info' | 'success' | 'warn' | 'error'
  surface:      'hud' | 'banner' | 'badge'
  body:         string
  actionLabel?: string
}

export const HUD_NOTIFICATIONS: CanvasNotification[] = [
  { id: 'ctx-window',  kind: 'info', surface: 'hud', body: 'Context 12% used' },
  { id: 'working-for', kind: 'info', surface: 'hud', body: 'working for 3m 42s' },
]

export const BANNER_NOTIFICATION: CanvasNotification = {
  id:      'nen-stale',
  kind:    'warn',
  surface: 'banner',
  body:    'nen-daemon staleness detected — last heartbeat 4m ago. Run nen-daemon restart',
}

export const BADGE_NOTIFICATIONS: CanvasNotification[] = [
  { id: 'nen-badge',  kind: 'warn',  surface: 'badge', body: 'nen stale 4m' },
  { id: 'jobs-badge', kind: 'error', surface: 'badge', body: '2 jobs overdue' },
  { id: 'ch-badge',   kind: 'info',  surface: 'badge', body: '3 messages' },
]

export const ALL_NOTIFICATIONS: CanvasNotification[] = [
  ...HUD_NOTIFICATIONS,
  BANNER_NOTIFICATION,
  ...BADGE_NOTIFICATIONS,
]
