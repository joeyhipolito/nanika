import type { Component, ListItem, TextStyle } from './types'

export type ActionHandler = (action: string, id?: string) => void | Promise<void>

// ---------------------------------------------------------------------------
// Variant renderers
// ---------------------------------------------------------------------------

function textColor(style?: TextStyle): React.CSSProperties {
  if (!style?.color) return {}
  const { r, g, b } = style.color
  return { color: `rgb(${r},${g},${b})` }
}

function textClassName(style?: TextStyle): string {
  const parts: string[] = ['text-sm leading-relaxed']
  if (style?.bold) parts.push('font-semibold')
  if (style?.italic) parts.push('italic')
  if (style?.underline) parts.push('underline')
  return parts.join(' ')
}

function RenderText({ content, style }: { content: string; style?: TextStyle }) {
  return (
    <p
      className={textClassName(style)}
      style={{ color: 'var(--text-primary)', ...textColor(style) }}
    >
      {content}
    </p>
  )
}

function RenderListItemRow({ item }: { item: ListItem }) {
  return (
    <div
      className="flex items-start gap-2 rounded px-2 py-1.5"
      style={{ opacity: item.disabled ? 0.4 : 1 }}
    >
      <span
        aria-hidden="true"
        className="mt-[6px] h-1.5 w-1.5 shrink-0 rounded-full"
        style={{ background: 'var(--text-secondary)' }}
      />
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm" style={{ color: 'var(--text-primary)' }}>
          {item.label}
        </p>
        {item.description && (
          <p className="truncate text-[11px]" style={{ color: 'var(--text-secondary)' }}>
            {item.description}
          </p>
        )}
      </div>
    </div>
  )
}

function RenderList({ items, title }: { items: ListItem[]; title?: string }) {
  return (
    <div className="flex flex-col gap-0.5">
      {title && (
        <p
          className="mb-1 px-2 text-[10px] font-semibold uppercase tracking-wider"
          style={{ color: 'var(--text-secondary)' }}
        >
          {title}
        </p>
      )}
      {items.length === 0 ? (
        <p className="px-2 py-3 text-xs" style={{ color: 'var(--text-secondary)' }}>
          No items
        </p>
      ) : (
        items.map(item => <RenderListItemRow key={item.id} item={item} />)
      )}
    </div>
  )
}

function RenderMarkdown({ content }: { content: string }) {
  return (
    <pre
      className="overflow-x-auto whitespace-pre-wrap rounded-md p-3 text-xs leading-relaxed"
      style={{
        background: 'var(--mic-bg)',
        border: '1px solid var(--pill-border)',
        color: 'var(--text-primary)',
        fontFamily: 'inherit',
      }}
    >
      {content}
    </pre>
  )
}

// ---------------------------------------------------------------------------
// Top-level dispatcher
// ---------------------------------------------------------------------------

export type ComponentRendererProps = {
  component: Component
  onAction?: ActionHandler
}

export function ComponentRenderer({ component }: ComponentRendererProps) {
  switch (component.type) {
    case 'text':
      return <RenderText content={component.content} style={component.style} />
    case 'list':
      return <RenderList items={component.items} title={component.title} />
    case 'markdown':
      return <RenderMarkdown content={component.content} />
    case 'divider':
      return <hr style={{ borderColor: 'var(--border)', margin: '8px 0' }} />
  }
}
