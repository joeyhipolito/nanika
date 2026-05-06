import { useEffect, useMemo, useRef, useState } from 'react'
import { Keycap } from './Keycap'
import { Icon } from '../icons/Icon'

// ─── Types ────────────────────────────────────────────────────────────────────

export type FileViewerLanguage = 'ts' | 'tsx' | 'md' | 'json' | 'sh' | 'plain'

interface FileViewerProps {
  filename: string
  language: FileViewerLanguage
  lines:    string[]
}

interface Match {
  line: number
  col:  number
}

type TokenKind = 'keyword' | 'string' | 'number' | 'comment' | 'plain'

interface Token {
  kind: TokenKind
  text: string
}

// ─── Token rules ──────────────────────────────────────────────────────────────

const KEYWORDS = new Set([
  'import', 'export', 'from', 'const', 'let', 'var', 'function', 'return',
  'if', 'else', 'for', 'while', 'switch', 'case', 'break', 'continue',
  'class', 'interface', 'type', 'extends', 'implements', 'new', 'this',
  'true', 'false', 'null', 'undefined', 'async', 'await', 'try', 'catch',
  'finally', 'throw', 'typeof', 'instanceof', 'in', 'of', 'as', 'void',
])

const KEYWORD_COLOR  = 'var(--accent)'
const STRING_COLOR   = 'var(--green)'
const NUMBER_COLOR   = 'var(--blue)'
const COMMENT_COLOR  = 'var(--muted)'
const PLAIN_COLOR    = 'var(--text)'
const GUTTER_COLOR   = '#5E5F68'  // matches DESIGN spec literal
const HIGHLIGHT_BG   = 'var(--accent-soft)'
const ACTIVE_HIGHLIGHT_BG = 'var(--accent-rim)'

const TOKEN_REGEX = /(\/\/[^\n]*|\/\*[\s\S]*?\*\/|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|`(?:\\.|[^`\\])*`|\b\d+(?:\.\d+)?\b|\b[A-Za-z_$][A-Za-z0-9_$]*\b)/g

function tokenize(line: string): Token[] {
  const tokens: Token[] = []
  let lastIndex = 0
  for (const m of line.matchAll(TOKEN_REGEX)) {
    const idx = m.index ?? 0
    if (idx > lastIndex) {
      tokens.push({ kind: 'plain', text: line.slice(lastIndex, idx) })
    }
    const text = m[0]
    let kind: TokenKind = 'plain'
    if (text.startsWith('//') || text.startsWith('/*')) kind = 'comment'
    else if (text.startsWith('"') || text.startsWith("'") || text.startsWith('`')) kind = 'string'
    else if (/^\d/.test(text)) kind = 'number'
    else if (KEYWORDS.has(text)) kind = 'keyword'
    tokens.push({ kind, text })
    lastIndex = idx + text.length
  }
  if (lastIndex < line.length) {
    tokens.push({ kind: 'plain', text: line.slice(lastIndex) })
  }
  return tokens
}

function tokenColor(kind: TokenKind): string {
  switch (kind) {
    case 'keyword': return KEYWORD_COLOR
    case 'string':  return STRING_COLOR
    case 'number':  return NUMBER_COLOR
    case 'comment': return COMMENT_COLOR
    default:        return PLAIN_COLOR
  }
}

// ─── FileViewer ───────────────────────────────────────────────────────────────

export function FileViewer({ filename, language, lines }: FileViewerProps) {
  const [searchOpen, setSearchOpen]           = useState(false)
  const [searchQuery, setSearchQuery]         = useState('')
  const [activeMatchIndex, setActiveMatchIndex] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  const containerRef = useRef<HTMLDivElement>(null)

  const matches: Match[] = useMemo(() => {
    if (!searchQuery) return []
    const out: Match[] = []
    const needle = searchQuery.toLowerCase()
    lines.forEach((line, lineIdx) => {
      const hay = line.toLowerCase()
      let from = 0
      while (true) {
        const col = hay.indexOf(needle, from)
        if (col === -1) break
        out.push({ line: lineIdx, col })
        from = col + Math.max(needle.length, 1)
      }
    })
    return out
  }, [searchQuery, lines])

  useEffect(() => { setActiveMatchIndex(0) }, [searchQuery])

  // Focus the input when search opens
  useEffect(() => {
    if (searchOpen) inputRef.current?.focus()
  }, [searchOpen])

  // Keyboard handler — `/` opens search, `n`/`N` advance/retreat, `Esc` closes
  useEffect(() => {
    const node = containerRef.current
    if (!node) return
    function onKey(e: KeyboardEvent) {
      const target = e.target as HTMLElement | null
      const inOurInput = target === inputRef.current
      const inOtherInput = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement
      if (inOurInput) {
        if (e.key === 'Escape') {
          e.preventDefault()
          setSearchOpen(false)
          setSearchQuery('')
        }
        return
      }
      if (inOtherInput) return

      if (!searchOpen && e.key === '/') {
        e.preventDefault()
        setSearchOpen(true)
        return
      }
      if (!searchOpen) return

      if (e.key === 'Escape') {
        e.preventDefault()
        setSearchOpen(false)
        setSearchQuery('')
        return
      }
      if (e.key === 'n' && !e.shiftKey) {
        if (matches.length === 0) return
        e.preventDefault()
        setActiveMatchIndex(i => (i + 1) % matches.length)
        return
      }
      if ((e.key === 'N') || (e.key === 'n' && e.shiftKey)) {
        if (matches.length === 0) return
        e.preventDefault()
        setActiveMatchIndex(i => (i - 1 + matches.length) % matches.length)
        return
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [searchOpen, matches.length])

  const matchesByLine = useMemo(() => {
    const m = new Map<number, Match[]>()
    matches.forEach(match => {
      const arr = m.get(match.line) ?? []
      arr.push(match)
      m.set(match.line, arr)
    })
    return m
  }, [matches])

  const activeMatch = matches[activeMatchIndex]

  function closeSearch() {
    setSearchOpen(false)
    setSearchQuery('')
  }

  return (
    <div
      ref={containerRef}
      style={{
        maxWidth:      '760px',
        width:         '100%',
        background:    'var(--s1)',
        border:        '0.5px solid var(--border)',
        borderRadius:  '10px',
        display:       'flex',
        flexDirection: 'column',
        overflow:      'hidden',
        fontFamily:    '"JetBrains Mono", ui-monospace, "SF Mono", Menlo, monospace',
      }}
    >
      {/* Header */}
      <div style={{
        display:      'flex',
        alignItems:   'center',
        gap:          '8px',
        padding:      '8px 12px',
        borderBottom: '0.5px solid var(--border)',
        background:   'var(--s2)',
      }}>
        <span style={{
          flex:       1,
          fontFamily: 'var(--mono)',
          fontSize:   '11.5px',
          color:      'var(--accent)',
          overflow:   'hidden',
          textOverflow: 'ellipsis',
          whiteSpace: 'nowrap',
        }}>
          {filename}
        </span>
        <span style={{
          fontFamily:    'var(--mono)',
          fontSize:      '10px',
          color:         'var(--faint)',
          letterSpacing: '0.06em',
          textTransform: 'uppercase',
        }}>
          {language}
        </span>
        {!searchOpen && (
          <span style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
            <Keycap>/</Keycap>
            <span style={{
              fontFamily: 'var(--mono)',
              fontSize:   '10.5px',
              color:      'var(--faint)',
            }}>
              search
            </span>
          </span>
        )}
      </div>

      {/* Search bar */}
      {searchOpen && (
        <div style={{
          display:      'flex',
          alignItems:   'center',
          gap:          '6px',
          padding:      '6px 10px',
          background:   'var(--s2)',
          borderBottom: '0.5px solid var(--border-soft)',
        }}>
          <span style={{ color: 'var(--accent)', display: 'flex', alignItems: 'center', flexShrink: 0 }}>
            <Icon name="Search" size={13} />
          </span>
          <input
            ref={inputRef}
            value={searchQuery}
            onChange={e => setSearchQuery(e.target.value)}
            onKeyDown={e => {
              if (e.key === 'Enter') {
                e.preventDefault()
                if (matches.length === 0) return
                if (e.shiftKey) {
                  setActiveMatchIndex(i => (i - 1 + matches.length) % matches.length)
                } else {
                  setActiveMatchIndex(i => (i + 1) % matches.length)
                }
              }
            }}
            placeholder="search file…"
            aria-label="Search file"
            style={{
              flex:         1,
              background:   'var(--s1)',
              border:       '1px solid var(--border)',
              borderRadius: '5px',
              padding:      '3px 8px',
              fontFamily:   'var(--mono)',
              fontSize:     '12px',
              color:        'var(--text)',
              outline:      'none',
            }}
          />
          <span style={{
            fontFamily: 'var(--mono)',
            fontSize:   '10.5px',
            color:      'var(--faint)',
            minWidth:   '60px',
            textAlign:  'right',
            flexShrink: 0,
          }}>
            {matches.length === 0 ? 'no match' : `${activeMatchIndex + 1} / ${matches.length}`}
          </span>
          {/* Prev / Next match */}
          <button
            onClick={() => matches.length > 0 && setActiveMatchIndex(i => (i - 1 + matches.length) % matches.length)}
            aria-label="Previous match"
            disabled={matches.length === 0}
            style={searchNavBtnStyle}
          >
            <Icon name="ArrowUp" size={12} />
          </button>
          <button
            onClick={() => matches.length > 0 && setActiveMatchIndex(i => (i + 1) % matches.length)}
            aria-label="Next match"
            disabled={matches.length === 0}
            style={searchNavBtnStyle}
          >
            <Icon name="ArrowDown" size={12} />
          </button>
          {/* Close search */}
          <button
            onClick={closeSearch}
            aria-label="Close search"
            style={{ ...searchNavBtnStyle, marginLeft: '2px' }}
          >
            <Icon name="Close" size={13} />
          </button>
        </div>
      )}

      {/* Body */}
      <div style={{
        flex:       1,
        overflow:   'auto',
        padding:    '10px 0',
        background: 'var(--s1)',
      }}>
        {lines.map((line, i) => (
          <LineRow
            key={i}
            lineNumber={i + 1}
            line={line}
            matchesOnLine={matchesByLine.get(i) ?? []}
            searchQuery={searchQuery}
            activeMatch={activeMatch && activeMatch.line === i ? activeMatch : null}
          />
        ))}
      </div>
    </div>
  )
}

// ─── Shared styles ────────────────────────────────────────────────────────────

const searchNavBtnStyle: React.CSSProperties = {
  display:        'flex',
  alignItems:     'center',
  justifyContent: 'center',
  background:     'none',
  border:         'none',
  borderRadius:   '4px',
  padding:        '3px',
  cursor:         'pointer',
  color:          'var(--faint)',
  flexShrink:     0,
}

// ─── LineRow ──────────────────────────────────────────────────────────────────

function LineRow({
  lineNumber,
  line,
  matchesOnLine,
  searchQuery,
  activeMatch,
}: {
  lineNumber:    number
  line:          string
  matchesOnLine: Match[]
  searchQuery:   string
  activeMatch:   Match | null
}) {
  const tokens = useMemo(() => tokenize(line), [line])

  return (
    <div style={{ display: 'flex', alignItems: 'flex-start', minHeight: '18px' }}>
      <span style={{
        width:        '44px',
        flexShrink:   0,
        textAlign:    'right',
        paddingRight: '12px',
        color:        GUTTER_COLOR,
        fontSize:     '11.5px',
        userSelect:   'none',
        lineHeight:   '1.6',
      }}>
        {lineNumber}
      </span>
      <span style={{
        flex:       1,
        fontSize:   '12px',
        lineHeight: '1.6',
        whiteSpace: 'pre',
        paddingRight: '12px',
      }}>
        {searchQuery && matchesOnLine.length > 0
          ? renderWithHighlights(line, matchesOnLine, searchQuery, activeMatch)
          : renderTokens(tokens)}
      </span>
    </div>
  )
}

function renderTokens(tokens: Token[]) {
  return tokens.map((t, i) => (
    <span key={i} style={{ color: tokenColor(t.kind) }}>
      {t.text}
    </span>
  ))
}

function renderWithHighlights(
  line: string,
  matches: Match[],
  query: string,
  activeMatch: Match | null,
) {
  const parts: React.ReactNode[] = []
  let cursor = 0
  matches.forEach((m, i) => {
    if (m.col > cursor) {
      parts.push(...colorize(line.slice(cursor, m.col), `pre-${i}`))
    }
    const isActive = activeMatch?.col === m.col && activeMatch?.line === m.line
    parts.push(
      <span
        key={`m-${i}`}
        style={{
          background: isActive ? ACTIVE_HIGHLIGHT_BG : HIGHLIGHT_BG,
          color:      'var(--text)',
          borderRadius: '2px',
        }}
      >
        {line.slice(m.col, m.col + query.length)}
      </span>,
    )
    cursor = m.col + query.length
  })
  if (cursor < line.length) {
    parts.push(...colorize(line.slice(cursor), 'tail'))
  }
  return parts
}

function colorize(segment: string, keyPrefix: string): React.ReactNode[] {
  return tokenize(segment).map((t, i) => (
    <span key={`${keyPrefix}-${i}`} style={{ color: tokenColor(t.kind) }}>
      {t.text}
    </span>
  ))
}
