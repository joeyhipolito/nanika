// @vitest-environment happy-dom
import { describe, it, expect, vi, afterEach } from 'vitest'
import { renderHook, waitFor } from '@testing-library/react'
import { usePluginHealth } from './usePluginHealth'
import * as wails from '../lib/wails'

vi.mock('../lib/wails', () => ({
  isWails: () => false,
  getPluginHealth: vi.fn(),
}))

afterEach(() => {
  vi.restoreAllMocks()
})

describe('usePluginHealth', () => {
  it('returns plugin list on success', async () => {
    vi.mocked(wails.getPluginHealth).mockResolvedValue({
      plugins: [
        { name: 'scheduler', status: 'ok', output: null, error: '' },
        { name: 'nen', status: 'error', output: null, error: 'daemon not running' },
      ],
      cached_at: '2026-03-31T00:00:00Z',
    })
    const { result } = renderHook(() => usePluginHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))

    expect(result.current.error).toBeNull()
    expect(result.current.plugins).toHaveLength(2)
    expect(result.current.plugins[0].name).toBe('scheduler')
    expect(result.current.plugins[1].status).toBe('error')
    expect(result.current.cachedAt).toBe('2026-03-31T00:00:00Z')
  })

  it('returns empty plugins array when plugins key is missing', async () => {
    vi.mocked(wails.getPluginHealth).mockResolvedValue({
      cached_at: '2026-03-31T00:00:00Z',
    } as never)
    const { result } = renderHook(() => usePluginHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.plugins).toHaveLength(0)
    expect(result.current.error).toBeNull()
  })

  it('sets error when backend is down (fetch rejects)', async () => {
    vi.mocked(wails.getPluginHealth).mockRejectedValue(new Error('HTTP 503'))
    const { result } = renderHook(() => usePluginHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.error).toContain('HTTP 503')
    expect(result.current.plugins).toHaveLength(0)
    expect(result.current.cachedAt).toBeNull()
  })

  it('starts in loading state', () => {
    vi.mocked(wails.getPluginHealth).mockReturnValue(new Promise(() => {}))
    const { result } = renderHook(() => usePluginHealth())
    expect(result.current.loading).toBe(true)
    expect(result.current.plugins).toHaveLength(0)
    expect(result.current.error).toBeNull()
  })

  it('does not update state after unmount (cancellation guard)', async () => {
    let resolve!: (v: ReturnType<typeof wails.getPluginHealth> extends Promise<infer T> ? T : never) => void
    vi.mocked(wails.getPluginHealth).mockReturnValue(
      new Promise(r => { resolve = r }),
    )
    const { result, unmount } = renderHook(() => usePluginHealth())
    expect(result.current.loading).toBe(true)

    unmount()

    // Resolve after unmount — state must NOT update (no setState-on-unmounted error)
    resolve({ plugins: [{ name: 'x', status: 'ok', output: null, error: '' }], cached_at: '2026-03-31T00:00:00Z' })

    // Give microtasks time to flush; hook state should remain loading (never updated)
    await new Promise(r => setTimeout(r, 10))
    expect(result.current.loading).toBe(true)
  })
})
