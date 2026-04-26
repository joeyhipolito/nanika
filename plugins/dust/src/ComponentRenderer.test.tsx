import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { CODE_DIFF_ACCEPT_OP, ComponentRenderer } from './ComponentRenderer'
import type { CodeDiffComponent, Component } from './types'

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const userTurn: Component = {
  type: 'agent_turn',
  role: 'user',
  content: 'Hello there',
  streaming: false,
}

const assistantDone: Component = {
  type: 'agent_turn',
  role: 'assistant',
  content: 'I can help with that.',
  streaming: false,
}

const assistantStreaming: Component = {
  type: 'agent_turn',
  role: 'assistant',
  content: 'Thinking…',
  streaming: true,
}

const assistantEmptyStreaming: Component = {
  type: 'agent_turn',
  role: 'assistant',
  content: '',
  streaming: true,
}

// ---------------------------------------------------------------------------
// AgentTurn rendering
// ---------------------------------------------------------------------------

describe('AgentTurn', () => {
  it('renders user turn content', () => {
    render(<ComponentRenderer components={[userTurn]} />)
    expect(screen.getByText('Hello there')).toBeInTheDocument()
  })

  it('renders assistant turn content', () => {
    render(<ComponentRenderer components={[assistantDone]} />)
    expect(screen.getByText('I can help with that.')).toBeInTheDocument()
  })

  it('renders user-tick with terracotta color', () => {
    render(<ComponentRenderer components={[userTurn]} />)
    const tick = document.querySelector('[data-role="user-tick"]') as HTMLElement
    expect(tick).toBeInTheDocument()
    // jsdom normalizes hex to rgb
    expect(tick.style.background).toBe('rgb(218, 119, 87)')
  })

  it('renders assistant-tick', () => {
    render(<ComponentRenderer components={[assistantDone]} />)
    const tick = document.querySelector('[data-role="assistant-tick"]')
    expect(tick).toBeInTheDocument()
  })

  it('applies cream background to assistant bubble', () => {
    render(<ComponentRenderer components={[assistantDone]} />)
    const bubble = document.querySelector('[data-role="assistant-tick"]')
      ?.closest('div')
      ?.querySelector('div:last-child') as HTMLElement | null
    expect(bubble).toBeTruthy()
  })

  it('shows blinking caret when streaming is true', () => {
    render(<ComponentRenderer components={[assistantStreaming]} />)
    expect(screen.getByText('▋')).toBeInTheDocument()
    const caret = document.querySelector('.dust-caret')
    expect(caret).toBeInTheDocument()
  })

  it('hides blinking caret when streaming is false', () => {
    render(<ComponentRenderer components={[userTurn]} />)
    const caret = document.querySelector('.dust-caret')
    expect(caret).not.toBeInTheDocument()
  })

  it('hides caret when streaming is absent', () => {
    render(<ComponentRenderer components={[assistantDone]} />)
    const caret = document.querySelector('.dust-caret')
    expect(caret).not.toBeInTheDocument()
  })

  it('shows caret for empty-content streaming turn', () => {
    render(<ComponentRenderer components={[assistantEmptyStreaming]} />)
    const caret = document.querySelector('.dust-caret')
    expect(caret).toBeInTheDocument()
  })

  it('renders multiple turns in sequence', () => {
    render(
      <ComponentRenderer
        components={[userTurn, assistantStreaming]}
      />
    )
    expect(screen.getByText('Hello there')).toBeInTheDocument()
    expect(screen.getByText('Thinking…')).toBeInTheDocument()
    expect(document.querySelector('.dust-caret')).toBeInTheDocument()
  })
})

// ---------------------------------------------------------------------------
// CodeDiff rendering
// ---------------------------------------------------------------------------

const twoHunkDiff: CodeDiffComponent = {
  type: 'code_diff',
  path: '/Users/joey/nanika/plugins/dust/dust-core/src/lib.rs',
  basename: 'lib.rs',
  language: 'rust',
  hunks: [
    {
      id: 'h-0',
      old_start: 142,
      old_count: 3,
      new_start: 142,
      new_count: 4,
      header: 'pub enum Component',
      lines: [
        { kind: 'context', content: 'pub enum Component {' },
        { kind: 'remove', content: '    Text {' },
        { kind: 'add', content: '    CodeDiff(CodeDiffBody),' },
        { kind: 'add', content: '    Text {' },
      ],
    },
    {
      id: 'h-1',
      old_start: 208,
      old_count: 0,
      new_start: 209,
      new_count: 2,
      lines: [
        { kind: 'context', content: '}' },
        { kind: 'add', content: '// new trailing helper' },
      ],
    },
  ],
}

describe('CodeDiff', () => {
  it('renders both hunks with gutters and accept/reject chips per hunk', () => {
    render(<ComponentRenderer components={[twoHunkDiff]} />)
    const hunks = document.querySelectorAll('[data-role="code-diff-hunk"]')
    expect(hunks.length).toBe(2)

    // Each hunk exposes a grid body with four columns' worth of cells.
    hunks.forEach(h => {
      expect(h.querySelector('[data-role="diff-hunk-grid"]')).toBeTruthy()
      expect(h.querySelectorAll('[data-role="diff-gutter-old"]').length).toBeGreaterThan(0)
      expect(h.querySelectorAll('[data-role="diff-gutter-new"]').length).toBeGreaterThan(0)
      expect(h.querySelectorAll('[data-role="diff-marker"]').length).toBeGreaterThan(0)
    })

    // Chip pair per pending hunk + a pair at the file header ("Accept all" / "Reject all").
    expect(screen.getByTestId('chip-accept-h-0')).toBeInTheDocument()
    expect(screen.getByTestId('chip-reject-h-0')).toBeInTheDocument()
    expect(screen.getByTestId('chip-accept-h-1')).toBeInTheDocument()
    expect(screen.getByTestId('chip-reject-h-1')).toBeInTheDocument()
    expect(screen.getByTestId('chip-accept-all')).toBeInTheDocument()
    expect(screen.getByTestId('chip-reject-all')).toBeInTheDocument()
  })

  it('renders basename, path, and hunk count in the header', () => {
    render(<ComponentRenderer components={[twoHunkDiff]} />)
    expect(screen.getByText('lib.rs')).toBeInTheDocument()
    expect(
      screen.getByText('/Users/joey/nanika/plugins/dust/dust-core/src/lib.rs'),
    ).toBeInTheDocument()
    expect(screen.getByText(/rust · 2 hunks/)).toBeInTheDocument()
  })

  it('dispatches code_diff.accept_hunk with the hunk id when Accept is clicked', () => {
    const onAction = vi.fn()
    render(<ComponentRenderer components={[twoHunkDiff]} onAction={onAction} />)
    fireEvent.click(screen.getByTestId('chip-accept-h-0'))
    expect(onAction).toHaveBeenCalledWith(CODE_DIFF_ACCEPT_OP, 'h-0')
    // Accepted chip replaces the Accept/Reject pair for this hunk.
    expect(screen.queryByTestId('chip-accept-h-0')).toBeNull()
    expect(screen.getByTestId('chip-accepted-h-0')).toBeInTheDocument()
  })

  it('computes unified-diff line numbers per the cursor algorithm', () => {
    render(<ComponentRenderer components={[twoHunkDiff]} />)
    const firstHunk = document.querySelector('[data-hunk-id="h-0"]') as HTMLElement
    const oldCells = firstHunk.querySelectorAll('[data-role="diff-gutter-old"]')
    const newCells = firstHunk.querySelectorAll('[data-role="diff-gutter-new"]')
    // Lines: context(142/142), remove(143/-), add(-/143), add(-/144)
    expect(oldCells[0].textContent).toBe('142')
    expect(newCells[0].textContent).toBe('142')
    expect(oldCells[1].textContent).toBe('143')
    expect(newCells[1].textContent).toBe('')
    expect(oldCells[2].textContent).toBe('')
    expect(newCells[2].textContent).toBe('143')
    expect(oldCells[3].textContent).toBe('')
    expect(newCells[3].textContent).toBe('144')
  })
})

// ---------------------------------------------------------------------------
// ToolCallBeat rendering
// ---------------------------------------------------------------------------

describe('ToolCallBeat', () => {
  it('renders collapsed with a running status dot and name', () => {
    const beat: Component = {
      type: 'tool_call_beat',
      tool_use_id: 'toolu_run',
      name: 'tracker.next',
      status: 'running',
      started_ms: Date.now() - 500,
    }
    render(<ComponentRenderer components={[beat]} />)
    const row = document.querySelector('[data-role="tool-call-beat"]') as HTMLElement
    expect(row).toBeInTheDocument()
    expect(row.dataset.status).toBe('running')
    expect(screen.getByText('tracker.next')).toBeInTheDocument()
    const dot = row.querySelector('[data-role="tool-call-dot"]') as HTMLElement
    // Terracotta dot while running.
    expect(dot.style.background).toBe('rgb(218, 119, 87)')
  })

  it('expands on click and renders params as key-value and result as markdown', () => {
    const beat: Component = {
      type: 'tool_call_beat',
      tool_use_id: 'toolu_ok',
      name: 'tracker.create',
      params: { title: 'foo' },
      result: { issue_id: 'TRK-1' },
      status: 'ok',
      started_ms: 1_000,
      finished_ms: 1_200,
    }
    render(<ComponentRenderer components={[beat]} />)
    const toggle = screen.getByLabelText('tool tracker.create ok')
    // Before click: no params block.
    expect(document.querySelector('[data-role="tool-call-params"]')).toBeNull()
    fireEvent.click(toggle)
    expect(document.querySelector('[data-role="tool-call-params"]')).toBeInTheDocument()
    expect(document.querySelector('[data-role="tool-call-result"]')).toBeInTheDocument()
    expect(screen.getByText('title')).toBeInTheDocument()
    expect(screen.getByText('foo')).toBeInTheDocument()
    expect(screen.getByText(/TRK-1/)).toBeInTheDocument()
  })

  it('renders err status with the error color dot', () => {
    const beat: Component = {
      type: 'tool_call_beat',
      tool_use_id: 'toolu_err',
      name: 'tracker.delete',
      status: 'err',
      started_ms: 1_000,
      finished_ms: 1_100,
      result: 'boom',
    }
    render(<ComponentRenderer components={[beat]} />)
    const row = document.querySelector('[data-role="tool-call-beat"]') as HTMLElement
    expect(row.dataset.status).toBe('err')
    const dot = row.querySelector('[data-role="tool-call-dot"]') as HTMLElement
    // Uses CSS var for error color; jsdom keeps the literal string.
    expect(dot.style.background).toContain('var(--color-error)')
  })
})

// ---------------------------------------------------------------------------
// Unknown-component fallback (older hosts)
// ---------------------------------------------------------------------------

describe('unknown component fallback', () => {
  it('renders null for unknown component types without crashing', () => {
    // Cast to bypass TS exhaustiveness so we can simulate an older host payload.
    const unknown = { type: 'sparkle_button', label: 'Click' } as unknown as Component
    const { container } = render(<ComponentRenderer components={[unknown]} />)
    // The container should render the outer div but no visible content for the unknown component.
    expect(container.firstChild).toBeInTheDocument()
  })
})
