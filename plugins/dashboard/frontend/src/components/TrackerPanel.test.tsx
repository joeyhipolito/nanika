// @vitest-environment happy-dom
import { describe, it, expect, vi, afterEach, beforeEach } from 'vitest'
import { render, screen, fireEvent, act, waitFor, cleanup, within } from '@testing-library/react'
import '@testing-library/jest-dom/vitest'
import { TrackerPanel } from './TrackerPanel'
import type { TrackerItem, TrackerStats } from '../types'

// ---------------------------------------------------------------------------
// Mock hooks — prevents real fetch/IPC calls
// ---------------------------------------------------------------------------

const mockUpdateItem = vi.fn()
const mockRefresh = vi.fn()

vi.mock('../hooks/useTracker', () => ({ useTracker: vi.fn() }))
vi.mock('../hooks/useTrackerStats', () => ({ useTrackerStats: vi.fn() }))

import * as useTrackerModule from '../hooks/useTracker'
import * as useTrackerStatsModule from '../hooks/useTrackerStats'

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const ITEMS: TrackerItem[] = [
  { id: 'trk-0001', title: 'Fix critical bug', status: 'open',        priority: 'P0', labels: 'backend,urgent', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-02T00:00:00Z' },
  { id: 'trk-0002', title: 'Add feature X',    status: 'open',        priority: 'P1', labels: 'frontend',       created_at: '2026-01-01T00:00:00Z' },
  { id: 'trk-0003', title: 'Update docs',      status: 'in-progress', priority: 'P2', labels: '',               created_at: '2026-01-01T00:00:00Z' },
  { id: 'trk-0004', title: 'Refactor module',  status: 'done',        priority: 'P3', labels: 'backend',        created_at: '2026-01-01T00:00:00Z' },
  { id: 'trk-0005', title: 'Deploy fix',       status: 'open',        priority: undefined, labels: undefined,   created_at: '2026-01-01T00:00:00Z' },
]

const STATS: TrackerStats = {
  total: 5,
  by_status:   { open: 3, 'in-progress': 1, done: 1, cancelled: 0 },
  by_priority: { P0: 1, P1: 1, P2: 1, P3: 1 },
}

function setupMocks(
  trackerOverrides?: Partial<ReturnType<typeof useTrackerModule.useTracker>>,
  statsOverrides?:   Partial<ReturnType<typeof useTrackerStatsModule.useTrackerStats>>,
) {
  vi.mocked(useTrackerModule.useTracker).mockReturnValue({
    items:      ITEMS,
    loading:    false,
    error:      null,
    refresh:    mockRefresh,
    updateItem: mockUpdateItem,
    ...trackerOverrides,
  })
  vi.mocked(useTrackerStatsModule.useTrackerStats).mockReturnValue({
    stats:   STATS,
    loading: false,
    error:   null,
    refresh: vi.fn(),
    ...statsOverrides,
  })
}

// Helper: get the <aside> sidebar element (implicit role 'complementary')
function getSidebar() {
  return screen.getByRole('complementary')
}

afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
  localStorage.clear()
})

// ---------------------------------------------------------------------------
// 1. Renders with mock data
// ---------------------------------------------------------------------------

describe('TrackerPanel — render', () => {
  it('renders all item titles', () => {
    setupMocks()
    render(<TrackerPanel />)
    for (const item of ITEMS) {
      expect(screen.getByText(item.title)).toBeInTheDocument()
    }
  })

  it('shows correct issue count in the top bar', () => {
    setupMocks()
    render(<TrackerPanel />)
    expect(screen.getByText('5 issues found')).toBeInTheDocument()
  })

  it('renders priority badges for items that have a priority (via aria-label)', () => {
    setupMocks()
    render(<TrackerPanel />)
    // Each issue card has a badge with aria-label "Change priority"
    const priorityBadges = screen.getAllByLabelText(/change priority/i)
    expect(priorityBadges.length).toBe(ITEMS.length)
  })

  it('shows error banner when useTracker reports an error', () => {
    setupMocks({ error: 'tracker: executable file not found in $PATH', items: [] })
    render(<TrackerPanel />)
    expect(screen.getByText(/executable file not found/i)).toBeInTheDocument()
  })

  it('shows empty-state message when no items match', () => {
    setupMocks({ items: [] })
    render(<TrackerPanel />)
    expect(screen.getByText(/no issues match/i)).toBeInTheDocument()
  })
})

// ---------------------------------------------------------------------------
// 2. Filters reduce visible issues
// ---------------------------------------------------------------------------

describe('TrackerPanel — filters', () => {
  it('status filter reduces visible items to matching status only', () => {
    setupMocks()
    render(<TrackerPanel />)

    // Click the 'open' status filter button in the sidebar
    const openBtn = within(getSidebar()).getByRole('button', { name: /^open/i })
    fireEvent.click(openBtn)

    // Only 3 items have status 'open'
    expect(screen.getByText('3 issues found')).toBeInTheDocument()
    expect(screen.queryByText('Update docs')).not.toBeInTheDocument()
    expect(screen.queryByText('Refactor module')).not.toBeInTheDocument()
  })

  it('priority filter shows only items with matching priority', () => {
    setupMocks()
    render(<TrackerPanel />)

    // P0 filter button in the sidebar
    const p0Btn = within(getSidebar()).getByRole('button', { name: /^P0/i })
    fireEvent.click(p0Btn)

    expect(screen.getByText('1 issue found')).toBeInTheDocument()
    expect(screen.getByText('Fix critical bug')).toBeInTheDocument()
  })

  it('clear filters button restores all items', () => {
    setupMocks()
    render(<TrackerPanel />)

    fireEvent.click(within(getSidebar()).getByRole('button', { name: /^open/i }))
    expect(screen.getByText('3 issues found')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: /clear filters/i }))
    expect(screen.getByText('5 issues found')).toBeInTheDocument()
  })

  it('label filter shows only items containing all selected labels', () => {
    setupMocks()
    render(<TrackerPanel />)

    // 'backend' label appears on trk-0001 and trk-0004
    const backendBtn = within(getSidebar()).getByRole('button', { name: /backend/i })
    fireEvent.click(backendBtn)

    expect(screen.getByText('2 issues found')).toBeInTheDocument()
  })

  it('active status filter button has aria-pressed=true', () => {
    setupMocks()
    render(<TrackerPanel />)

    const openBtn = within(getSidebar()).getByRole('button', { name: /^open/i })
    expect(openBtn).toHaveAttribute('aria-pressed', 'false')

    fireEvent.click(openBtn)
    expect(openBtn).toHaveAttribute('aria-pressed', 'true')
  })
})

// ---------------------------------------------------------------------------
// 3. Grouping works for all group-by options
// ---------------------------------------------------------------------------

describe('TrackerPanel — grouping', () => {
  function changeGroupBy(value: string) {
    fireEvent.change(screen.getByRole('combobox'), { target: { value } })
  }

  it('group by none shows all items in a flat list — no group-section buttons', () => {
    setupMocks()
    render(<TrackerPanel />)
    // In 'none' grouping, no buttons have aria-expanded="true" (GroupSection buttons do, IssueCard headers default to false)
    expect(screen.queryAllByRole('button', { expanded: true })).toHaveLength(0)
    expect(screen.getByText('5 issues found')).toBeInTheDocument()
  })

  it('group by priority renders a section for each priority value present', () => {
    setupMocks()
    render(<TrackerPanel />)
    changeGroupBy('priority')

    // GroupSection buttons have aria-expanded; sidebar filter buttons do not.
    // Get all expanded group-section buttons and check their names.
    const groupBtns = screen.getAllByRole('button', { expanded: true })
    const names = groupBtns.map(b => b.textContent ?? '')
    expect(names.some(n => n.startsWith('P0'))).toBe(true)
    expect(names.some(n => n.startsWith('P1'))).toBe(true)
    expect(names.some(n => n.startsWith('P2'))).toBe(true)
    expect(names.some(n => n.startsWith('P3'))).toBe(true)
  })

  it('group by status renders a section for each status value present', () => {
    setupMocks()
    render(<TrackerPanel />)
    changeGroupBy('status')

    const groupBtns = screen.getAllByRole('button', { expanded: true })
    const names = groupBtns.map(b => b.textContent ?? '')
    expect(names.some(n => n.startsWith('open'))).toBe(true)
    expect(names.some(n => n.startsWith('in-progress'))).toBe(true)
    expect(names.some(n => n.startsWith('done'))).toBe(true)
  })

  it('group by label renders sections for backend, frontend, and unlabeled', () => {
    setupMocks()
    render(<TrackerPanel />)
    changeGroupBy('label')

    const groupBtns = screen.getAllByRole('button', { expanded: true })
    const names = groupBtns.map(b => b.textContent ?? '')
    expect(names.some(n => n.includes('backend'))).toBe(true)
    expect(names.some(n => n.includes('frontend'))).toBe(true)
    expect(names.some(n => n.includes('unlabeled'))).toBe(true)
  })

  it('group section collapses on click and hides its items', () => {
    setupMocks()
    render(<TrackerPanel />)
    changeGroupBy('status')

    // GroupSection 'open' button (has aria-expanded, not aria-pressed — unlike sidebar buttons)
    const openGroupBtn = screen.getByRole('button', { expanded: true, name: /^open/i })
    expect(openGroupBtn).toHaveAttribute('aria-expanded', 'true')

    fireEvent.click(openGroupBtn)
    expect(openGroupBtn).toHaveAttribute('aria-expanded', 'false')

    // Items in the 'open' group should no longer be visible
    expect(screen.queryByText('Fix critical bug')).not.toBeInTheDocument()
  })
})

// ---------------------------------------------------------------------------
// 4. Search debounce
// ---------------------------------------------------------------------------

describe('TrackerPanel — search debounce', () => {
  beforeEach(() => {
    vi.useFakeTimers()
  })
  afterEach(() => {
    vi.useRealTimers()
  })

  it('does not filter immediately on keystroke — debounce delay', () => {
    setupMocks()
    render(<TrackerPanel />)

    const input = screen.getByPlaceholderText(/search issues/i)
    fireEvent.change(input, { target: { value: 'critical' } })

    // Before 300ms has elapsed the full list should still render
    expect(screen.getByText('5 issues found')).toBeInTheDocument()
  })

  it('filters items after 300ms debounce elapses', () => {
    setupMocks()
    render(<TrackerPanel />)

    const input = screen.getByPlaceholderText(/search issues/i)
    fireEvent.change(input, { target: { value: 'critical' } })

    // Advance fake timers inside act() so React flushes the state update synchronously
    act(() => { vi.advanceTimersByTime(300) })

    expect(screen.getByText('1 issue found')).toBeInTheDocument()
    expect(screen.getByText('Fix critical bug')).toBeInTheDocument()
    expect(screen.queryByText('Add feature X')).not.toBeInTheDocument()
  })

  it('resets to full list when search is cleared after debounce', () => {
    setupMocks()
    render(<TrackerPanel />)

    const input = screen.getByPlaceholderText(/search issues/i)
    fireEvent.change(input, { target: { value: 'critical' } })
    act(() => { vi.advanceTimersByTime(300) })
    expect(screen.getByText('1 issue found')).toBeInTheDocument()

    fireEvent.change(input, { target: { value: '' } })
    act(() => { vi.advanceTimersByTime(300) })
    expect(screen.getByText('5 issues found')).toBeInTheDocument()
  })

  it('only commits the last keystroke value when typing rapidly', () => {
    setupMocks()
    render(<TrackerPanel />)

    const input = screen.getByPlaceholderText(/search issues/i)

    // Rapid typing: each change resets the 300ms debounce
    fireEvent.change(input, { target: { value: 'ref' } })
    act(() => { vi.advanceTimersByTime(100) })  // 100ms elapsed — debounce not yet fired
    fireEvent.change(input, { target: { value: 'refac' } })
    act(() => { vi.advanceTimersByTime(100) })  // 200ms total since last change — not fired
    fireEvent.change(input, { target: { value: 'refactor' } })
    act(() => { vi.advanceTimersByTime(300) })  // now 300ms since 'refactor' was typed

    expect(screen.getByText('1 issue found')).toBeInTheDocument()
    expect(screen.getByText('Refactor module')).toBeInTheDocument()
  })
})

// ---------------------------------------------------------------------------
// 5. Inline status / priority update calls the API
// ---------------------------------------------------------------------------

describe('TrackerPanel — inline updates', () => {
  it('clicking status badge and selecting an option calls updateItem with new status', async () => {
    setupMocks()
    mockUpdateItem.mockResolvedValue(undefined)
    render(<TrackerPanel />)

    // Badge renders as a <div> with aria-label="Change status"; click the first one.
    // Items are sorted P0→P3, so trk-0001 (P0) is first.
    const statusBadges = screen.getAllByLabelText(/change status/i)
    fireEvent.click(statusBadges[0])

    // InlineDropdown renders <button> elements for each option
    const doneOption = screen.getByText('done', { selector: 'button' })
    fireEvent.click(doneOption)

    await waitFor(() => {
      expect(mockUpdateItem).toHaveBeenCalledOnce()
      expect(mockUpdateItem).toHaveBeenCalledWith('trk-0001', { status: 'done' })
    })
  })

  it('clicking priority badge and selecting an option calls updateItem with new priority', async () => {
    setupMocks()
    mockUpdateItem.mockResolvedValue(undefined)
    render(<TrackerPanel />)

    const priorityBadges = screen.getAllByLabelText(/change priority/i)
    fireEvent.click(priorityBadges[0])

    // P3 button appears in the dropdown
    const p3Option = screen.getByText('P3', { selector: 'button' })
    fireEvent.click(p3Option)

    await waitFor(() => {
      expect(mockUpdateItem).toHaveBeenCalledOnce()
      expect(mockUpdateItem).toHaveBeenCalledWith('trk-0001', { priority: 'P3' })
    })
  })

  it('re-selects current status still calls updateItem (server is authoritative)', async () => {
    setupMocks()
    mockUpdateItem.mockResolvedValue(undefined)
    render(<TrackerPanel />)

    // trk-0001 has status 'open'; select 'open' again from dropdown
    const statusBadges = screen.getAllByLabelText(/change status/i)
    fireEvent.click(statusBadges[0])
    // Find the 'open' option specifically in a dropdown button (text is exactly "open")
    const openOptions = screen.getAllByText('open', { selector: 'button' })
    // The dropdown option button has text exactly "open"; sidebar "open" button has text "open 3"
    const dropdownOpenBtn = openOptions.find(el => el.textContent === 'open')
    expect(dropdownOpenBtn).toBeDefined()
    fireEvent.click(dropdownOpenBtn!)

    await waitFor(() => {
      expect(mockUpdateItem).toHaveBeenCalledWith('trk-0001', { status: 'open' })
    })
  })
})

// ---------------------------------------------------------------------------
// 6. Backend error (tracker binary not found) surfaced gracefully
// ---------------------------------------------------------------------------

describe('TrackerPanel — backend error handling', () => {
  it('renders error banner when tracker binary is unavailable', () => {
    setupMocks({
      items: [],
      error: 'tracker query items --json: exec: "tracker": executable file not found in $PATH',
    })
    render(<TrackerPanel />)
    expect(screen.getByText(/executable file not found/i)).toBeInTheDocument()
  })

  it('shows 0 issues found (not a crash) when items array is empty after error', () => {
    setupMocks({ items: [], error: 'tracker: not found' })
    render(<TrackerPanel />)
    expect(screen.getByText(/tracker: not found/i)).toBeInTheDocument()
    expect(screen.getByText('0 issues found')).toBeInTheDocument()
  })

  it('refresh button is functional after an error', async () => {
    mockRefresh.mockResolvedValue(undefined)
    setupMocks({ items: [], error: 'tracker: not found' })
    render(<TrackerPanel />)

    const refreshBtn = screen.getByRole('button', { name: /refresh/i })
    fireEvent.click(refreshBtn)

    await waitFor(() => {
      expect(mockRefresh).toHaveBeenCalledOnce()
    })
  })
})

// ---------------------------------------------------------------------------
// 7. localStorage persistence across remounts
// ---------------------------------------------------------------------------

describe('TrackerPanel — localStorage persistence', () => {
  it('persists active status filter to localStorage', () => {
    setupMocks()
    render(<TrackerPanel />)

    fireEvent.click(within(getSidebar()).getByRole('button', { name: /^open/i }))

    const stored = JSON.parse(localStorage.getItem('tracker-filters') ?? '{}') as { statuses?: string[] }
    expect(stored.statuses).toContain('open')
  })

  it('persists group-by selection to localStorage', () => {
    setupMocks()
    render(<TrackerPanel />)

    fireEvent.change(screen.getByRole('combobox'), { target: { value: 'priority' } })

    expect(localStorage.getItem('tracker-group')).toBe('priority')
  })

  it('restores persisted filter on remount — status filter and count re-apply', () => {
    localStorage.setItem('tracker-filters', JSON.stringify({ statuses: ['open'], priorities: [], labels: [] }))

    setupMocks()
    render(<TrackerPanel />)

    // Filter was persisted — only 3 'open' items should be shown on first render
    expect(screen.getByText('3 issues found')).toBeInTheDocument()

    // The filter button should show aria-pressed=true
    const openBtn = within(getSidebar()).getByRole('button', { name: /^open/i })
    expect(openBtn).toHaveAttribute('aria-pressed', 'true')
  })

  it('restores persisted group-by on remount — group sections present immediately', () => {
    localStorage.setItem('tracker-group', 'status')

    setupMocks()
    render(<TrackerPanel />)

    // Group sections should be visible immediately (no user interaction needed)
    // GroupSection buttons have aria-expanded
    const groupBtns = screen.getAllByRole('button', { expanded: true })
    expect(groupBtns.length).toBeGreaterThan(0)
    const names = groupBtns.map(b => b.textContent ?? '')
    expect(names.some(n => n.startsWith('open'))).toBe(true)
  })

  it('ignores malformed localStorage entries and uses defaults', () => {
    localStorage.setItem('tracker-filters', '{invalid json}')
    localStorage.setItem('tracker-group', 'invalid-group-value')

    setupMocks()
    expect(() => render(<TrackerPanel />)).not.toThrow()
    expect(screen.getByText('5 issues found')).toBeInTheDocument()
  })
})
