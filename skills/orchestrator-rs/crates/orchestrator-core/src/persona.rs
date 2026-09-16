//! Persona file loading and frontmatter stripping.
//!
//! Ports the minimal subset of Go's `internal/persona/personas.go` needed by
//! the worker-spawn layer: read a persona `.md` file from the personas
//! directory, strip YAML frontmatter, and return the prompt body.
//!
//! Hot-reload, keyword routing, MEMORY.md, and learning-focus extraction are
//! separate concerns ported in later waves.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// The personas directory under the nanika root.
pub const PERSONAS_SUBDIR: &str = "personas";

/// Returns the default personas directory: `<nanika_dir>/personas/`.
#[must_use]
pub fn default_personas_dir(nanika_dir: &Path) -> PathBuf {
    nanika_dir.join(PERSONAS_SUBDIR)
}

/// Loads a persona's prompt body from `personas_dir/<name>.md`, stripping any
/// YAML frontmatter. Falls back to `<name>/<name>.md` if the flat file is
/// absent. Returns `None` if the persona file does not exist.
///
/// Ports Go's `Get(name).PromptBody` (or `Content` when `PromptBody` is empty).
pub fn load_persona_prompt(personas_dir: &Path, name: &str) -> Option<String> {
    // Try flat file: <personas_dir>/<name>.md
    let flat = personas_dir.join(format!("{name}.md"));
    if flat.is_file() {
        let content = fs::read_to_string(&flat).ok()?;
        return Some(strip_frontmatter(&content));
    }
    // Try directory convention: <personas_dir>/<name>/<name>.md
    let nested = personas_dir.join(name).join(format!("{name}.md"));
    if nested.is_file() {
        let content = fs::read_to_string(&nested).ok()?;
        return Some(strip_frontmatter(&content));
    }
    None
}

/// Strips YAML frontmatter from a persona file's content.
///
/// Frontmatter is an optional block at the very start of the file delimited by
/// `---\n` on its own line. If present, everything up to and including the
/// closing `---\n` is removed. If absent or malformed, the original content is
/// returned unchanged.
fn strip_frontmatter(content: &str) -> String {
    let content = content.strip_prefix("---\n").unwrap_or(content);
    if let Some(end) = content.find("\n---\n") {
        return content[end + "\n---\n".len()..]
            .trim_start_matches('\n')
            .to_owned();
    }
    // If the file starts with --- but has no closing delimiter, treat the
    // whole file as the body (Go's parseFrontmatter does the same).
    content.to_owned()
}

/// Lists all persona names available in `personas_dir`, sorted. Includes both
/// flat `<name>.md` files and `<name>/<name>.md` directory entries.
pub fn list_persona_names(personas_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(entries) = fs::read_dir(personas_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if path.is_file() && name.ends_with(".md") {
                    names.push(name.trim_end_matches(".md").to_owned());
                } else if path.is_dir() {
                    let nested = path.join(format!("{name}.md"));
                    if nested.is_file() {
                        names.push(name.to_owned());
                    }
                }
            }
        }
    }
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn strip_frontmatter_removes_yaml_block() {
        let input =
            "---\nrole: implementer\ncapabilities:\n  - rust\n---\nYou are a Rust engineer.\n";
        let result = strip_frontmatter(input);
        assert_eq!(result, "You are a Rust engineer.\n");
    }

    #[test]
    fn strip_frontmatter_returns_content_when_no_frontmatter() {
        let input = "You are an architect.\n";
        let result = strip_frontmatter(input);
        assert_eq!(result, "You are an architect.\n");
    }

    #[test]
    fn strip_frontmatter_handles_unclosed_frontmatter() {
        let input = "---\nrole: implementer\nYou are a Rust engineer.\n";
        let result = strip_frontmatter(input);
        assert!(result.contains("You are a Rust engineer."));
    }

    #[test]
    fn load_persona_prompt_reads_flat_file() -> std::io::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "orchestrator-rs-persona-flat-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir)?;
        let mut file = fs::File::create(dir.join("architect.md"))?;
        writeln!(file, "---\nrole: planner\n---\nYou are a system architect.")?;
        let prompt = load_persona_prompt(&dir, "architect");
        assert!(
            prompt
                .as_deref()
                .is_some_and(|p| p.starts_with("You are a system architect.")),
            "expected architect prompt body, got {prompt:?}"
        );
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn load_persona_prompt_reads_nested_directory() -> std::io::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "orchestrator-rs-persona-nested-{}",
            std::process::id()
        ));
        let nested = dir.join("lifekeeper");
        fs::create_dir_all(&nested)?;
        fs::write(
            nested.join("lifekeeper.md"),
            "You are a lifekeeper persona.",
        )?;
        let prompt = load_persona_prompt(&dir, "lifekeeper");
        assert_eq!(prompt.as_deref(), Some("You are a lifekeeper persona."));
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn load_persona_prompt_returns_none_for_missing() {
        let dir = std::env::temp_dir().join("nonexistent-persona-dir-12345");
        let prompt = load_persona_prompt(&dir, "ghost");
        assert!(prompt.is_none());
    }

    #[test]
    fn list_persona_names_sorts_and_includes_both_kinds() -> std::io::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "orchestrator-rs-persona-list-{}",
            std::process::id()
        ));
        fs::create_dir_all(dir.join("nested"))?;
        fs::write(dir.join("nested").join("nested.md"), "nested")?;
        fs::write(dir.join("alpha.md"), "alpha")?;
        fs::write(dir.join("beta.md"), "beta")?;
        fs::write(dir.join("not-a-persona.txt"), "ignored")?;
        let names = list_persona_names(&dir);
        assert_eq!(names, vec!["alpha", "beta", "nested"]);
        fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
