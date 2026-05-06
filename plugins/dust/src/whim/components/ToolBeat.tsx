"use client"
import { useState } from 'react'
import { Icon } from '../icons/Icon'
import type { IconName } from '../icons/registry'

// ─── Types ────────────────────────────────────────────────────────────────────

interface ToolBeatProps {
  summary: string
  body?: string
}

// ─── Verb color map ───────────────────────────────────────────────────────────

type VerbColor = string

interface VerbMeta {
  verb:  string
  rest:  string
  color: VerbColor
  icon:  IconName | null
}

function parseVerb(summary: string): VerbMeta {
  const first = summary.split(' ')[0]
  const rest  = summary.slice(first.length)

  if (first === 'Ran')      return { verb: first, rest, color: 'var(--red)',    icon: 'Terminal' }
  if (first === 'Recalled') return { verb: first, rest, color: 'var(--accent)', icon: 'Bookmark' }
  if (first === 'Read')     return { verb: first, rest, color: 'var(--text)',   icon: 'BookOpen' }
  if (first === 'Failed')   return { verb: first, rest, color: 'var(--red)',    icon: 'Alert' }

  return { verb: first, rest, color: 'var(--muted)', icon: null }
}

// ─── ToolBeat ─────────────────────────────────────────────────────────────────

export function ToolBeat({ summary, body }: ToolBeatProps) {
  const [expanded, setExpanded] = useState(false)
  const { verb, rest, color, icon } = parseVerb(summary)

  const iconColor = verb === 'Recalled' ? 'var(--accent)' : color

  return (
    <div style={{ fontFamily: 'var(--sans)' }}>
      <button
        onClick={() => setExpanded(e => !e)}
        style={{
          display:        'flex',
          alignItems:     'center',
          gap:            '6px',
          width:          '100%',
          background:     'transparent',
          border:         'none',
          cursor:         body ? 'pointer' : 'default',
          padding:        '4px 0',
          textAlign:      'left',
          pointerEvents:  body ? 'auto' : 'none',
        }}
        aria-expanded={expanded}
      >
        {/* Leading verb icon */}
        {icon && (
          <span style={{ display: 'flex', flexShrink: 0, color: iconColor }}>
            <Icon name={icon} size={12} />
          </span>
        )}

        {/* Verb */}
        <span style={{
          fontSize:   '12.5px',
          fontFamily: 'var(--mono)',
          color,
          flexShrink: 0,
        }}>
          {verb}
        </span>

        {/* Rest of summary */}
        <span style={{
          fontSize:     '12.5px',
          color:        'var(--muted)',
          flex:         1,
          overflow:     'hidden',
          textOverflow: 'ellipsis',
          whiteSpace:   'nowrap',
        }}>
          {rest}
        </span>

        {/* Chevron */}
        {body && (
          <span style={{
            color:      'var(--muted)',
            flexShrink: 0,
            transform:  expanded ? 'rotate(90deg)' : 'none',
            transition: 'transform 0.15s ease',
            display:    'flex',
          }}>
            <Icon name="ChevronRight" size={13} />
          </span>
        )}
      </button>

      {/* Expanded body */}
      {expanded && body && (
        <div style={{
          background:   'var(--s1)',
          border:       '0.5px solid var(--border)',
          borderRadius: '6px',
          padding:      '8px 12px',
          marginTop:    '2px',
          marginLeft:   '2px',
        }}>
          <pre style={{
            fontFamily: 'var(--mono)',
            fontSize:   '11.5px',
            color:      'var(--muted)',
            lineHeight: 1.6,
            margin:     0,
            whiteSpace: 'pre-wrap',
          }}>
            {body}
          </pre>
        </div>
      )}
    </div>
  )
}
