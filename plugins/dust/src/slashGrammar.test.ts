import { describe, expect, it } from 'vitest'
import { parseSlash } from './slashGrammar'

// Test vector table from DESIGN-SLASH-GRAMMAR.md §5.3 — treated as the
// normative acceptance suite. Each row is one case.
describe('parseSlash', () => {
  it('/track → prefix only, empty args', () => {
    expect(parseSlash('/track')).toEqual({
      prefix: 'track',
      args: '',
      raw: '/track',
    })
  })

  it('/track TRK-419 → prefix + single-arg tail', () => {
    expect(parseSlash('/track TRK-419')).toEqual({
      prefix: 'track',
      args: 'TRK-419',
      raw: '/track TRK-419',
    })
  })

  it('/open ~/notes/foo.md → preserves tilde + slashes in args', () => {
    expect(parseSlash('/open ~/notes/foo.md')).toEqual({
      prefix: 'open',
      args: '~/notes/foo.md',
      raw: '/open ~/notes/foo.md',
    })
  })

  it('/ask what is dust? → preserves spaces + punctuation in args', () => {
    expect(parseSlash('/ask what is dust?')).toEqual({
      prefix: 'ask',
      args: 'what is dust?',
      raw: '/ask what is dust?',
    })
  })

  it('/foo-bar baz → hyphen allowed in prefix', () => {
    expect(parseSlash('/foo-bar baz')).toEqual({
      prefix: 'foo-bar',
      args: 'baz',
      raw: '/foo-bar baz',
    })
  })

  it('/foo  baz → double space preserved in args (leading space kept)', () => {
    expect(parseSlash('/foo  baz')).toEqual({
      prefix: 'foo',
      args: ' baz',
      raw: '/foo  baz',
    })
  })

  it('/ → null (empty prefix)', () => {
    expect(parseSlash('/')).toBeNull()
  })

  it('/Foo → null (uppercase rejected)', () => {
    expect(parseSlash('/Foo')).toBeNull()
  })

  it('/1foo → null (must start with [a-z])', () => {
    expect(parseSlash('/1foo')).toBeNull()
  })

  it('/foo_bar → null (underscore not in charset)', () => {
    expect(parseSlash('/foo_bar')).toBeNull()
  })

  it('/foo\\tbaz → null (separator must be U+0020, not tab)', () => {
    expect(parseSlash('/foo\tbaz')).toBeNull()
  })

  it('hi /foo → null (slash must be at column 0)', () => {
    expect(parseSlash('hi /foo')).toBeNull()
  })

  it(' /foo → null (no implicit trim)', () => {
    expect(parseSlash(' /foo')).toBeNull()
  })

  it('"" → null (empty input)', () => {
    expect(parseSlash('')).toBeNull()
  })
})
