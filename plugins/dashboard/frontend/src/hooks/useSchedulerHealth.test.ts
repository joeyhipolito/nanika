// @vitest-environment happy-dom
import { describe, it, expect, vi, afterEach } from 'vitest'
import { renderHook, waitFor } from '@testing-library/react'
import { useSchedulerHealth } from './useSchedulerHealth'
import * as wails from '../lib/wails'

vi.mock('../lib/wails', () => ({
  isWails: () => false,
  getSchedulerHealth: vi.fn(),
}))

afterEach(() => {
  vi.restoreAllMocks()
})

describe('useSchedulerHealth', () => {
  it('maps a jobs array into SchedulerJobStatus entries', async () => {
    vi.mocked(wails.getSchedulerHealth).mockResolvedValue({
      status: 'healthy',
      jobs: [
        { name: 'daily-backup', status: 'ok', last_run: '2026-03-30T08:00:00Z' },
        { name: 'health-check', status: 'error', error: 'timeout' },
      ],
    })
    const { result } = renderHook(() => useSchedulerHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))

    expect(result.current.error).toBeNull()
    expect(result.current.jobs).toHaveLength(2)
    expect(result.current.jobs[0]).toMatchObject({ name: 'daily-backup', status: 'healthy' })
    expect(result.current.jobs[1]).toMatchObject({ name: 'health-check', status: 'error', message: 'timeout' })
  })

  it('maps "ok" job status to healthy', async () => {
    vi.mocked(wails.getSchedulerHealth).mockResolvedValue({
      jobs: [{ name: 'ping', status: 'ok' }],
    })
    const { result } = renderHook(() => useSchedulerHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.jobs[0].status).toBe('healthy')
  })

  it('maps "healthy" job status to healthy', async () => {
    vi.mocked(wails.getSchedulerHealth).mockResolvedValue({
      jobs: [{ name: 'ping', status: 'healthy' }],
    })
    const { result } = renderHook(() => useSchedulerHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.jobs[0].status).toBe('healthy')
  })

  it('maps "degraded" job status to degraded', async () => {
    vi.mocked(wails.getSchedulerHealth).mockResolvedValue({
      jobs: [{ name: 'ping', status: 'degraded' }],
    })
    const { result } = renderHook(() => useSchedulerHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.jobs[0].status).toBe('degraded')
  })

  it('falls back to daemon entry when jobs array is absent (daemon down shape)', async () => {
    vi.mocked(wails.getSchedulerHealth).mockResolvedValue({ status: 'healthy' })
    const { result } = renderHook(() => useSchedulerHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.jobs).toHaveLength(1)
    expect(result.current.jobs[0].name).toBe('scheduler daemon')
    expect(result.current.jobs[0].status).toBe('healthy')
  })

  it('falls back daemon entry with error status when overall is error string', async () => {
    vi.mocked(wails.getSchedulerHealth).mockResolvedValue({ status: 'error' })
    const { result } = renderHook(() => useSchedulerHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.jobs[0].status).toBe('error')
  })

  it('falls back daemon entry as healthy when status is empty (daemon not yet started)', async () => {
    vi.mocked(wails.getSchedulerHealth).mockResolvedValue({})
    const { result } = renderHook(() => useSchedulerHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.jobs[0].status).toBe('healthy')
  })

  it('sets error when fetch rejects (daemon not running)', async () => {
    vi.mocked(wails.getSchedulerHealth).mockRejectedValue(new Error('ECONNREFUSED'))
    const { result } = renderHook(() => useSchedulerHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.error).toContain('ECONNREFUSED')
    expect(result.current.jobs).toHaveLength(0)
  })

  it('uses error field as message over last_run', async () => {
    vi.mocked(wails.getSchedulerHealth).mockResolvedValue({
      jobs: [{ name: 'j', status: 'error', error: 'boom', last_run: '2026-01-01' }],
    })
    const { result } = renderHook(() => useSchedulerHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.jobs[0].message).toBe('boom')
  })
})
