"use client"
import { type ReactNode } from 'react'

// ─── Types ────────────────────────────────────────────────────────────────────

interface DocumentTurnProps {
  role: 'user' | 'assistant'
  content: string
}

// ─── Inline parser ────────────────────────────────────────────────────────────

function parseInline(text: string): ReactNode[] {
  const result: ReactNode[] = []
  let rest = text
  let key = 0

  while (rest.length > 0) {
    // inline code
    const codeMatch = rest.match(/^(.*?)`([^`]+)`/)
    if (codeMatch) {
      if (codeMatch[1]) result.push(<span key={key++}>{codeMatch[1]}</span>)
      result.push(
        <code key={key++} style={{
          fontFamily:   'var(--mono)',
          fontSize:     '13px',
          background:   'var(--s2)',
          borderRadius: '4px',
          padding:      '1px 5px',
          color:        'var(--text)',
        }}>
          {codeMatch[2]}
        </code>
      )
      rest = rest.slice(codeMatch[0].length)
      continue
    }

    // bold
    const boldMatch = rest.match(/^(.*?)\*\*(.+?)\*\*/)
    if (boldMatch) {
      if (boldMatch[1]) result.push(<span key={key++}>{boldMatch[1]}</span>)
      result.push(<strong key={key++} style={{ fontWeight: 600, color: 'var(--text)' }}>{boldMatch[2]}</strong>)
      rest = rest.slice(boldMatch[0].length)
      continue
    }

    // italic
    const italicMatch = rest.match(/^(.*?)\*(.+?)\*/)
    if (italicMatch) {
      if (italicMatch[1]) result.push(<span key={key++}>{italicMatch[1]}</span>)
      result.push(<em key={key++} style={{ fontStyle: 'italic' }}>{italicMatch[2]}</em>)
      rest = rest.slice(italicMatch[0].length)
      continue
    }

    result.push(<span key={key++}>{rest}</span>)
    break
  }

  return result
}

// ─── Block renderer ───────────────────────────────────────────────────────────

type Block =
  | { type: 'h1' | 'h2' | 'h3'; text: string }
  | { type: 'p'; text: string }
  | { type: 'ul'; items: string[] }
  | { type: 'ol'; items: string[] }
  | { type: 'code'; lang: string; lines: string[] }
  | { type: 'table'; header: string[]; rows: string[][] }
  | { type: 'hr' }

function parseBlocks(markdown: string): Block[] {
  const lines = markdown.split('\n')
  const blocks: Block[] = []

  let i = 0
  while (i < lines.length) {
    const line = lines[i]

    // Skip blank
    if (line.trim() === '') { i++; continue }

    // Code fence
    if (line.startsWith('```')) {
      const lang = line.slice(3).trim()
      const codeLines: string[] = []
      i++
      while (i < lines.length && !lines[i].startsWith('```')) {
        codeLines.push(lines[i])
        i++
      }
      i++ // skip closing ```
      blocks.push({ type: 'code', lang, lines: codeLines })
      continue
    }

    // Headings
    if (line.startsWith('### ')) { blocks.push({ type: 'h3', text: line.slice(4) }); i++; continue }
    if (line.startsWith('## '))  { blocks.push({ type: 'h2', text: line.slice(3) }); i++; continue }
    if (line.startsWith('# '))   { blocks.push({ type: 'h1', text: line.slice(2) }); i++; continue }

    // HR
    if (/^---+$/.test(line.trim())) { blocks.push({ type: 'hr' }); i++; continue }

    // Table
    if (line.startsWith('|')) {
      const tableLines: string[] = []
      while (i < lines.length && lines[i].startsWith('|')) {
        tableLines.push(lines[i])
        i++
      }
      // first row is header, second row is separator, rest are data
      const parseRow = (r: string) =>
        r.split('|').slice(1, -1).map(c => c.trim())
      const header = parseRow(tableLines[0])
      const rows = tableLines.slice(2).map(parseRow)
      blocks.push({ type: 'table', header, rows })
      continue
    }

    // Unordered list
    if (/^[-*] /.test(line)) {
      const items: string[] = []
      while (i < lines.length && /^[-*] /.test(lines[i])) {
        items.push(lines[i].slice(2))
        i++
      }
      blocks.push({ type: 'ul', items })
      continue
    }

    // Ordered list
    if (/^\d+\. /.test(line)) {
      const items: string[] = []
      while (i < lines.length && /^\d+\. /.test(lines[i])) {
        items.push(lines[i].replace(/^\d+\. /, ''))
        i++
      }
      blocks.push({ type: 'ol', items })
      continue
    }

    // Paragraph — collect consecutive non-special lines
    const paraLines: string[] = []
    while (
      i < lines.length &&
      lines[i].trim() !== '' &&
      !lines[i].startsWith('#') &&
      !lines[i].startsWith('```') &&
      !lines[i].startsWith('|') &&
      !/^[-*] /.test(lines[i]) &&
      !/^\d+\. /.test(lines[i]) &&
      !/^---+$/.test(lines[i].trim())
    ) {
      paraLines.push(lines[i])
      i++
    }
    if (paraLines.length > 0) {
      blocks.push({ type: 'p', text: paraLines.join(' ') })
    }
  }

  return blocks
}

function renderBlock(block: Block, idx: number): ReactNode {
  switch (block.type) {
    case 'h1':
      return (
        <h1 key={idx} style={{
          fontSize:     '28px',
          fontWeight:   700,
          lineHeight:   1.25,
          margin:       '0 0 12px',
          color:        'var(--text)',
          letterSpacing: '-0.02em',
        }}>
          {parseInline(block.text)}
        </h1>
      )

    case 'h2':
      return (
        <h2 key={idx} style={{
          fontSize:   '20px',
          fontWeight: 600,
          lineHeight: 1.3,
          margin:     '20px 0 8px',
          color:      'var(--text)',
        }}>
          {parseInline(block.text)}
        </h2>
      )

    case 'h3':
      return (
        <h3 key={idx} style={{
          fontSize:   '16px',
          fontWeight: 600,
          lineHeight: 1.35,
          margin:     '16px 0 6px',
          color:      'var(--text)',
        }}>
          {parseInline(block.text)}
        </h3>
      )

    case 'p':
      return (
        <p key={idx} style={{
          fontSize:   '15px',
          lineHeight: 1.55,
          margin:     '0 0 10px',
          color:      'var(--text)',
        }}>
          {parseInline(block.text)}
        </p>
      )

    case 'ul':
      return (
        <ul key={idx} style={{
          margin:      '0 0 10px',
          paddingLeft: '18px',
          fontSize:    '15px',
          lineHeight:  1.55,
          color:       'var(--text)',
        }}>
          {block.items.map((item, i) => (
            <li key={i}>{parseInline(item)}</li>
          ))}
        </ul>
      )

    case 'ol':
      return (
        <ol key={idx} style={{
          margin:      '0 0 10px',
          paddingLeft: '18px',
          fontSize:    '15px',
          lineHeight:  1.55,
          color:       'var(--text)',
        }}>
          {block.items.map((item, i) => (
            <li key={i}>{parseInline(item)}</li>
          ))}
        </ol>
      )

    case 'code':
      return (
        <div key={idx} style={{
          margin:       '0 0 12px',
          background:   'var(--s2)',
          border:       '0.5px solid var(--border)',
          borderRadius: '7px',
          overflowX:    'auto',
        }}>
          <pre style={{
            fontFamily: 'var(--mono)',
            fontSize:   '12.5px',
            lineHeight: 1.6,
            color:      'var(--text)',
            padding:    '12px 16px',
            margin:     0,
            whiteSpace: 'pre',
          }}>
            {block.lines.join('\n')}
          </pre>
        </div>
      )

    case 'table':
      return (
        <div key={idx} style={{ margin: '0 0 12px', overflowX: 'auto' }}>
          <table style={{
            width:           '100%',
            borderCollapse:  'collapse',
            fontSize:        '14px',
            fontFamily:      'var(--sans)',
          }}>
            <thead>
              <tr>
                {block.header.map((h, i) => (
                  <th key={i} style={{
                    borderBottom: '1px solid var(--border)',
                    borderRight:  i < block.header.length - 1 ? '0.5px solid var(--border)' : 'none',
                    padding:      '6px 12px',
                    textAlign:    'left',
                    fontWeight:   600,
                    color:        'var(--text)',
                    whiteSpace:   'nowrap',
                  }}>
                    {h}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {block.rows.map((row, ri) => (
                <tr key={ri}>
                  {row.map((cell, ci) => (
                    <td key={ci} style={{
                      borderBottom: ri < block.rows.length - 1 ? '0.5px solid var(--border)' : 'none',
                      borderRight:  ci < row.length - 1 ? '0.5px solid var(--border)' : 'none',
                      padding:      '6px 12px',
                      color:        'var(--muted)',
                      lineHeight:   1.5,
                    }}>
                      {parseInline(cell)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )

    case 'hr':
      return (
        <hr key={idx} style={{
          border:       'none',
          borderTop:    '0.5px solid var(--border)',
          margin:       '16px 0',
        }} />
      )
  }
}

// ─── DocumentTurn ─────────────────────────────────────────────────────────────

export function DocumentTurn({ role, content }: DocumentTurnProps) {
  if (role === 'user') {
    return (
      <div style={{
        display:     'flex',
        justifyContent: 'flex-end',
        padding:     '4px 0',
      }}>
        <div style={{
          background:   'var(--s1)',
          border:       '0.5px solid var(--border)',
          borderRadius: '12px 12px 4px 12px',
          padding:      '10px 14px',
          maxWidth:     '72%',
          fontSize:     '15px',
          lineHeight:   1.55,
          color:        'var(--text)',
          fontFamily:   'var(--sans)',
        }}>
          {content}
        </div>
      </div>
    )
  }

  // assistant turn — background: transparent
  const blocks = parseBlocks(content)
  return (
    <div style={{
      background: 'transparent',
      padding:    '4px 0',
      fontFamily: 'var(--sans)',
    }}>
      {blocks.map((block, i) => renderBlock(block, i))}
    </div>
  )
}
