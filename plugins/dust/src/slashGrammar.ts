// Slash-command grammar parser.
//
// Grammar (see docs/nanika-ui/DESIGN-SLASH-GRAMMAR.md):
//   command := "/" prefix ( " " rest )?
//   prefix  := [a-z] [a-z0-9-]*
//   rest    := <every character to end-of-line, verbatim>
//
// The separator between prefix and rest is exactly one ASCII space (U+0020).
// Returns null iff the line is not a syntactically valid slash command —
// callers route null to free-text handling.

export type SlashCommand = {
  prefix: string
  args: string
  raw: string
}

const SLASH_RE = /^\/([a-z][a-z0-9-]*)(?: (.*))?$/

export function parseSlash(line: string): SlashCommand | null {
  const m = SLASH_RE.exec(line)
  if (!m) return null
  return { prefix: m[1], args: m[2] ?? '', raw: line }
}
