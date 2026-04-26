//! Slash-command grammar parser.
//!
//! See `docs/nanika-ui/DESIGN-SLASH-GRAMMAR.md` for the full spec. This
//! port mirrors the TypeScript parser in `plugins/dust/src/slashGrammar.ts`
//! byte-for-byte; the field names and order are deliberately identical so
//! wire payloads do not need a translation table.
//!
//! Grammar:
//!
//! ```text
//! command := "/" prefix ( " " rest )?
//! prefix  := [a-z] [a-z0-9-]*
//! rest    := <every character to end-of-line, verbatim>
//! ```
//!
//! The separator between `prefix` and `rest` is exactly one ASCII space
//! (U+0020). `parse_slash` returns `None` when the line is not a
//! syntactically valid slash command.
//!
//! The implementation is a hand-rolled scanner rather than a regex so the
//! crate does not grow a `regex` dependency just to reimplement a four-line
//! state machine.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    pub prefix: String,
    pub args: String,
    pub raw: String,
}

pub fn parse_slash(line: &str) -> Option<SlashCommand> {
    let bytes = line.as_bytes();

    // Must start with '/'
    if bytes.first() != Some(&b'/') {
        return None;
    }

    // First char after '/' must be [a-z].
    let mut i = 1;
    if i >= bytes.len() || !bytes[i].is_ascii_lowercase() {
        return None;
    }
    i += 1;

    // Subsequent prefix chars: [a-z0-9-]*.
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' {
            i += 1;
        } else {
            break;
        }
    }

    let prefix = line[1..i].to_string();

    // End-of-line immediately after prefix → args = "".
    if i == bytes.len() {
        return Some(SlashCommand {
            prefix,
            args: String::new(),
            raw: line.to_string(),
        });
    }

    // Otherwise, the next byte must be exactly one ASCII space (U+0020).
    if bytes[i] != b' ' {
        return None;
    }
    i += 1;

    Some(SlashCommand {
        prefix,
        args: line[i..].to_string(),
        raw: line.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(prefix: &str, args: &str, raw: &str) -> SlashCommand {
        SlashCommand {
            prefix: prefix.to_string(),
            args: args.to_string(),
            raw: raw.to_string(),
        }
    }

    // Test vector table from DESIGN-SLASH-GRAMMAR.md §5.3 — normative suite.

    #[test]
    fn prefix_only_returns_empty_args() {
        assert_eq!(parse_slash("/track"), Some(cmd("track", "", "/track")));
    }

    #[test]
    fn prefix_with_tail() {
        assert_eq!(
            parse_slash("/track TRK-419"),
            Some(cmd("track", "TRK-419", "/track TRK-419"))
        );
    }

    #[test]
    fn preserves_tilde_and_slashes_in_args() {
        assert_eq!(
            parse_slash("/open ~/notes/foo.md"),
            Some(cmd("open", "~/notes/foo.md", "/open ~/notes/foo.md"))
        );
    }

    #[test]
    fn preserves_spaces_and_punctuation_in_args() {
        assert_eq!(
            parse_slash("/ask what is dust?"),
            Some(cmd("ask", "what is dust?", "/ask what is dust?"))
        );
    }

    #[test]
    fn hyphen_allowed_in_prefix() {
        assert_eq!(
            parse_slash("/foo-bar baz"),
            Some(cmd("foo-bar", "baz", "/foo-bar baz"))
        );
    }

    #[test]
    fn double_space_preserved_in_args() {
        // First space is the separator; the rest (" baz") is captured verbatim.
        assert_eq!(
            parse_slash("/foo  baz"),
            Some(cmd("foo", " baz", "/foo  baz"))
        );
    }

    #[test]
    fn empty_prefix_is_null() {
        assert_eq!(parse_slash("/"), None);
    }

    #[test]
    fn uppercase_is_null() {
        assert_eq!(parse_slash("/Foo"), None);
    }

    #[test]
    fn leading_digit_is_null() {
        assert_eq!(parse_slash("/1foo"), None);
    }

    #[test]
    fn underscore_is_null() {
        assert_eq!(parse_slash("/foo_bar"), None);
    }

    #[test]
    fn tab_separator_is_null() {
        assert_eq!(parse_slash("/foo\tbaz"), None);
    }

    #[test]
    fn slash_not_at_column_zero_is_null() {
        assert_eq!(parse_slash("hi /foo"), None);
    }

    #[test]
    fn leading_space_is_null() {
        assert_eq!(parse_slash(" /foo"), None);
    }

    #[test]
    fn empty_input_is_null() {
        assert_eq!(parse_slash(""), None);
    }
}
