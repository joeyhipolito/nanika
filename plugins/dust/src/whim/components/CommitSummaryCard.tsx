"use client"
import { useState } from 'react'
import { Icon } from '../icons/Icon'

// ─── Types ────────────────────────────────────────────────────────────────────

interface CommitSummaryCardProps {
  from:      string
  to:        string
  additions: number
  deletions: number
}

// ─── CommitSummaryCard ────────────────────────────────────────────────────────

export function CommitSummaryCard({ from, to, additions, deletions }: CommitSummaryCardProps) {
  const [prOpen, setPrOpen] = useState(false)

  // Truncate long branch names for display
  const truncate = (s: string, max: number) =>
    s.length > max ? s.slice(0, max - 1) + '…' : s

  const fromLabel = truncate(from, 20)
  const toLabel   = truncate(to, 30)

  return (
    <div style={{
      background:   'var(--s1)',
      border:       '0.5px solid var(--border)',
      borderRadius: '10px',
      overflow:     'hidden',
      fontFamily:   'var(--sans)',
    }}>
      {/* Main row */}
      <div style={{
        display:     'flex',
        alignItems:  'center',
        gap:         '10px',
        padding:     '10px 14px',
        flexWrap:    'wrap',
      }}>
        {/* Branch chips */}
        <div style={{ display: 'flex', alignItems: 'center', gap: '6px', flex: 1, minWidth: 0 }}>
          <span style={{ color: 'var(--faint)', display: 'flex', flexShrink: 0 }}>
            <Icon name="Branch" size={13} />
          </span>

          <span style={{
            fontFamily:   'var(--mono)',
            fontSize:     '11.5px',
            background:   'var(--s2)',
            border:       '0.5px solid var(--border)',
            borderRadius: '5px',
            padding:      '2px 8px',
            color:        'var(--muted)',
            whiteSpace:   'nowrap',
            flexShrink:   0,
          }}>
            {fromLabel}
          </span>

          <span style={{
            color:      'var(--faint)',
            flexShrink: 0,
            display:    'flex',
            alignItems: 'center',
          }}>
            <Icon name="ArrowLeft" size={13} />
          </span>

          <span style={{
            fontFamily:   'var(--mono)',
            fontSize:     '11.5px',
            background:   'var(--s2)',
            border:       '0.5px solid var(--border)',
            borderRadius: '5px',
            padding:      '2px 8px',
            color:        'var(--accent)',
            overflow:     'hidden',
            textOverflow: 'ellipsis',
            whiteSpace:   'nowrap',
            minWidth:     0,
          }}>
            {toLabel}
          </span>
        </div>

        {/* +/- pill */}
        <span style={{
          fontFamily:   'var(--mono)',
          fontSize:     '11.5px',
          background:   'var(--s2)',
          border:       '0.5px solid var(--border)',
          borderRadius: '5px',
          padding:      '2px 8px',
          display:      'flex',
          gap:          '6px',
          flexShrink:   0,
        }}>
          <span style={{ color: 'var(--green)' }}>+{additions.toLocaleString()}</span>
          <span style={{ color: 'var(--red)' }}>−{deletions.toLocaleString()}</span>
        </span>

        {/* Create PR button */}
        <div style={{ position: 'relative', flexShrink: 0 }}>
          <button
            onClick={() => setPrOpen(o => !o)}
            style={{
              display:      'flex',
              alignItems:   'center',
              gap:          '6px',
              fontFamily:   'var(--sans)',
              fontSize:     '12px',
              fontWeight:   500,
              color:        'var(--accent)',
              background:   'var(--accent-soft)',
              border:       '0.5px solid var(--accent-rim)',
              borderRadius: '6px',
              padding:      '4px 10px',
              cursor:       'pointer',
              whiteSpace:   'nowrap',
            }}
          >
            <Icon name="PullRequest" variant="duotone" size={13} />
            Create PR
            <Icon name="CaretDown" size={10} />
          </button>

          {prOpen && (
            <div style={{
              position:     'absolute',
              top:          'calc(100% + 4px)',
              right:        0,
              background:   'var(--s2)',
              border:       '0.5px solid var(--border)',
              borderRadius: '7px',
              overflow:     'hidden',
              zIndex:       100,
              minWidth:     '160px',
              boxShadow:    '0 8px 24px rgba(0,0,0,0.4)',
            }}>
              {['Open draft PR', 'Open ready PR', 'Copy branch URL'].map(opt => (
                <button
                  key={opt}
                  onClick={() => setPrOpen(false)}
                  style={{
                    display:    'block',
                    width:      '100%',
                    background: 'transparent',
                    border:     'none',
                    padding:    '8px 12px',
                    textAlign:  'left',
                    fontSize:   '12px',
                    fontFamily: 'var(--sans)',
                    color:      'var(--muted)',
                    cursor:     'pointer',
                  }}
                >
                  {opt}
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
