import { useState, useEffect, useCallback } from 'react'
import type { Project, Thread } from '../mocks/projects'
import { Icon } from '../icons/Icon'

// ─── Types ────────────────────────────────────────────────────────────────────

type FlatRow =
  | { kind: 'project'; projectId: string; project: Project; depth: 0 }
  | { kind: 'thread';  projectId: string; thread: Thread;   depth: 1 }

// ─── ProjectsTree ─────────────────────────────────────────────────────────────

interface ProjectsTreeProps {
  projects: Project[]
  onThreadSelect?: (projectId: string, threadId: string) => void
  onProjectToggle?: (projectId: string) => void
}

export function ProjectsTree({ projects, onThreadSelect, onProjectToggle }: ProjectsTreeProps) {
  const [query, setQuery]           = useState('')
  const [expanded, setExpanded]     = useState<Set<string>>(() => new Set(projects.map(p => p.id)))
  const [cursor, setCursor]         = useState(0)
  const [sortAsc, setSortAsc]       = useState(true)

  // Build flat navigable row list
  const sortedProjects = [...projects].sort((a, b) =>
    sortAsc ? a.name.localeCompare(b.name) : b.name.localeCompare(a.name)
  )

  const filtered = query
    ? sortedProjects.map(p => ({
        ...p,
        threads: p.threads.filter(t =>
          t.title.toLowerCase().includes(query.toLowerCase())
        ),
      })).filter(p => p.name.toLowerCase().includes(query.toLowerCase()) || p.threads.length > 0)
    : sortedProjects

  const rows: FlatRow[] = []
  for (const project of filtered) {
    rows.push({ kind: 'project', projectId: project.id, project, depth: 0 })
    if (expanded.has(project.id)) {
      for (const thread of project.threads) {
        rows.push({ kind: 'thread', projectId: project.id, thread, depth: 1 })
      }
    }
  }

  const clampedCursor = Math.min(cursor, Math.max(0, rows.length - 1))

  const activate = useCallback((idx: number) => {
    const row = rows[idx]
    if (!row) return
    if (row.kind === 'project') {
      setExpanded(s => {
        const next = new Set(s)
        next.has(row.projectId) ? next.delete(row.projectId) : next.add(row.projectId)
        return next
      })
      onProjectToggle?.(row.projectId)
    } else {
      onThreadSelect?.(row.projectId, row.thread.id)
    }
  }, [rows, onProjectToggle, onThreadSelect])

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.target instanceof HTMLInputElement) return
      if (e.key === 'j') {
        e.preventDefault()
        setCursor(c => Math.min(c + 1, rows.length - 1))
      } else if (e.key === 'k') {
        e.preventDefault()
        setCursor(c => Math.max(c - 1, 0))
      } else if (e.key === 'o') {
        e.preventDefault()
        const row = rows[clampedCursor]
        if (row?.kind === 'project') {
          setExpanded(s => {
            const next = new Set(s)
            next.has(row.projectId) ? next.delete(row.projectId) : next.add(row.projectId)
            return next
          })
        }
      } else if (e.key === 'Enter') {
        e.preventDefault()
        activate(clampedCursor)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [rows, clampedCursor, activate])

  return (
    <div style={{
      width:           '280px',
      background:      'var(--s1)',
      borderRight:     '1px solid var(--border)',
      display:         'flex',
      flexDirection:   'column',
      fontFamily:      'var(--sans)',
      overflow:        'hidden',
      flexShrink:      0,
    }}>
      {/* ⌘K search input */}
      <div style={{ padding: '10px 12px 8px', borderBottom: '1px solid var(--border-soft)' }}>
        <div style={{
          display:      'flex',
          alignItems:   'center',
          gap:          '6px',
          background:   'var(--s2)',
          border:       '1px solid var(--border)',
          borderRadius: '6px',
          padding:      '5px 10px',
        }}>
          <Icon name="Search" size={12} style={{ color: 'var(--faint)', flexShrink: 0 }} />
          <input
            value={query}
            onChange={e => { setQuery(e.target.value); setCursor(0) }}
            placeholder="Search projects…"
            aria-label="Search projects"
            style={{
              flex:        1,
              background:  'transparent',
              border:      'none',
              outline:     'none',
              color:       'var(--text)',
              fontSize:    '12.5px',
              fontFamily:  'var(--sans)',
              minWidth:    0,
            }}
          />
          <span style={{
            fontFamily:   'var(--mono)',
            fontSize:     '10px',
            color:        'var(--faint)',
            letterSpacing: '0.04em',
          }}>⌘K</span>
        </div>
      </div>

      {/* PROJECTS header */}
      <div style={{
        display:        'flex',
        alignItems:     'center',
        padding:        '8px 12px 4px',
        gap:            '4px',
      }}>
        <h6 style={{
          flex:          1,
          margin:        0,
          fontFamily:    'var(--mono)',
          fontSize:      '10px',
          fontWeight:    600,
          letterSpacing: '0.10em',
          color:         'var(--faint)',
          textTransform: 'uppercase',
        }}>PROJECTS</h6>
        <button
          onClick={() => setSortAsc(s => !s)}
          title={sortAsc ? 'Sort Z→A' : 'Sort A→Z'}
          aria-label="Toggle sort order"
          style={iconBtn}
        >
          <Icon name={sortAsc ? 'SortAsc' : 'SortDesc'} size={13} />
        </button>
        <button
          title="Add project"
          aria-label="Add project"
          style={iconBtn}
        >
          <Icon name="Plus" size={13} />
        </button>
      </div>

      {/* Rows */}
      <div
        role="tree"
        aria-label="Projects"
        style={{ flex: 1, overflowY: 'auto' }}
      >
        {rows.map((row, idx) => {
          const isActive = idx === clampedCursor
          if (row.kind === 'project') {
            const isOpen = expanded.has(row.projectId)
            return (
              <div
                key={row.projectId}
                role="treeitem"
                aria-expanded={isOpen}
                aria-selected={isActive}
                tabIndex={isActive ? 0 : -1}
                onClick={() => { setCursor(idx); activate(idx) }}
                style={{
                  display:       'flex',
                  alignItems:    'center',
                  gap:           '8px',
                  padding:       '6px 12px',
                  cursor:        'pointer',
                  background:    isActive ? 'var(--s2)' : 'transparent',
                  borderLeft:    isActive ? '2px solid var(--accent)' : '2px solid transparent',
                }}
              >
                {/* Avatar */}
                <span style={{
                  width:        '20px',
                  height:       '20px',
                  borderRadius: '5px',
                  background:   row.project.avatarColor,
                  display:      'flex',
                  alignItems:   'center',
                  justifyContent: 'center',
                  fontSize:     '10px',
                  fontWeight:   700,
                  color:        '#fff',
                  flexShrink:   0,
                  fontFamily:   'var(--mono)',
                }}>
                  {row.project.avatarInitials
                    ? row.project.avatarInitials
                    : <Icon name="User" size={12} />}
                </span>
                <span style={{ flex: 1, fontSize: '13px', color: 'var(--text)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {row.project.name}
                </span>
                <Icon
                  name="ChevronRight"
                  size={10}
                  style={{
                    transform:  isOpen ? 'rotate(90deg)' : 'rotate(0deg)',
                    transition: 'transform 150ms ease',
                    flexShrink: 0,
                  }}
                />
              </div>
            )
          } else {
            return (
              <div
                key={row.thread.id}
                role="treeitem"
                aria-selected={isActive}
                tabIndex={isActive ? 0 : -1}
                onClick={() => { setCursor(idx); activate(idx) }}
                style={{
                  display:     'flex',
                  alignItems:  'center',
                  gap:         '6px',
                  padding:     '5px 12px 5px 40px',
                  cursor:      'pointer',
                  background:  isActive ? 'var(--s2)' : 'transparent',
                  borderLeft:  isActive ? '2px solid var(--accent)' : '2px solid transparent',
                }}
              >
                <span style={{ flex: 1, fontSize: '12px', color: 'var(--muted)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {row.thread.title}
                </span>
                <span style={{ fontSize: '10.5px', color: 'var(--faint)', flexShrink: 0 }}>
                  {row.thread.agoLabel}
                </span>
              </div>
            )
          }
        })}
        {rows.length === 0 && (
          <div style={{ padding: '12px', fontSize: '12px', color: 'var(--faint)', textAlign: 'center' }}>
            No results
          </div>
        )}
      </div>

      {/* Key hints */}
      <div style={{
        display:       'flex',
        gap:           '8px',
        padding:       '6px 12px',
        borderTop:     '1px solid var(--border-soft)',
        fontFamily:    'var(--mono)',
        fontSize:      '10px',
        color:         'var(--ghost)',
      }}>
        <span><span className="K nav">j</span>/<span className="K nav">k</span> nav</span>
        <span><span className="K nav">o</span> expand</span>
        <span><span className="K nav">↵</span> open</span>
      </div>
    </div>
  )
}

const iconBtn: React.CSSProperties = {
  background:  'transparent',
  border:      'none',
  padding:     '2px 4px',
  cursor:      'pointer',
  color:       'var(--faint)',
  fontSize:    '13px',
  lineHeight:  1,
  borderRadius: '3px',
  display:     'flex',
  alignItems:  'center',
}
