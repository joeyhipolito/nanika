// @vitest-environment happy-dom
import { describe, it, expect, vi, afterEach } from 'vitest'
import { renderHook, waitFor } from '@testing-library/react'
import { useNenHealth } from './useNenHealth'
import * as wails from '../lib/wails'

vi.mock('../lib/wails', () => ({
  isWails: () => false,
  getNenHealth: vi.fn(),
}))

afterEach(() => {
  vi.restoreAllMocks()
})

describe('useNenHealth', () => {
  it('normalises a healthy object-shaped ability', async () => {
    vi.mocked(wails.getNenHealth).mockResolvedValue({
      gyo: { status: 'healthy', message: 'all good' },
    })
    const { result } = renderHook(() => useNenHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.error).toBeNull()
    expect(result.current.abilities).toHaveLength(1)
    expect(result.current.abilities[0]).toEqual({ name: 'gyo', status: 'healthy', message: 'all good' })
  })

  it('normalises a degraded ability', async () => {
    vi.mocked(wails.getNenHealth).mockResolvedValue({
      zetsu: { status: 'degraded', message: 'slow' },
    })
    const { result } = renderHook(() => useNenHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.abilities[0].status).toBe('degraded')
  })

  it('treats unknown status string as error', async () => {
    vi.mocked(wails.getNenHealth).mockResolvedValue({
      ko: { status: 'broken', message: 'no idea' },
    })
    const { result } = renderHook(() => useNenHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.abilities[0].status).toBe('error')
  })

  it('falls back message to statusRaw when no message key', async () => {
    vi.mocked(wails.getNenHealth).mockResolvedValue({
      en: { status: 'healthy' },
    })
    const { result } = renderHook(() => useNenHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.abilities[0].message).toBe('healthy')
  })

  it('handles scalar truthy value as healthy', async () => {
    vi.mocked(wails.getNenHealth).mockResolvedValue({ ryu: true })
    const { result } = renderHook(() => useNenHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.abilities[0].status).toBe('healthy')
    expect(result.current.abilities[0].message).toBe('true')
  })

  it('handles scalar falsy value as error', async () => {
    vi.mocked(wails.getNenHealth).mockResolvedValue({ ryu: false })
    const { result } = renderHook(() => useNenHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.abilities[0].status).toBe('error')
  })

  it('returns empty abilities when backend returns empty object (daemon down)', async () => {
    vi.mocked(wails.getNenHealth).mockResolvedValue({})
    const { result } = renderHook(() => useNenHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.abilities).toHaveLength(0)
    expect(result.current.error).toBeNull()
  })

  it('sets error when fetch rejects (daemon not running)', async () => {
    vi.mocked(wails.getNenHealth).mockRejectedValue(new Error('connection refused'))
    const { result } = renderHook(() => useNenHealth())
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.error).toContain('connection refused')
    expect(result.current.abilities).toHaveLength(0)
  })

  it('starts in loading state', () => {
    vi.mocked(wails.getNenHealth).mockReturnValue(new Promise(() => {}))
    const { result } = renderHook(() => useNenHealth())
    expect(result.current.loading).toBe(true)
    expect(result.current.error).toBeNull()
    expect(result.current.abilities).toHaveLength(0)
  })
})
