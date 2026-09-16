//! Secret-redaction boundary (B3-DESIGN §7).
//!
//! [`Redactor`] is a hand-written port of `secretPatterns` and `containsSecret`
//! from `skills/orchestrator/internal/advisorbridge/prepare.go:32-43`. The
//! workspace has no `regex` dependency and the B3 lease forbids adding one, so
//! each Go pattern is reproduced as an explicit scanner below; the module test
//! pins one positive and one negative case per pattern family against the Go
//! source so a divergence surfaces as a test failure rather than a silent hole.
//!
//! [`Redacted<T>`] is the *type-level* half of the boundary: its `Display` and
//! `Debug` both print a placeholder, so a value wrapped in it cannot reach a
//! log through either formatting trait.

use std::fmt;

/// Wrapper whose `Display` and `Debug` never render the value they hold.
///
/// This is what makes B3-DESIGN §7 rule 1 true by construction: an error
/// variant holding `Redacted<String>` cannot leak its contents through `{}` or
/// `{:?}`, and `thiserror`'s `#[error]` attribute renders through `Display`.
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    /// Wraps a value so it can no longer be formatted.
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    /// Unwraps the value. Callers that do this own the leak.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Redacted(..)")
    }
}

impl<T: Clone> Clone for Redacted<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// Which denylist family matched a scan.
///
/// Reported in place of the matched text so an error can name *what* was found
/// without carrying the secret itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[non_exhaustive]
pub enum SecretKind {
    /// A PEM private-key header.
    PrivateKey,
    /// An `authorization` / `cookie` style header line.
    CredentialHeader,
    /// A provider API token (`sk-`, `ghp_`, `AIza`, …).
    ApiToken,
    /// An AWS access key id (`AKIA` / `ASIA`).
    AwsAccessKeyId,
    /// A JSON Web Token.
    JsonWebToken,
    /// A `Bearer` / `Basic` credential.
    HttpCredential,
    /// A `password=` / `api_key:` style assignment.
    CredentialAssignment,
    /// A URL carrying `user:password@`.
    UrlUserInfo,
}

impl SecretKind {
    /// Stable lowercase label used in `[redacted:<kind>]` replacements.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PrivateKey => "private-key",
            Self::CredentialHeader => "credential-header",
            Self::ApiToken => "api-token",
            Self::AwsAccessKeyId => "aws-access-key-id",
            Self::JsonWebToken => "jwt",
            Self::HttpCredential => "http-credential",
            Self::CredentialAssignment => "credential-assignment",
            Self::UrlUserInfo => "url-userinfo",
        }
    }
}

impl fmt::Display for SecretKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Outcome of scanning one string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretVerdict {
    /// No denylist family matched.
    Clean,
    /// The named family matched. The matched text is deliberately not carried.
    Bearing(SecretKind),
}

impl SecretVerdict {
    /// Returns the matched family, if any.
    #[must_use]
    pub const fn kind(self) -> Option<SecretKind> {
        match self {
            Self::Clean => None,
            Self::Bearing(kind) => Some(kind),
        }
    }

    /// Reports whether anything matched.
    #[must_use]
    pub const fn is_bearing(self) -> bool {
        matches!(self, Self::Bearing(_))
    }
}

/// One matched span, used by [`Redactor::redact_into`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Span {
    start: usize,
    end: usize,
    kind: SecretKind,
}

/// Denylist scanner over credential-shaped text.
#[derive(Clone, Copy, Debug, Default)]
pub struct Redactor {
    _private: (),
}

const HEADER_KEYWORDS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
];

const ASSIGNMENT_KEYWORDS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "db_password",
    "database_url",
    "aws_access_key_id",
    "aws_secret_access_key",
    "aws_session_token",
    "client_secret",
    "api_key",
    "api-key",
    "apikey",
    "access_token",
    "access-token",
    "accesstoken",
    "refresh_token",
    "refresh-token",
    "refreshtoken",
];

const GITHUB_PREFIXES: &[&str] = &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"];

impl Redactor {
    /// Builds the scanner. Construction is infallible and allocation-free.
    #[must_use]
    pub const fn new() -> Self {
        Self { _private: () }
    }

    /// Reports the first matching denylist family, in a stable order.
    #[must_use]
    pub fn scan(&self, text: &str) -> SecretVerdict {
        match self.spans(text).first() {
            Some(span) => SecretVerdict::Bearing(span.kind),
            None => SecretVerdict::Clean,
        }
    }

    /// Appends `text` to `out` with every matched span replaced by
    /// `[redacted:<kind>]`.
    pub fn redact_into(&self, text: &str, out: &mut String) {
        let mut cursor = 0usize;
        for span in self.spans(text) {
            if span.start < cursor {
                continue;
            }
            out.push_str(&text[cursor..span.start]);
            out.push_str("[redacted:");
            out.push_str(span.kind.label());
            out.push(']');
            cursor = span.end;
        }
        out.push_str(&text[cursor..]);
    }

    /// Convenience wrapper over [`Self::redact_into`].
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        self.redact_into(text, &mut out);
        out
    }

    /// Collects every match, sorted by start offset and de-overlapped.
    fn spans(&self, text: &str) -> Vec<Span> {
        let bytes = text.as_bytes();
        let mut spans = Vec::new();
        scan_private_key(bytes, &mut spans);
        scan_line_prefixed(bytes, &mut spans);
        scan_api_tokens(bytes, &mut spans);
        scan_aws_key_ids(bytes, &mut spans);
        scan_jwts(bytes, &mut spans);
        scan_http_credentials(bytes, &mut spans);
        scan_url_userinfo(bytes, &mut spans);
        spans.sort_by(|left, right| {
            left.start
                .cmp(&right.start)
                .then(right.end.cmp(&left.end))
                .then(left.kind.cmp(&right.kind))
        });
        let mut deduped: Vec<Span> = Vec::with_capacity(spans.len());
        for span in spans {
            if deduped.last().is_some_and(|last| span.start < last.end) {
                continue;
            }
            deduped.push(span);
        }
        deduped
    }
}

const fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// ASCII word boundary, matching RE2's `\b`.
fn at_word_boundary(bytes: &[u8], index: usize) -> bool {
    let before = index.checked_sub(1).and_then(|i| bytes.get(i)).copied();
    let after = bytes.get(index).copied();
    match (before, after) {
        (Some(left), Some(right)) => is_word_byte(left) != is_word_byte(right),
        (Some(left), None) => is_word_byte(left),
        (None, Some(right)) => is_word_byte(right),
        (None, None) => false,
    }
}

fn eq_ignore_ascii_case_at(bytes: &[u8], index: usize, needle: &str) -> bool {
    bytes
        .get(index..index + needle.len())
        .is_some_and(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

/// `(?i)-----BEGIN (?:RSA |EC |OPENSSH |PGP )?PRIVATE KEY-----`
fn scan_private_key(bytes: &[u8], out: &mut Vec<Span>) {
    const HEAD: &str = "-----BEGIN ";
    const TAIL: &str = "PRIVATE KEY-----";
    const ALGORITHMS: &[&str] = &["RSA ", "EC ", "OPENSSH ", "PGP "];
    let mut index = 0usize;
    while index + HEAD.len() <= bytes.len() {
        if !eq_ignore_ascii_case_at(bytes, index, HEAD) {
            index += 1;
            continue;
        }
        let after_head = index + HEAD.len();
        let mut candidates = vec![after_head];
        for algorithm in ALGORITHMS {
            if eq_ignore_ascii_case_at(bytes, after_head, algorithm) {
                candidates.push(after_head + algorithm.len());
            }
        }
        let matched = candidates
            .into_iter()
            .find(|start| eq_ignore_ascii_case_at(bytes, *start, TAIL));
        if let Some(start) = matched {
            out.push(Span {
                start: index,
                end: start + TAIL.len(),
                kind: SecretKind::PrivateKey,
            });
            index = start + TAIL.len();
            continue;
        }
        index += 1;
    }
}

/// Yields `(line_start, line_end_exclusive)` for every line in `bytes`.
fn lines(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            spans.push((start, index));
            start = index + 1;
        }
    }
    spans.push((start, bytes.len()));
    spans
}

/// Skips the `(?m)^\s*[-+ ]?\s*` prefix shared by the two line-anchored Go
/// patterns. Diff markers are treated as syntax, not as protection.
fn skip_diff_prefix(bytes: &[u8], line_start: usize, line_end: usize) -> usize {
    let mut cursor = line_start;
    while cursor < line_end && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor < line_end && matches!(bytes[cursor], b'-' | b'+' | b' ') {
        cursor += 1;
    }
    while cursor < line_end && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    cursor
}

/// The two `(?im)^...`-anchored Go patterns: credential headers and
/// credential assignments.
fn scan_line_prefixed(bytes: &[u8], out: &mut Vec<Span>) {
    for (line_start, line_end) in lines(bytes) {
        let cursor = skip_diff_prefix(bytes, line_start, line_end);
        if let Some(span) = match_header(bytes, cursor, line_end) {
            out.push(span);
            continue;
        }
        if let Some(span) = match_assignment(bytes, cursor, line_end) {
            out.push(span);
        }
    }
}

/// `(?:authorization|proxy-authorization|cookie|set-cookie)\s*:`
fn match_header(bytes: &[u8], cursor: usize, line_end: usize) -> Option<Span> {
    let keyword = HEADER_KEYWORDS
        .iter()
        .filter(|keyword| eq_ignore_ascii_case_at(bytes, cursor, keyword))
        .max_by_key(|keyword| keyword.len())?;
    let mut after = cursor + keyword.len();
    while after < line_end && bytes[after].is_ascii_whitespace() {
        after += 1;
    }
    if bytes.get(after) != Some(&b':') {
        return None;
    }
    Some(Span {
        start: cursor,
        end: line_end,
        kind: SecretKind::CredentialHeader,
    })
}

/// `(?:password|…|refresh[_-]?token)\s*[:=]\s*[^\s#]{8,}`
fn match_assignment(bytes: &[u8], cursor: usize, line_end: usize) -> Option<Span> {
    let keyword = ASSIGNMENT_KEYWORDS
        .iter()
        .filter(|keyword| eq_ignore_ascii_case_at(bytes, cursor, keyword))
        .max_by_key(|keyword| keyword.len())?;
    let mut after = cursor + keyword.len();
    while after < line_end && bytes[after].is_ascii_whitespace() {
        after += 1;
    }
    if !matches!(bytes.get(after), Some(b':' | b'=')) {
        return None;
    }
    after += 1;
    while after < line_end && bytes[after].is_ascii_whitespace() {
        after += 1;
    }
    let value_start = after;
    while after < line_end && !bytes[after].is_ascii_whitespace() && bytes[after] != b'#' {
        after += 1;
    }
    if after - value_start < 8 {
        return None;
    }
    Some(Span {
        start: cursor,
        end: after,
        kind: SecretKind::CredentialAssignment,
    })
}

/// `\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|AIza[0-9A-Za-z_-]{20,})\b`
fn scan_api_tokens(bytes: &[u8], out: &mut Vec<Span>) {
    let mut index = 0usize;
    while index < bytes.len() {
        if !at_word_boundary(bytes, index) {
            index += 1;
            continue;
        }
        let matched = match_token_at(bytes, index, "sk-", 16, |byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
        })
        .or_else(|| {
            GITHUB_PREFIXES.iter().find_map(|prefix| {
                match_token_at(bytes, index, prefix, 20, u8::is_ascii_alphanumeric)
            })
        })
        .or_else(|| {
            match_token_at(bytes, index, "AIza", 20, |byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
            })
        });
        if let Some(end) = matched {
            out.push(Span {
                start: index,
                end,
                kind: SecretKind::ApiToken,
            });
            index = end;
            continue;
        }
        index += 1;
    }
}

/// Matches `prefix` followed by at least `minimum` bytes accepted by `accept`,
/// returning the exclusive end offset of the maximal run.
fn match_token_at(
    bytes: &[u8],
    index: usize,
    prefix: &str,
    minimum: usize,
    accept: impl Fn(&u8) -> bool,
) -> Option<usize> {
    if bytes.get(index..index + prefix.len())? != prefix.as_bytes() {
        return None;
    }
    let mut end = index + prefix.len();
    while bytes.get(end).is_some_and(&accept) {
        end += 1;
    }
    (end - index - prefix.len() >= minimum).then_some(end)
}

/// `\b(?:AKIA|ASIA)[A-Z0-9]{16}\b`
fn scan_aws_key_ids(bytes: &[u8], out: &mut Vec<Span>) {
    const PREFIXES: &[&str] = &["AKIA", "ASIA"];
    let mut index = 0usize;
    while index < bytes.len() {
        if !at_word_boundary(bytes, index) {
            index += 1;
            continue;
        }
        let matched = PREFIXES.iter().find_map(|prefix| {
            let start = index + prefix.len();
            let tail = bytes.get(index..start)?;
            if tail != prefix.as_bytes() {
                return None;
            }
            let body = bytes.get(start..start + 16)?;
            if !body
                .iter()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            {
                return None;
            }
            at_word_boundary(bytes, start + 16).then_some(start + 16)
        });
        if let Some(end) = matched {
            out.push(Span {
                start: index,
                end,
                kind: SecretKind::AwsAccessKeyId,
            });
            index = end;
            continue;
        }
        index += 1;
    }
}

const fn is_base64url_byte(byte: &u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-')
}

/// `\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\b`
fn scan_jwts(bytes: &[u8], out: &mut Vec<Span>) {
    let mut index = 0usize;
    while index < bytes.len() {
        if !at_word_boundary(bytes, index) || !eq_ignore_ascii_case_at(bytes, index, "eyJ") {
            index += 1;
            continue;
        }
        if bytes.get(index..index + 3) != Some(b"eyJ") {
            index += 1;
            continue;
        }
        let segment = |from: usize| {
            let mut end = from;
            while bytes.get(end).is_some_and(is_base64url_byte) {
                end += 1;
            }
            end
        };
        let first = segment(index + 3);
        if first - index - 3 < 8 || bytes.get(first) != Some(&b'.') {
            index += 1;
            continue;
        }
        let second = segment(first + 1);
        if second - first - 1 < 6 || bytes.get(second) != Some(&b'.') {
            index += 1;
            continue;
        }
        let third = segment(second + 1);
        if third - second - 1 < 6 || !at_word_boundary(bytes, third) {
            index += 1;
            continue;
        }
        out.push(Span {
            start: index,
            end: third,
            kind: SecretKind::JsonWebToken,
        });
        index = third;
    }
}

/// `(?i)\b(?:bearer|basic)\s+[A-Za-z0-9._~+/-]{16,}={0,2}\b`
fn scan_http_credentials(bytes: &[u8], out: &mut Vec<Span>) {
    const SCHEMES: &[&str] = &["bearer", "basic"];
    let mut index = 0usize;
    while index < bytes.len() {
        if !at_word_boundary(bytes, index) {
            index += 1;
            continue;
        }
        let matched = SCHEMES.iter().find_map(|scheme| {
            if !eq_ignore_ascii_case_at(bytes, index, scheme) {
                return None;
            }
            let mut cursor = index + scheme.len();
            let space_start = cursor;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            if cursor == space_start {
                return None;
            }
            let body_start = cursor;
            while bytes.get(cursor).is_some_and(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(*byte, b'.' | b'_' | b'~' | b'+' | b'/' | b'-')
            }) {
                cursor += 1;
            }
            if cursor - body_start < 16 {
                return None;
            }
            let mut padding = 0usize;
            while padding < 2 && bytes.get(cursor) == Some(&b'=') {
                cursor += 1;
                padding += 1;
            }
            Some(cursor)
        });
        if let Some(end) = matched {
            out.push(Span {
                start: index,
                end,
                kind: SecretKind::HttpCredential,
            });
            index = end;
            continue;
        }
        index += 1;
    }
}

/// `(?i)\b[a-z][a-z0-9+.-]*://[^\s/@:]+:[^\s/@]+@`
fn scan_url_userinfo(bytes: &[u8], out: &mut Vec<Span>) {
    let mut index = 0usize;
    while index < bytes.len() {
        if !at_word_boundary(bytes, index) || !bytes[index].is_ascii_alphabetic() {
            index += 1;
            continue;
        }
        let mut cursor = index + 1;
        while bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'+' | b'.' | b'-'))
        {
            cursor += 1;
        }
        if bytes.get(cursor..cursor + 3) != Some(b"://") {
            index += 1;
            continue;
        }
        cursor += 3;
        let user_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(*byte, b'/' | b'@' | b':'))
        {
            cursor += 1;
        }
        if cursor == user_start || bytes.get(cursor) != Some(&b':') {
            index += 1;
            continue;
        }
        cursor += 1;
        let password_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(*byte, b'/' | b'@'))
        {
            cursor += 1;
        }
        if cursor == password_start || bytes.get(cursor) != Some(&b'@') {
            index += 1;
            continue;
        }
        out.push(Span {
            start: index,
            end: cursor + 1,
            kind: SecretKind::UrlUserInfo,
        });
        index = cursor + 1;
    }
}

/// Counts invisible-Unicode and homoglyph findings in `text`.
///
/// Delegates to `orchestrator-core`'s port of
/// `skills/orchestrator/internal/sanitize/sanitize.go`, so knowledge admission
/// and the Go worker boundary share one definition of unsafe text.
#[must_use]
pub fn unsafe_text_findings(text: &str) -> usize {
    orchestrator_core::detect_invisible(text).len()
        + orchestrator_core::detect_homoglyphs(text).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One positive and one negative case per Go `secretPatterns` entry
    /// (`internal/advisorbridge/prepare.go:32-43`).
    #[test]
    fn every_go_pattern_family_is_covered() {
        let redactor = Redactor::new();
        let cases: &[(&str, SecretKind)] = &[
            (
                "-----BEGIN OPENSSH PRIVATE KEY-----",
                SecretKind::PrivateKey,
            ),
            ("-----BEGIN PRIVATE KEY-----", SecretKind::PrivateKey),
            ("Authorization: Token abc", SecretKind::CredentialHeader),
            ("+  set-cookie : x=1", SecretKind::CredentialHeader),
            ("sk-abcdefghijklmnopqrstuv", SecretKind::ApiToken),
            ("ghp_abcdefghijklmnopqrstuvwxyz01", SecretKind::ApiToken),
            ("AIzaSyABCDEFGHIJKLMNOPQRSTUVWX", SecretKind::ApiToken),
            ("AKIAIOSFODNN7EXAMPLE", SecretKind::AwsAccessKeyId),
            ("ASIAIOSFODNN7EXAMPLE", SecretKind::AwsAccessKeyId),
            (
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r",
                SecretKind::JsonWebToken,
            ),
            ("Bearer AbCdEfGhIjKlMnOpQrSt", SecretKind::HttpCredential),
            ("basic YWxhZGRpbjpvcGVuc2VzYW1l", SecretKind::HttpCredential),
            (
                "api_key = 0123456789abcdef",
                SecretKind::CredentialAssignment,
            ),
            (
                "- database_url: postgres-value-here",
                SecretKind::CredentialAssignment,
            ),
            ("postgres://user:hunter2@host/db", SecretKind::UrlUserInfo),
        ];
        for (text, expected) in cases {
            assert_eq!(
                redactor.scan(text),
                SecretVerdict::Bearing(*expected),
                "expected {expected:?} for {text:?}",
            );
        }

        for clean in [
            "-----BEGIN CERTIFICATE-----",
            "authorization is granted by policy",
            "sk-short",
            "AKIATOOSHORT",
            "eyJonly",
            "bearer short",
            "api_key = tiny",
            "postgres://host/db",
            "a perfectly ordinary sentence about widgets",
        ] {
            assert_eq!(
                redactor.scan(clean),
                SecretVerdict::Clean,
                "unexpected match for {clean:?}",
            );
        }
    }

    #[test]
    fn redaction_replaces_the_match_and_keeps_the_rest() {
        let redactor = Redactor::new();
        let redacted = redactor.redact("token=sk-abcdefghijklmnopqrstuv trailing");
        assert!(
            !redacted.contains("sk-abcdefghijklmnopqrstuv"),
            "{redacted}"
        );
        assert!(redacted.contains("[redacted:api-token]"), "{redacted}");
        assert!(redacted.ends_with(" trailing"), "{redacted}");
        assert_eq!(redactor.redact("nothing to see"), "nothing to see");
    }

    #[test]
    fn overlapping_matches_do_not_corrupt_the_output() {
        let redactor = Redactor::new();
        let text = "Authorization: Bearer AbCdEfGhIjKlMnOpQrSt";
        let redacted = redactor.redact(text);
        assert!(!redacted.contains("AbCdEfGhIjKlMnOpQrSt"), "{redacted}");
        assert!(redacted.starts_with("[redacted:"), "{redacted}");
    }

    #[test]
    fn redacted_wrapper_hides_its_value_in_both_formats() {
        let held = Redacted::new("AKIAIOSFODNN7EXAMPLE");
        assert_eq!(held.to_string(), "[redacted]");
        assert_eq!(format!("{held:?}"), "Redacted(..)");
        assert_eq!(held.into_inner(), "AKIAIOSFODNN7EXAMPLE");
    }

    #[test]
    fn unsafe_text_reuses_the_shared_sanitizer() {
        assert_eq!(unsafe_text_findings("plain widget text"), 0);
        assert!(unsafe_text_findings("wid\u{200b}get") > 0);
    }
}
