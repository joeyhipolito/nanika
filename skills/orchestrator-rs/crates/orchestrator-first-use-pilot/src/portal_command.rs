//! Parses the Codex `command_execution.command` shell display text into a
//! validated portal helper invocation, without ever executing anything.
//!
//! The outer text is `SHELL FLAG SCRIPT` (e.g. `/bin/zsh -lc '...'`) and the
//! decoded `SCRIPT` must itself be exactly `[exec] HELPER --portal on
//! --output-cap on --log LOG -- /bin/sh -c INNER_SCRIPT`. Both layers are
//! parsed with the same bounded, literal-only shell word lexer: it honors
//! POSIX quoting and backslash escaping but rejects any operator, expansion,
//! redirect, or glob outside of a quoted `INNER_SCRIPT`, since that argument
//! runs inside the helper's own capture and may contain arbitrary shell.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Invocation {
    pub(crate) log: PathBuf,
    pub(crate) script: String,
}

const MAX_COMMAND_BYTES: usize = 65536;

pub(crate) fn parse(command: &str, helper: &Path, log_dir: &Path) -> Result<Invocation, String> {
    if command.is_empty() {
        return Err("command must not be empty".to_string());
    }
    if command.len() > MAX_COMMAND_BYTES {
        return Err("command exceeds the maximum size".to_string());
    }
    if !helper.is_absolute() {
        return Err("helper path must be absolute".to_string());
    }
    if !log_dir.is_absolute() {
        return Err("log dir must be absolute".to_string());
    }
    if has_parent_dir(helper) {
        return Err("helper path must not contain '..'".to_string());
    }
    if has_parent_dir(log_dir) {
        return Err("log dir must not contain '..'".to_string());
    }
    let helper_str = helper.to_str().ok_or("helper path must be UTF-8")?;
    let log_dir_str = log_dir.to_str().ok_or("log dir must be UTF-8")?;

    let outer = lex_words(command)?;
    if outer.len() != 3 {
        return Err("outer command must be exactly SHELL FLAG SCRIPT".to_string());
    }
    match (outer[0].as_str(), outer[1].as_str()) {
        ("/bin/zsh", "-lc") | ("/bin/zsh", "-c") | ("/bin/bash", "-lc") | ("/bin/bash", "-c") => {}
        ("/bin/sh", "-c") => {}
        _ => return Err("unsupported outer shell or flag".to_string()),
    }

    let mut inner = lex_words(&outer[2])?;
    if !inner.is_empty() && inner[0] == "exec" {
        inner.remove(0);
    }
    if inner.len() != 11 {
        return Err("inner script has an unexpected argument shape".to_string());
    }
    if inner[0] != helper_str {
        return Err("helper path mismatch".to_string());
    }
    if inner[1] != "--portal" || inner[2] != "on" {
        return Err("expected '--portal on'".to_string());
    }
    if inner[3] != "--output-cap" || inner[4] != "on" {
        return Err("expected '--output-cap on'".to_string());
    }
    if inner[5] != "--log" {
        return Err("expected '--log'".to_string());
    }
    if inner[7] != "--" {
        return Err("expected '--'".to_string());
    }
    if inner[8] != "/bin/sh" || inner[9] != "-c" {
        return Err("expected '/bin/sh -c'".to_string());
    }

    let log = validate_log_path(&inner[6], log_dir, log_dir_str)?;

    let script = inner[10].clone();
    if script.is_empty() {
        return Err("SCRIPT must not be empty".to_string());
    }
    if script.contains('\0') {
        return Err("SCRIPT must not contain NUL".to_string());
    }

    Ok(Invocation { log, script })
}

fn has_parent_dir(path: &Path) -> bool {
    path.components().any(|c| c == Component::ParentDir)
}

fn validate_log_path(log_arg: &str, log_dir: &Path, log_dir_str: &str) -> Result<PathBuf, String> {
    let prefix = if log_dir_str.ends_with('/') {
        log_dir_str.to_string()
    } else {
        format!("{log_dir_str}/")
    };
    let filename = log_arg
        .strip_prefix(&prefix)
        .ok_or("LOG must be directly inside log dir")?;
    let stem = filename
        .strip_suffix(".log")
        .ok_or("LOG must end with '.log'")?;
    let len = stem.chars().count();
    if len == 0 || len > 64 {
        return Err("LOG filename stem must be 1..64 characters".to_string());
    }
    if !stem
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("LOG filename stem has invalid characters".to_string());
    }
    Ok(log_dir.join(filename))
}

/// Splits `s` into literal shell words. Honors POSIX single quotes, double
/// quotes, and backslash escaping, but rejects any operator, expansion,
/// redirect, glob, or unquoted control character. Adjacent quoted/unquoted
/// fragments concatenate into a single word, matching POSIX word-splitting.
fn lex_words(s: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;

    while i < n {
        let c = chars[i];
        if c == '\0' {
            return Err("NUL byte is not allowed".to_string());
        }
        if c == ' ' || c == '\t' {
            if in_word {
                words.push(std::mem::take(&mut cur));
                in_word = false;
            }
            i += 1;
            continue;
        }
        if c == '\n' || c == '\r' {
            return Err("unquoted newline is not allowed".to_string());
        }
        in_word = true;
        match c {
            '\'' => {
                i += 1;
                loop {
                    if i >= n {
                        return Err("unterminated single quote".to_string());
                    }
                    let ch = chars[i];
                    if ch == '\0' {
                        return Err("NUL byte is not allowed".to_string());
                    }
                    if ch == '\'' {
                        i += 1;
                        break;
                    }
                    cur.push(ch);
                    i += 1;
                }
            }
            '"' => {
                i += 1;
                loop {
                    if i >= n {
                        return Err("unterminated double quote".to_string());
                    }
                    let ch = chars[i];
                    if ch == '\0' {
                        return Err("NUL byte is not allowed".to_string());
                    }
                    if ch == '"' {
                        i += 1;
                        break;
                    }
                    if ch == '$' || ch == '`' {
                        return Err("expansion is not allowed in double quotes".to_string());
                    }
                    if ch == '\\' {
                        if i + 1 >= n {
                            return Err("unterminated escape".to_string());
                        }
                        let next = chars[i + 1];
                        if next == '\0' {
                            return Err("NUL byte is not allowed".to_string());
                        }
                        if next == '\n' {
                            return Err("escaped newline is not allowed".to_string());
                        }
                        if next == '$' || next == '`' || next == '"' || next == '\\' {
                            cur.push(next);
                            i += 2;
                            continue;
                        }
                        cur.push(ch);
                        i += 1;
                        continue;
                    }
                    cur.push(ch);
                    i += 1;
                }
            }
            '\\' => {
                i += 1;
                if i >= n {
                    return Err("trailing backslash".to_string());
                }
                let next = chars[i];
                if next == '\0' || next == '\n' || next == '\r' {
                    return Err("backslash cannot escape this character".to_string());
                }
                cur.push(next);
                i += 1;
            }
            _ => {
                if is_safe_unquoted(c) {
                    cur.push(c);
                    i += 1;
                } else {
                    return Err(format!("unsupported unquoted character '{c}'"));
                }
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

fn is_safe_unquoted(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | ':' | '-' | '=')
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELPER: &str = "/opt/portal/helper";
    const LOG_DIR: &str = "/var/log/portal";

    fn sq(s: &str) -> String {
        let mut out = String::from("'");
        for c in s.chars() {
            if c == '\'' {
                out.push_str("'\\''");
            } else {
                out.push(c);
            }
        }
        out.push('\'');
        out
    }

    fn build(inner_prefix: &str, log: &str, script: &str) -> String {
        format!(
            "{inner_prefix}{HELPER} --portal on --output-cap on --log {log} -- /bin/sh -c {}",
            sq(script)
        )
    }

    fn wrap_outer(inner: &str) -> String {
        format!("/bin/zsh -lc {}", sq(inner))
    }

    #[test]
    fn roundtrip_with_spaces_and_quoting() -> Result<(), String> {
        let script = "echo hi; ls -la | grep foo";
        let log = format!("{LOG_DIR}/run-1.log");
        let inner = build("", &log, script);
        let outer = wrap_outer(&inner);
        let invocation = parse(&outer, Path::new(HELPER), Path::new(LOG_DIR))?;
        if invocation.log != Path::new(&log) {
            return Err("log mismatch".to_string());
        }
        if invocation.script != script {
            return Err("script mismatch".to_string());
        }
        Ok(())
    }

    #[test]
    fn singlequote_escape_sequence_roundtrip() -> Result<(), String> {
        let script = "echo it's a test && rm -rf /tmp/x";
        let log = format!("{LOG_DIR}/run-2.log");
        let inner = build("", &log, script);
        let outer = wrap_outer(&inner);
        let invocation = parse(&outer, Path::new(HELPER), Path::new(LOG_DIR))?;
        if invocation.script != script {
            return Err("script mismatch".to_string());
        }
        Ok(())
    }

    #[test]
    fn optional_exec_prefix_is_accepted() -> Result<(), String> {
        let script = "echo hi";
        let log = format!("{LOG_DIR}/run-3.log");
        let inner = build("exec ", &log, script);
        let outer = wrap_outer(&inner);
        let invocation = parse(&outer, Path::new(HELPER), Path::new(LOG_DIR))?;
        if invocation.script != script {
            return Err("script mismatch".to_string());
        }
        Ok(())
    }

    #[test]
    fn doublequote_expansion_is_rejected() -> Result<(), String> {
        let log = format!("{LOG_DIR}/run-4.log");
        let inner = format!(
            "{HELPER} --portal on --output-cap on --log \"$LOG\" -- /bin/sh -c {}",
            sq("echo hi")
        );
        let _ = log;
        let outer = wrap_outer(&inner);
        match parse(&outer, Path::new(HELPER), Path::new(LOG_DIR)) {
            Err(_) => Ok(()),
            Ok(_) => Err("expected rejection of double-quoted expansion".to_string()),
        }
    }

    #[test]
    fn outer_trailing_pipe_is_rejected() -> Result<(), String> {
        let log = format!("{LOG_DIR}/run-5.log");
        let inner = build("", &log, "echo hi");
        let outer = format!("/bin/zsh -lc {} | cat", sq(&inner));
        match parse(&outer, Path::new(HELPER), Path::new(LOG_DIR)) {
            Err(_) => Ok(()),
            Ok(_) => Err("expected rejection of trailing outer pipe".to_string()),
        }
    }

    #[test]
    fn inner_trailing_command_is_rejected() -> Result<(), String> {
        let log = format!("{LOG_DIR}/run-6.log");
        let inner = format!("{} && rm -rf /", build("", &log, "echo hi"));
        let outer = wrap_outer(&inner);
        match parse(&outer, Path::new(HELPER), Path::new(LOG_DIR)) {
            Err(_) => Ok(()),
            Ok(_) => Err("expected rejection of trailing inner command".to_string()),
        }
    }

    #[test]
    fn wrong_helper_is_rejected() -> Result<(), String> {
        let log = format!("{LOG_DIR}/run-7.log");
        let inner = format!(
            "/opt/portal/other-helper --portal on --output-cap on --log {log} -- /bin/sh -c {}",
            sq("echo hi")
        );
        let outer = wrap_outer(&inner);
        match parse(&outer, Path::new(HELPER), Path::new(LOG_DIR)) {
            Err(_) => Ok(()),
            Ok(_) => Err("expected rejection of mismatched helper".to_string()),
        }
    }

    #[test]
    fn path_traversal_in_log_dir_is_rejected() -> Result<(), String> {
        let log_dir = Path::new("/var/log/../portal");
        let log = format!("{LOG_DIR}/run-8.log");
        let inner = build("", &log, "echo hi");
        let outer = wrap_outer(&inner);
        match parse(&outer, Path::new(HELPER), log_dir) {
            Err(_) => Ok(()),
            Ok(_) => Err("expected rejection of '..' in log dir".to_string()),
        }
    }

    #[test]
    fn reused_suffix_shape_is_rejected() -> Result<(), String> {
        let log = format!("{LOG_DIR}/run-9.log");
        let inner = format!(
            "{HELPER} --log {log} --portal on --output-cap on -- /bin/sh -c {}",
            sq("echo hi")
        );
        let outer = wrap_outer(&inner);
        match parse(&outer, Path::new(HELPER), Path::new(LOG_DIR)) {
            Err(_) => Ok(()),
            Ok(_) => Err("expected rejection of reordered argument shape".to_string()),
        }
    }

    #[test]
    fn overlong_command_is_rejected() -> Result<(), String> {
        let outer = "a".repeat(MAX_COMMAND_BYTES + 1);
        match parse(&outer, Path::new(HELPER), Path::new(LOG_DIR)) {
            Err(_) => Ok(()),
            Ok(_) => Err("expected rejection of overlong command".to_string()),
        }
    }

    #[test]
    fn unclosed_quote_is_rejected() -> Result<(), String> {
        let outer = "/bin/zsh -lc 'unclosed";
        match parse(outer, Path::new(HELPER), Path::new(LOG_DIR)) {
            Err(_) => Ok(()),
            Ok(_) => Err("expected rejection of unclosed quote".to_string()),
        }
    }

    #[test]
    fn unquoted_newline_outside_script_is_rejected() -> Result<(), String> {
        let outer = "/bin/zsh\n-lc 'echo hi'";
        match parse(outer, Path::new(HELPER), Path::new(LOG_DIR)) {
            Err(_) => Ok(()),
            Ok(_) => Err("expected rejection of unquoted newline".to_string()),
        }
    }

    #[test]
    fn literal_newline_inside_script_is_accepted() -> Result<(), String> {
        let script = "echo hi\necho bye";
        let log = format!("{LOG_DIR}/run-10.log");
        let inner = build("", &log, script);
        let outer = wrap_outer(&inner);
        let invocation = parse(&outer, Path::new(HELPER), Path::new(LOG_DIR))?;
        if invocation.script != script {
            return Err("script mismatch".to_string());
        }
        Ok(())
    }
}
