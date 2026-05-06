import { useState } from 'react'
import type { FileEntry } from '../mocks/files'
import { Icon } from '../icons/Icon'
import type { IconName } from '../icons/registry'

// ─── Types ────────────────────────────────────────────────────────────────────

type ViewMode = 'list' | 'compact'

interface FilesPanelProps {
  files:    FileEntry[]
  onClose?: () => void
  onSelect?: (path: string) => void
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

const FILE_EXT_ICON: Record<string, IconName> = {
  md:   'FileText',
  json: 'FileText',
  txt:  'FileText',
  css:  'FileText',
}

const FILE_EXT_COLOR: Record<string, string> = {
  tsx:  'var(--blue)',
  ts:   'var(--accent-2)',
  css:  'var(--green)',
  md:   'var(--muted)',
  json: 'var(--accent)',
}

function fileIconForExt(ext: string): IconName {
  return FILE_EXT_ICON[ext] ?? 'File'
}

function fileColorForExt(ext: string): string {
  return FILE_EXT_COLOR[ext] ?? 'var(--faint)'
}

// ─── FilesPanel ───────────────────────────────────────────────────────────────

export function FilesPanel({ files, onClose, onSelect }: FilesPanelProps) {
  const [query, setQuery]       = useState('')
  const [viewMode, setViewMode] = useState<ViewMode>('list')
  const [expanded, setExpanded] = useState<Set<string>>(() => {
    const s = new Set<string>()
    files.forEach(f => { if (f.kind === 'folder' && f.expanded) s.add(f.path) })
    return s
  })

  const isContentSearch = query.startsWith('?')
  const searchTerm      = isContentSearch ? query.slice(1).toLowerCase() : query.toLowerCase()

  const visible = files.filter(f => {
    if (!searchTerm) return true
    return f.name.toLowerCase().includes(searchTerm) || f.path.toLowerCase().includes(searchTerm)
  })

  function toggleFolder(path: string) {
    setExpanded(prev => {
      const next = new Set(prev)
      if (next.has(path)) next.delete(path)
      else next.add(path)
      return next
    })
  }

  return (
    <div style={{
      width:         '300px',
      background:    'var(--s1)',
      border:        '0.5px solid var(--border)',
      borderRadius:  '10px',
      display:       'flex',
      flexDirection: 'column',
      fontFamily:    'var(--sans)',
      overflow:      'hidden',
      flexShrink:    0,
    }}>
      {/* Header */}
      <div style={{
        display:        'flex',
        alignItems:     'center',
        gap:            '8px',
        padding:        '10px 12px 10px 14px',
        borderBottom:   '0.5px solid var(--border)',
        flexShrink:     0,
      }}>
        <span style={{
          flex:          1,
          fontSize:      '12.5px',
          fontWeight:    600,
          color:         'var(--text)',
          letterSpacing: '0.01em',
        }}>
          Files
        </span>

        {/* View toggle */}
        <button
          onClick={() => setViewMode(m => m === 'list' ? 'compact' : 'list')}
          aria-label={viewMode === 'list' ? 'Switch to compact view' : 'Switch to list view'}
          title={viewMode === 'list' ? 'Compact view' : 'List view'}
          style={{
            background:   viewMode === 'compact' ? 'var(--s3)' : 'transparent',
            border:       'none',
            borderRadius: '4px',
            padding:      '3px 5px',
            cursor:       'pointer',
            color:        viewMode === 'compact' ? 'var(--accent)' : 'var(--faint)',
            display:      'flex',
            alignItems:   'center',
          }}
        >
          <Icon name={viewMode === 'list' ? 'ViewList' : 'ViewCompact'} size={13} />
        </button>

        {/* Close */}
        <button
          onClick={onClose}
          aria-label="Close files panel"
          style={{
            background:   'transparent',
            border:       'none',
            cursor:       'pointer',
            color:        'var(--faint)',
            lineHeight:   1,
            padding:      '0 2px',
            display:      'flex',
            alignItems:   'center',
          }}
        >
          <Icon name="Close" size={15} />
        </button>
      </div>

      {/* Filter input */}
      <div style={{ padding: '8px 10px', borderBottom: '0.5px solid var(--border-soft)', flexShrink: 0 }}>
        <div style={{ position: 'relative', display: 'flex', alignItems: 'center' }}>
          <span style={{
            position:      'absolute',
            left:          '9px',
            top:           '50%',
            transform:     'translateY(-50%)',
            color:         isContentSearch ? 'var(--accent)' : 'var(--faint)',
            display:       'flex',
            pointerEvents: 'none',
          }}>
            <Icon name="Search" size={12} />
          </span>
          <input
            value={query}
            onChange={e => setQuery(e.target.value)}
            placeholder="Filter files... (?text to search contents)"
            style={{
              width:        '100%',
              background:   'var(--s2)',
              border:       `1px solid ${isContentSearch ? 'var(--accent)' : 'var(--border)'}`,
              borderRadius: '6px',
              padding:      '5px 9px 5px 28px',
              fontSize:     '12px',
              color:        'var(--text)',
              fontFamily:   'var(--mono)',
              outline:      'none',
              boxSizing:    'border-box',
              transition:   'border-color 120ms',
            }}
          />
        </div>
        {isContentSearch && (
          <div style={{
            marginTop:  '4px',
            fontSize:   '10.5px',
            color:      'var(--accent)',
            fontFamily: 'var(--mono)',
            paddingLeft: '2px',
          }}>
            content search mode
          </div>
        )}
      </div>

      {/* Body */}
      <div style={{ flex: 1, overflowY: 'auto', paddingBottom: '8px' }}>
        {visible.map(entry => (
          <FileRow
            key={entry.path}
            entry={entry}
            expanded={expanded.has(entry.path)}
            compact={viewMode === 'compact'}
            onToggle={() => entry.kind === 'folder' && toggleFolder(entry.path)}
            onSelect={() => onSelect?.(entry.path)}
          />
        ))}
        {visible.length === 0 && (
          <div style={{
            padding:   '24px 16px',
            fontSize:  '12px',
            color:     'var(--ghost)',
            fontFamily: 'var(--mono)',
            textAlign: 'center',
          }}>
            no matches
          </div>
        )}
      </div>
    </div>
  )
}

// ─── FileRow ──────────────────────────────────────────────────────────────────

function FileRow({
  entry,
  expanded,
  compact,
  onToggle,
  onSelect,
}: {
  entry:    FileEntry
  expanded: boolean
  compact:  boolean
  onToggle: () => void
  onSelect: () => void
}) {
  const depth  = entry.path.split('/').length - 1
  const indent = depth * 14

  const isFolder = entry.kind === 'folder'
  const ext      = isFolder ? '' : (entry as { ext: string }).ext

  return (
    <button
      onClick={isFolder ? onToggle : onSelect}
      style={{
        display:     'flex',
        alignItems:  'center',
        gap:         '6px',
        width:       '100%',
        background:  'transparent',
        border:      'none',
        padding:     compact ? `3px 12px 3px ${12 + indent}px` : `5px 12px 5px ${12 + indent}px`,
        cursor:      'pointer',
        textAlign:   'left',
        fontFamily:  'var(--mono)',
      }}
    >
      {/* Icon */}
      <span style={{ flexShrink: 0, color: isFolder ? 'var(--accent-2)' : fileColorForExt(ext), display: 'flex' }}>
        {isFolder
          ? <Icon name={expanded ? 'FolderOpen' : 'Folder'} size={13} />
          : <Icon name={fileIconForExt(ext)} size={13} />}
      </span>

      {/* Name */}
      <span style={{
        fontSize:      compact ? '11px' : '12px',
        color:         isFolder ? 'var(--muted)' : 'var(--text)',
        overflow:      'hidden',
        textOverflow:  'ellipsis',
        whiteSpace:    'nowrap',
        flex:          1,
      }}>
        {entry.name}
      </span>
    </button>
  )
}
