// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { render, fireEvent, cleanup } from '@testing-library/react'
import '@testing-library/jest-dom/vitest'
import { ActivityHeatmap } from './ActivityHeatmap'
import type { RecentMission } from '../types'

// Ensure DOM is wiped between each test — prevents cross-test query contamination.
afterEach(cleanup)

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeMission(partial: Partial<RecentMission>): RecentMission {
  return {
    workspace_id: 'ws1',
    domain: 'dev',
    task: 'test task',
    status: 'completed',
    started_at: new Date().toISOString(),
    duration_s: 60,
    ...partial,
  }
}

/** Returns an ISO timestamp for a day offset from today (negative = past). */
function dayISO(offsetDays = 0): string {
  const d = new Date()
  d.setDate(d.getDate() + offsetDays)
  d.setHours(12, 0, 0, 0)
  return d.toISOString()
}

/**
 * Returns the YYYY-MM-DD string that the component derives from dayISO(offsetDays)
 * by slicing the first 10 chars. Using the same derivation avoids UTC/local
 * timezone mismatches on machines with large UTC offsets.
 */
function dayDateStr(offsetDays = 0): string {
  return dayISO(offsetDays).slice(0, 10)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe('ActivityHeatmap', () => {
  describe('renders with mock data', () => {
    it('renders the heatmap wrapper with summary bar', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO() }),
        makeMission({ status: 'failed', started_at: dayISO(-1) }),
      ]} />)
      expect(container.querySelector('.heatmap-wrapper')).toBeInTheDocument()
      expect(container.querySelector('.heatmap-summary')).toBeInTheDocument()
    })

    it('renders grid cells for each day in the 6-month range', () => {
      const { container } = render(<ActivityHeatmap recent={[]} />)
      const cells = container.querySelectorAll('.heatmap-cell')
      // 6 months spans ~183 days; grid rounds up to nearest week column (7 * numWeeks)
      // numWeeks = ceil(~183/7) ≈ 27 → 7*27 = 189 cells, allow ±14 for boundary alignment
      expect(cells.length).toBeGreaterThan(170)
      expect(cells.length).toBeLessThan(210)
    })

    it('renders month labels row', () => {
      const { container } = render(<ActivityHeatmap recent={[]} />)
      expect(container.querySelector('.heatmap-month-row')).toBeInTheDocument()
      const labels = container.querySelectorAll('.heatmap-month')
      // 6-month span crosses at least 5 distinct month boundaries
      expect(labels.length).toBeGreaterThanOrEqual(5)
    })

    it('renders day-of-week gutter labels', () => {
      const { container } = render(<ActivityHeatmap recent={[]} />)
      const gutter = container.querySelector('.heatmap-gutter')
      expect(gutter).toBeInTheDocument()
    })
  })

  describe('cell coloring by status', () => {
    it('applies empty class to days with no missions', () => {
      const { container } = render(<ActivityHeatmap recent={[]} />)
      const emptyCells = container.querySelectorAll('.heatmap-cell--empty')
      expect(emptyCells.length).toBeGreaterThan(0)
    })

    it('applies fail class to a day with at least one failed mission', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'failed', started_at: dayISO(-3) }),
      ]} />)
      expect(container.querySelectorAll('.heatmap-cell--fail').length).toBe(1)
    })

    it('fail class takes priority over warn when both statuses exist on the same day', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'failed', started_at: dayISO(-5) }),
        makeMission({ status: 'cancelled', started_at: dayISO(-5) }),
      ]} />)
      expect(container.querySelectorAll('.heatmap-cell--fail').length).toBe(1)
      expect(container.querySelectorAll('.heatmap-cell--warn').length).toBe(0)
    })

    it('applies warn class to a day with only cancelled/stalled missions', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'cancelled', started_at: dayISO(-4) }),
        makeMission({ status: 'stalled', started_at: dayISO(-4) }),
      ]} />)
      expect(container.querySelectorAll('.heatmap-cell--warn').length).toBe(1)
    })

    it('applies green-lo class for 1 completed mission with no failures', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-2) }),
      ]} />)
      expect(container.querySelectorAll('.heatmap-cell--green-lo').length).toBe(1)
    })

    it('applies green-lo class for 2 completed missions with no failures', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-2) }),
        makeMission({ status: 'completed', started_at: dayISO(-2) }),
      ]} />)
      expect(container.querySelectorAll('.heatmap-cell--green-lo').length).toBe(1)
    })

    it('applies green-hi class for 3+ completed missions', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-2) }),
        makeMission({ status: 'completed', started_at: dayISO(-2) }),
        makeMission({ status: 'completed', started_at: dayISO(-2) }),
      ]} />)
      expect(container.querySelectorAll('.heatmap-cell--green-hi').length).toBe(1)
    })

    it('multiple active days each get their own colored cell', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'failed', started_at: dayISO(-1) }),
        makeMission({ status: 'cancelled', started_at: dayISO(-2) }),
        makeMission({ status: 'completed', started_at: dayISO(-3) }),
      ]} />)
      expect(container.querySelectorAll('.heatmap-cell--fail').length).toBe(1)
      expect(container.querySelectorAll('.heatmap-cell--warn').length).toBe(1)
      expect(container.querySelectorAll('.heatmap-cell--green-lo').length).toBe(1)
    })
  })

  describe('empty days show outline cells', () => {
    it('all cells are empty when recent is empty', () => {
      const { container } = render(<ActivityHeatmap recent={[]} />)
      const allCells = container.querySelectorAll('.heatmap-cell')
      const emptyCells = container.querySelectorAll('.heatmap-cell--empty')
      // Every visible (non-hidden) cell should be empty
      const hiddenCells = container.querySelectorAll('.heatmap-cell[style*="visibility: hidden"]')
      expect(emptyCells.length + hiddenCells.length).toBe(allCells.length)
    })

    it('exactly one active cell when only one day has activity', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-10) }),
      ]} />)
      const activeCells = container.querySelectorAll(
        '.heatmap-cell--fail, .heatmap-cell--warn, .heatmap-cell--green-lo, .heatmap-cell--green-hi'
      )
      expect(activeCells.length).toBe(1)
    })

    it('empty cells are the majority when only a few days have activity', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-5) }),
        makeMission({ status: 'failed', started_at: dayISO(-10) }),
      ]} />)
      const emptyCells = container.querySelectorAll('.heatmap-cell--empty')
      const activeCells = container.querySelectorAll(
        '.heatmap-cell--fail, .heatmap-cell--warn, .heatmap-cell--green-lo, .heatmap-cell--green-hi'
      )
      expect(emptyCells.length).toBeGreaterThan(activeCells.length)
    })
  })

  describe('tooltip on hover', () => {
    it('tooltip is not visible before any hover', () => {
      const { container } = render(<ActivityHeatmap recent={[]} />)
      expect(container.querySelector('.heatmap-tooltip')).not.toBeInTheDocument()
    })

    it('tooltip appears when hovering an empty cell', () => {
      const { container } = render(<ActivityHeatmap recent={[]} />)
      const emptyCell = container.querySelector('.heatmap-cell--empty')!
      fireEvent.mouseEnter(emptyCell)
      expect(container.querySelector('.heatmap-tooltip')).toBeInTheDocument()
    })

    it('tooltip appears when hovering an active cell', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-1) }),
      ]} />)
      const activeCell = container.querySelector('.heatmap-cell--green-lo, .heatmap-cell--green-hi')!
      fireEvent.mouseEnter(activeCell)
      expect(container.querySelector('.heatmap-tooltip')).toBeInTheDocument()
    })

    it('tooltip disappears on mouse leave', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-1) }),
      ]} />)
      const activeCell = container.querySelector('.heatmap-cell--green-lo, .heatmap-cell--green-hi')!
      fireEvent.mouseEnter(activeCell)
      expect(container.querySelector('.heatmap-tooltip')).toBeInTheDocument()
      fireEvent.mouseLeave(activeCell)
      expect(container.querySelector('.heatmap-tooltip')).not.toBeInTheDocument()
    })

    it('tooltip shows correct ok count', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-1) }),
        makeMission({ status: 'completed', started_at: dayISO(-1) }),
        makeMission({ status: 'failed', started_at: dayISO(-1) }),
      ]} />)
      const failCell = container.querySelector('.heatmap-cell--fail')!
      fireEvent.mouseEnter(failCell)
      const ttCounts = container.querySelector('.heatmap-tt-counts')!
      expect(ttCounts.textContent).toMatch(/2 ok/)
      expect(ttCounts.textContent).toMatch(/1 failed/)
    })

    it('tooltip shows the date for the hovered cell', () => {
      const dateStr = dayDateStr(-1)
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-1) }),
      ]} />)
      const activeCell = container.querySelector('.heatmap-cell--green-lo, .heatmap-cell--green-hi')!
      fireEvent.mouseEnter(activeCell)
      expect(container.querySelector('.heatmap-tt-date')?.textContent).toBe(dateStr)
    })

    it('tooltip shows "No missions" for an empty cell', () => {
      const { container } = render(<ActivityHeatmap recent={[]} />)
      const emptyCell = container.querySelector('.heatmap-cell--empty')!
      fireEvent.mouseEnter(emptyCell)
      expect(container.querySelector('.heatmap-tooltip')?.textContent).toMatch(/No missions/)
    })

    it('tooltip has role="tooltip" for accessibility', () => {
      const { container } = render(<ActivityHeatmap recent={[]} />)
      const emptyCell = container.querySelector('.heatmap-cell--empty')!
      fireEvent.mouseEnter(emptyCell)
      expect(container.querySelector('[role="tooltip"]')).toBeInTheDocument()
    })
  })

  describe('success rate calculation', () => {
    it('shows 0% when no missions', () => {
      const { container } = render(<ActivityHeatmap recent={[]} />)
      expect(container.querySelector('.heatmap-rate-num')?.textContent).toBe('0%')
    })

    it('shows 100% when all missions completed', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-1) }),
        makeMission({ status: 'completed', started_at: dayISO(-2) }),
        makeMission({ status: 'completed', started_at: dayISO(-3) }),
      ]} />)
      expect(container.querySelector('.heatmap-rate-num')?.textContent).toBe('100%')
    })

    it('shows 0% when all missions failed', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'failed', started_at: dayISO(-1) }),
        makeMission({ status: 'failed', started_at: dayISO(-2) }),
      ]} />)
      expect(container.querySelector('.heatmap-rate-num')?.textContent).toBe('0%')
    })

    it('rounds to nearest integer (4 of 5 = 80%)', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-1) }),
        makeMission({ status: 'completed', started_at: dayISO(-2) }),
        makeMission({ status: 'completed', started_at: dayISO(-3) }),
        makeMission({ status: 'completed', started_at: dayISO(-4) }),
        makeMission({ status: 'failed', started_at: dayISO(-5) }),
      ]} />)
      expect(container.querySelector('.heatmap-rate-num')?.textContent).toBe('80%')
    })

    it('counts ALL missions passed via recent prop regardless of visual window', () => {
      // FINDING: success rate iterates byDate which includes all recent missions,
      // not just those within the 6-month grid range. This means missions older
      // than 6 months still affect the rate if passed in recent[].
      const oldDate = new Date()
      oldDate.setDate(oldDate.getDate() - 400) // well outside 6-month range
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-1) }),
        makeMission({ status: 'failed', started_at: oldDate.toISOString() }),
      ]} />)
      // 1 completed + 1 failed = 50%
      expect(container.querySelector('.heatmap-rate-num')?.textContent).toBe('50%')
    })

    it('shows success rate label', () => {
      const { container } = render(<ActivityHeatmap recent={[]} />)
      expect(container.querySelector('.heatmap-rate-lbl')?.textContent).toBe('success rate')
    })

    it('shows trend badge', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-1) }),
      ]} />)
      expect(container.querySelector('.heatmap-trend')).toBeInTheDocument()
    })
  })

  describe('aria labels', () => {
    it('active cell has aria-label with date and count', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-1) }),
      ]} />)
      const activeCell = container.querySelector('.heatmap-cell--green-lo, .heatmap-cell--green-hi')!
      const label = activeCell.getAttribute('aria-label')!
      expect(label).toContain(dayDateStr(-1))
      expect(label).toContain('1 mission')
    })

    it('empty cells have aria-label showing 0 missions', () => {
      const { container } = render(<ActivityHeatmap recent={[]} />)
      const emptyCell = container.querySelector('.heatmap-cell--empty')!
      expect(emptyCell.getAttribute('aria-label')).toContain('0 missions')
    })

    it('cell with 2 missions uses plural label', () => {
      const { container } = render(<ActivityHeatmap recent={[
        makeMission({ status: 'completed', started_at: dayISO(-1) }),
        makeMission({ status: 'completed', started_at: dayISO(-1) }),
      ]} />)
      const activeCell = container.querySelector('.heatmap-cell--green-lo')!
      expect(activeCell.getAttribute('aria-label')).toContain('2 missions')
    })
  })
})
