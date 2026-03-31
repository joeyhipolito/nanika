// @vitest-environment happy-dom
import { describe, it, expect, vi, afterEach } from 'vitest'
import { render, screen, act, waitFor, cleanup } from '@testing-library/react'
import '@testing-library/jest-dom/vitest'

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

vi.mock('motion/react', () => ({
  motion: {
    div: ({ children, className, style, ...rest }: React.HTMLAttributes<HTMLDivElement>) => (
      <div className={className} style={style} {...rest}>{children}</div>
    ),
  },
  AnimatePresence: ({ children }: { children: React.ReactNode }) => <>{children}</>,
}))

// ---------------------------------------------------------------------------
// Fake EventSource
// ---------------------------------------------------------------------------

class FakeEventSource {
  static instances: FakeEventSource[] = []
  url: string
  onmessage: ((e: MessageEvent) => void) | null = null
  onerror: ((e: Event) => void) | null = null
  private listeners: Record<string, Array<(e: Event) => void>> = {}
  closed = false

  constructor(url: string) {
    this.url = url
    FakeEventSource.instances.push(this)
  }

  addEventListener(type: string, handler: (e: Event) => void) {
    if (!this.listeners[type]) this.listeners[type] = []
    this.listeners[type].push(handler)
  }

  removeEventListener(type: string, handler: (e: Event) => void) {
    this.listeners[type] = (this.listeners[type] ?? []).filter(h => h !== handler)
  }

  dispatchMessage(data: string) {
    this.onmessage?.(new MessageEvent('message', { data }))
  }

  dispatchNamed(type: string) {
    const handlers = this.listeners[type] ?? []
    for (const h of handlers) h(new Event(type))
  }

  close() {
    this.closed = true
  }
}

// ---------------------------------------------------------------------------
// Test-specific tiny component that mirrors MissionDetailView stream logic
// so we can test the stream lifecycle in isolation without exporting internals.
// ---------------------------------------------------------------------------

import { useEffect, useRef, useState } from 'react'
import type { OrchestratorEvent } from '../types'

function StreamProbe({
  missionId,
  isRunning,
  streamMission,
}: {
  missionId: string
  isRunning: boolean
  streamMission: (id: string) => EventSource
}) {
  const [liveEvents, setLiveEvents] = useState<OrchestratorEvent[]>([])
  const [isStreamDone, setIsStreamDone] = useState(false)
  const isStreamDoneRef = useRef(false)

  useEffect(() => {
    isStreamDoneRef.current = false
    setIsStreamDone(false)
    setLiveEvents([])
  }, [missionId])

  useEffect(() => {
    if (!isRunning || isStreamDoneRef.current) return

    const es = streamMission(missionId)

    es.addEventListener('stream:done', () => {
      isStreamDoneRef.current = true
      setIsStreamDone(true)
      es.close()
    })

    es.onmessage = (e: MessageEvent) => {
      try {
        const event = JSON.parse(e.data) as OrchestratorEvent
        setLiveEvents(prev => [...prev, event])
      } catch {
        // ignore
      }
    }

    return () => {
      es.close()
    }
  }, [missionId, isRunning, streamMission])

  const isLive = isRunning && !isStreamDone

  return (
    <div>
      {isLive && <div data-testid="live-badge">live</div>}
      <div data-testid="event-count">{liveEvents.length}</div>
      {isStreamDone && <div data-testid="stream-done">done</div>}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe('MissionDetailView — stream lifecycle', () => {
  afterEach(() => {
    FakeEventSource.instances.length = 0
    cleanup()
    vi.restoreAllMocks()
  })

  function makeStreamMission() {
    return vi.fn((id: string) => new FakeEventSource(`/api/orchestrator/missions/${id}/stream`) as unknown as EventSource)
  }

  it('shows live badge while mission is running', () => {
    render(<StreamProbe missionId="mid-1" isRunning={true} streamMission={makeStreamMission()} />)
    expect(screen.getByTestId('live-badge')).toBeInTheDocument()
  })

  it('does not show live badge when isRunning is false', () => {
    render(<StreamProbe missionId="mid-1" isRunning={false} streamMission={makeStreamMission()} />)
    expect(screen.queryByTestId('live-badge')).not.toBeInTheDocument()
  })

  it('opens exactly one EventSource when isRunning=true', () => {
    render(<StreamProbe missionId="mid-1" isRunning={true} streamMission={makeStreamMission()} />)
    expect(FakeEventSource.instances).toHaveLength(1)
  })

  it('accumulates events delivered via onmessage', async () => {
    render(<StreamProbe missionId="mid-1" isRunning={true} streamMission={makeStreamMission()} />)
    const es = FakeEventSource.instances[0]

    act(() => {
      es.dispatchMessage(JSON.stringify({ id: 'e1', type: 'phase.started', timestamp: new Date().toISOString(), sequence: 1, mission_id: 'mid-1' }))
      es.dispatchMessage(JSON.stringify({ id: 'e2', type: 'phase.completed', timestamp: new Date().toISOString(), sequence: 2, mission_id: 'mid-1' }))
    })

    await waitFor(() => {
      expect(screen.getByTestId('event-count').textContent).toBe('2')
    })
  })

  it('closes the EventSource and hides the live badge on stream:done', async () => {
    render(<StreamProbe missionId="mid-1" isRunning={true} streamMission={makeStreamMission()} />)
    const es = FakeEventSource.instances[0]

    expect(screen.getByTestId('live-badge')).toBeInTheDocument()

    act(() => {
      es.dispatchNamed('stream:done')
    })

    await waitFor(() => {
      expect(screen.queryByTestId('live-badge')).not.toBeInTheDocument()
      expect(screen.getByTestId('stream-done')).toBeInTheDocument()
    })

    expect(es.closed).toBe(true)
  })

  it('does not open a new stream after stream:done even if isRunning stays true', async () => {
    const { rerender } = render(
      <StreamProbe missionId="mid-1" isRunning={true} streamMission={makeStreamMission()} />,
    )
    const es = FakeEventSource.instances[0]

    act(() => { es.dispatchNamed('stream:done') })
    await waitFor(() => expect(screen.getByTestId('stream-done')).toBeInTheDocument())

    // isRunning stays true — parent list hasn't refreshed yet
    rerender(<StreamProbe missionId="mid-1" isRunning={true} streamMission={makeStreamMission()} />)

    // Still only one EventSource ever created
    expect(FakeEventSource.instances).toHaveLength(1)
  })

  it('resets stream state when missionId changes', async () => {
    const { rerender } = render(
      <StreamProbe missionId="mid-1" isRunning={true} streamMission={makeStreamMission()} />,
    )
    const es1 = FakeEventSource.instances[0]
    act(() => { es1.dispatchNamed('stream:done') })
    await waitFor(() => expect(screen.getByTestId('stream-done')).toBeInTheDocument())

    // Switch to a different mission
    rerender(<StreamProbe missionId="mid-2" isRunning={true} streamMission={makeStreamMission()} />)

    await waitFor(() => {
      // stream-done indicator cleared for new mission
      expect(screen.queryByTestId('stream-done')).not.toBeInTheDocument()
      // live badge visible for the new in-progress mission
      expect(screen.getByTestId('live-badge')).toBeInTheDocument()
    })

    // A second EventSource opened for mid-2
    expect(FakeEventSource.instances).toHaveLength(2)
  })
})
