// @vitest-environment happy-dom
import { describe, it, expect, vi, afterEach } from 'vitest'
import { renderHook, act, waitFor } from '@testing-library/react'
import { useMissions, runMissionOnce } from './useMissions'

// ---------------------------------------------------------------------------
// runMissionOnce — standalone fetch wrapper
// ---------------------------------------------------------------------------

describe('runMissionOnce', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('returns request_id / status / task on 202 accepted', async () => {
    const payload = { request_id: '20260324-aabb', status: 'accepted', task: 'do something' }
    vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(
      new Response(JSON.stringify(payload), { status: 202 }),
    )

    const result = await runMissionOnce('do something')
    expect(result).toEqual(payload)
  })

  it('returns null when the server responds non-ok', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(
      new Response('bad request', { status: 400 }),
    )
    const result = await runMissionOnce('task')
    expect(result).toBeNull()
  })

  it('returns null when fetch throws', async () => {
    vi.spyOn(globalThis, 'fetch').mockRejectedValueOnce(new Error('network error'))
    const result = await runMissionOnce('task')
    expect(result).toBeNull()
  })

  it('includes domain and flags in the request body', async () => {
    const payload = { request_id: '20260324-aabb', status: 'accepted', task: 'task' }
    const spy = vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(
      new Response(JSON.stringify(payload), { status: 202 }),
    )
    await runMissionOnce('task', { domain: 'dev', flags: { no_review: true } })

    const [, init] = spy.mock.calls[0]
    const body = JSON.parse((init as RequestInit).body as string)
    expect(body.domain).toBe('dev')
    expect(body.flags.no_review).toBe(true)
  })
})

// ---------------------------------------------------------------------------
// useMissions — hook wrapping runMissionOnce
// ---------------------------------------------------------------------------

describe('useMissions.runMission', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('delegates to the same endpoint and returns request_id', async () => {
    const payload = { request_id: '20260324-ccdd', status: 'accepted', task: 'build it' }
    // First call is fetchMissions (list), subsequent calls are runMission.
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response('[]', { status: 200 })) // initial list fetch
      .mockResolvedValueOnce(new Response(JSON.stringify(payload), { status: 202 }))

    const { result } = renderHook(() => useMissions())

    let out: typeof payload | null = null
    await act(async () => {
      out = await result.current.runMission('build it')
    })

    expect(out).toEqual(payload)
  })
})

// ---------------------------------------------------------------------------
// useMissions — mission list helpers
// ---------------------------------------------------------------------------

describe('useMissions — list', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('fetches missions on mount and exposes them', async () => {
    const missions = [
      { mission_id: 'mid-1', status: 'in_progress', task: 'x', phases: 2, event_count: 5, size_bytes: 100, modified_at: new Date().toISOString() },
    ]
    vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(
      new Response(JSON.stringify(missions), { status: 200 }),
    )

    const { result } = renderHook(() => useMissions())

    await waitFor(() => {
      expect(result.current.missions).toHaveLength(1)
    })
    expect(result.current.missions[0].mission_id).toBe('mid-1')
  })

  it('optimistically sets cancelled status after cancelMission', async () => {
    const missions = [
      { mission_id: 'mid-1', status: 'in_progress', task: 'x', phases: 1, event_count: 1, size_bytes: 50, modified_at: new Date().toISOString() },
    ]
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify(missions), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ action: 'cancelled' }), { status: 200 }))

    const { result } = renderHook(() => useMissions())

    await waitFor(() => expect(result.current.missions).toHaveLength(1))

    await act(async () => {
      await result.current.cancelMission('mid-1')
    })

    expect(result.current.missions[0].status).toBe('cancelled')
  })
})
