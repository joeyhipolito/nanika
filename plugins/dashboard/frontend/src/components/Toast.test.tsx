// @vitest-environment happy-dom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, act, fireEvent, cleanup } from '@testing-library/react'
import '@testing-library/jest-dom/vitest'
import { ToastStack } from './Toast'
import { useToast } from '../hooks/useToast'
import { renderHook } from '@testing-library/react'
import { status } from '../colors'

// motion/react AnimatePresence renders children without animation in tests
vi.mock('motion/react', () => ({
  motion: {
    div: ({ children, onClick, className, style }: React.HTMLAttributes<HTMLDivElement> & { layout?: boolean }) => (
      <div className={className} style={style} onClick={onClick} data-testid="toast-motion">
        {children}
      </div>
    ),
  },
  AnimatePresence: ({ children }: { children: React.ReactNode }) => <>{children}</>,
}))

describe('useToast', () => {
  it('starts with an empty queue', () => {
    const { result } = renderHook(() => useToast())
    expect(result.current.toasts).toHaveLength(0)
  })

  it('adds a toast with default type info', () => {
    const { result } = renderHook(() => useToast())
    act(() => { result.current.addToast('hello') })
    expect(result.current.toasts).toHaveLength(1)
    expect(result.current.toasts[0].text).toBe('hello')
    expect(result.current.toasts[0].type).toBe('info')
  })

  it('adds a toast with an explicit type', () => {
    const { result } = renderHook(() => useToast())
    act(() => { result.current.addToast('boom', 'error') })
    expect(result.current.toasts[0].type).toBe('error')
  })

  it('dismisses a toast by id', () => {
    const { result } = renderHook(() => useToast())
    act(() => { result.current.addToast('one') })
    act(() => { result.current.addToast('two') })
    const id = result.current.toasts[0].id
    act(() => { result.current.dismissToast(id) })
    expect(result.current.toasts).toHaveLength(1)
    expect(result.current.toasts[0].text).toBe('two')
  })
})

describe('ToastStack — stacking behavior', () => {
  beforeEach(() => { vi.useFakeTimers() })
  afterEach(() => {
    vi.useRealTimers()
    cleanup()
  })

  function renderStack(count: number, onDismiss = vi.fn()) {
    const toasts = Array.from({ length: count }, (_, i) => ({
      id: `t${i}`,
      text: `Toast ${i + 1}`,
      type: 'info' as const,
    }))
    return { toasts, onDismiss, ...render(<ToastStack toasts={toasts} onDismiss={onDismiss} />) }
  }

  it('renders nothing when toasts array is empty', () => {
    render(<ToastStack toasts={[]} onDismiss={vi.fn()} />)
    expect(screen.queryAllByTestId('toast-motion')).toHaveLength(0)
  })

  it('renders up to 5 toasts', () => {
    renderStack(5)
    expect(screen.getAllByTestId('toast-motion')).toHaveLength(5)
    expect(screen.queryByText(/more/)).not.toBeInTheDocument()
  })

  it('shows at most 5 toasts when 6 are queued', () => {
    renderStack(6)
    const toastEls = screen.getAllByTestId('toast-motion')
    // 5 visible + 1 queue indicator = 6 motion divs
    expect(toastEls).toHaveLength(6)
    expect(screen.getByText('+1 more')).toBeInTheDocument()
  })

  it('queue indicator shows correct count for 10 toasts', () => {
    renderStack(10)
    expect(screen.getByText('+5 more')).toBeInTheDocument()
  })

  it('calls onDismiss when a toast is clicked', () => {
    const onDismiss = vi.fn()
    renderStack(1, onDismiss)
    fireEvent.click(screen.getByText('Toast 1'))
    expect(onDismiss).toHaveBeenCalledWith('t0')
  })

  it('applies colored left border for each toast type', () => {
    const types = [
      { type: 'info' as const, color: status.info },
      { type: 'success' as const, color: status.success },
      { type: 'warning' as const, color: status.warning },
      { type: 'error' as const, color: status.error },
    ]

    for (const { type, color } of types) {
      const { unmount } = render(
        <ToastStack
          toasts={[{ id: 't1', text: 'msg', type }]}
          onDismiss={vi.fn()}
        />
      )
      const el = screen.getByTestId('toast-motion')
      expect(el).toHaveStyle({ borderLeft: `3px solid ${color}` })
      unmount()
    }
  })
})
