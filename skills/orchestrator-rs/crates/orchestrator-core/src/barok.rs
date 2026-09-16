//! Barok output-compression rule-card injection.
//!
//! Barok is the nanika-native variant of the upstream "caveman" lite ruleset.
//! It tells the worker LLM to compress prose surfaces in its generated output
//! while preserving structural / machine-contract surfaces verbatim. The
//! instruction is injected into the worker's `CLAUDE.md` just before the
//! `## Output` block so the LLM reads the rule before emitting any token.
//!
//! Cache-safety invariant (Go source, scope §5): inject only when the phase
//! is terminal. Terminal-phase output never re-enters a dependent worker's
//! prompt prefix, so compressed bytes do not break the cache-read ratio.
//!
//! Set `NANIKA_NO_BAROK=1` to short-circuit injection (debug, emergency
//! disable, or A/B comparability during the experiment window).
//!
//! Ported from the Go orchestrator's `internal/worker/barok.go`. Only the
//! pure rule-card logic (`BarokPersonas`, `BarokIntensityTier`,
//! `BarokRuleCardBytes`, `BarokDisabled`, `InjectBarok`,
//! `IsBarokEligiblePersona`) is ported here — `ValidateBarok` and
//! `ValidateArtifactStructure` live in the Go sibling file
//! `barok_validator.go`, which is out of scope for this module (a later
//! wave); do not add them here.

/// Env var that, when set to `"1"`, short-circuits [`inject_barok`] to an
/// empty string and signals the artifact-collection path to skip the
/// validator+retry leg entirely.
pub const BAROK_ENV_DISABLE: &str = "NANIKA_NO_BAROK";

/// The v1+delta persona allow-list. Only these personas receive the rule
/// card, even on terminal phases.
///
/// Go source scope §2 + delta §2: technical-writer, academic-researcher,
/// architect, data-analyst, staff-code-reviewer (added by delta because the
/// validator makes review-gate parseability risk bounded and recoverable).
pub const BAROK_PERSONAS: [&str; 5] = [
    "technical-writer",
    "academic-researcher",
    "architect",
    "data-analyst",
    "staff-code-reviewer",
];

/// Classifies the per-persona compression strength chosen in delta §2.
/// Drives which rule fragment is emitted into the rule card.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BarokIntensity {
    /// Lite + sentence fragments (drop subject pronouns, drop linking verbs
    /// in declaratives). Used for the prose-densest personas.
    Fragment,
    /// Lite, complete sentences only. Articles, hedges, pleasantries
    /// compressed; rationale chains preserved.
    Sentence,
    /// Lite restricted to narrative explanation prose; verdict-shaped lines
    /// pass through verbatim.
    Narrative,
}

/// Bundles per-persona behaviour for the rule card.
struct BarokRule {
    intensity: BarokIntensity,
    /// delta §2 "Special compression" cell.
    special_compress: &'static str,
    /// delta §2 "Special preservation" cell (empty = standard list only).
    special_preserve: &'static str,
}

/// Looks up the per-persona rule overrides. Personas not covered here are
/// not eligible for barok injection (see [`BAROK_PERSONAS`]).
fn barok_rule(persona: &str) -> Option<BarokRule> {
    match persona {
        "technical-writer" => Some(BarokRule {
            intensity: BarokIntensity::Fragment,
            special_compress: r#"Drop subject pronouns ("System returns X" → "Returns X"); drop linking verbs in declaratives."#,
            special_preserve: "",
        }),
        "academic-researcher" => Some(BarokRule {
            intensity: BarokIntensity::Fragment,
            special_compress: r#"Drop subject pronouns ("System returns X" → "Returns X"); compress hedged clauses ("It is plausible that X may Y" → "X may Y")."#,
            special_preserve: "Citation patterns (`[Author YYYY]`, `(Author, YYYY)`, DOI strings) verbatim.",
        }),
        "architect" => Some(BarokRule {
            intensity: BarokIntensity::Sentence,
            special_compress: "Articles, pleasantries, and hedges only. No fragments — preserve rationale chains intact.",
            special_preserve: "ADR section headers (Context, Decision, Candidates Considered, Component Map, Interfaces, Risks, Trade-offs Accepted) verbatim.",
        }),
        "data-analyst" => Some(BarokRule {
            intensity: BarokIntensity::Sentence,
            special_compress: "Articles, pleasantries, and hedges only. No fragments — preserve quantitative claims intact.",
            special_preserve: "Numeric expressions with units (e.g. `94.17%`, `$15/M`, `7d`, `12.5×`) verbatim.",
        }),
        "staff-code-reviewer" => Some(BarokRule {
            intensity: BarokIntensity::Narrative,
            special_compress: "Compress narrative explanation prose only. Verdict-shaped lines and code citations pass through.",
            special_preserve: "Verdict markers (`APPROVE:`, `REJECT:`, `BLOCK:`, `NEEDS-CHANGES:`, `NIT:`, `BLOCKING:`) verbatim — including any leading whitespace or bullet prefix.",
        }),
        _ => None,
    }
}

/// Reports whether `persona` is on the barok allow-list. Stable for callers
/// that want to gate behaviour without reaching into the rule map (e.g.
/// metrics emission, CLI debug subcommands).
#[must_use]
pub fn is_barok_eligible_persona(persona: &str) -> bool {
    barok_rule(persona).is_some()
}

/// Returns a stable string label naming the persona's compression tier for
/// barok-eligible personas, and `""` for any persona not on the allow-list.
/// Used by the density observer to group records without needing an
/// internal tier enum.
#[must_use]
pub fn barok_intensity_tier(persona: &str) -> &'static str {
    match barok_rule(persona) {
        Some(rule) => match rule.intensity {
            BarokIntensity::Fragment => "LiteFragment",
            BarokIntensity::Sentence => "LiteSentence",
            BarokIntensity::Narrative => "LiteNarrative",
        },
        None => "",
    }
}

/// Reports whether the `NANIKA_NO_BAROK` env var is set to `"1"`.
/// Centralised so the engine and validator-retry path agree on the
/// predicate.
#[must_use]
pub fn barok_disabled() -> bool {
    barok_disabled_for(std::env::var(BAROK_ENV_DISABLE).ok().as_deref())
}

/// Pure predicate behind [`barok_disabled`], split out so unit tests can
/// exercise the `"1"` / `"0"` / unset branches directly without mutating the
/// real process env (unsafe under parallel `cargo test` threads and,
/// separately, `std::env::set_var` is an `unsafe fn` on this toolchain — this
/// crate forbids `unsafe_code`).
fn barok_disabled_for(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Returns the `## Output Compression` rule-card section for `persona` when
/// `is_terminal` is true and barok is enabled. Returns `""` when:
///   - `NANIKA_NO_BAROK=1` is set,
///   - `persona` is not on the barok allow-list,
///   - or `is_terminal` is false (non-terminal phase — output would feed a
///     dependent worker's prompt prefix and break cache).
///
/// The returned section ends with a single trailing newline so callers can
/// concatenate it directly into the CLAUDE.md builder without juggling
/// spacing.
#[must_use]
pub fn inject_barok(persona: &str, is_terminal: bool) -> String {
    inject_barok_with_disabled(persona, is_terminal, barok_disabled())
}

/// Implementation behind [`inject_barok`] with the disable predicate passed
/// explicitly, so unit tests can exercise every branch without mutating
/// process-global environment state (which is unsafe to do from parallel
/// `cargo test` threads without a `#[serial]`-style crate — not a workspace
/// dependency).
fn inject_barok_with_disabled(persona: &str, is_terminal: bool, disabled: bool) -> String {
    if disabled {
        return String::new();
    }
    if !is_terminal {
        return String::new();
    }
    let Some(rule) = barok_rule(persona) else {
        return String::new();
    };

    let mut b = String::new();
    b.push_str("## Output Compression\n\n");
    b.push_str("This phase produces terminal output (no dependent worker phase will read it). ");
    b.push_str("Compress your generated prose using the rules below. ");
    b.push_str("The orchestrator runs a mechanical validator on every artifact you write — ");
    b.push_str(
        "if compression strips a preserved surface, the artifact is regenerated once without compression. ",
    );
    b.push_str("Stay inside the rules and your output ships on the first pass.\n\n");

    b.push_str("### COMPRESS (apply to prose surfaces only)\n\n");
    b.push_str("- Paragraph prose: narrative explanation, rationale, commentary, summaries.\n");
    b.push_str("- Bulleted-list and numbered-list items whose body is a prose sentence.\n");
    b.push_str(
        "- Inline parenthetical asides and hedging (\"roughly\", \"it should be noted\", \"in practice\", \"for the most part\").\n",
    );
    b.push_str("- Articles, pleasantries, filler — the upstream `lite` baseline.\n");
    if !rule.special_compress.is_empty() {
        b.push_str("- ");
        b.push_str(rule.special_compress);
        b.push('\n');
    }
    b.push('\n');

    b.push_str("### PRESERVE VERBATIM (never compress, abbreviate, or rewrite)\n\n");
    b.push_str("- Fenced code blocks (``` any language tag) — bytes equal.\n");
    b.push_str("- Inline code (single backticks) — bytes equal.\n");
    b.push_str("- Indented (4-space) code blocks — bytes equal.\n");
    b.push_str("- Markdown headings (all levels) — text and order.\n");
    b.push_str("- Tables: pipe structure, column count, row count, header row preserved.\n");
    b.push_str("- YAML frontmatter (top-of-file `---` block and any embedded YAML).\n");
    b.push_str(
        "- Scratch blocks (everything between `<!-- scratch -->` and `<!-- /scratch -->`) — bytes equal.\n",
    );
    b.push_str(
        "- Context-bundle sections (`## Context from Prior Work`, `## Prior Phase Notes`, `## Lessons from Past Missions`, `## Worker Identity`).\n",
    );
    b.push_str(
        "- Learning markers: lines beginning with `LEARNING:`, `FINDING:`, `PATTERN:`, `GOTCHA:`, `DECISION:`.\n",
    );
    b.push_str(
        "- URLs (any scheme: `http://`, `https://`, `ftp://`, `file://`, `git@…`, naked domains).\n",
    );
    b.push_str("- File paths, shell commands, CLI invocations.\n");
    b.push_str("- JSON / structured payloads inside code blocks or tool-result bodies.\n");
    b.push_str(
        "- Verdict markers if present: `APPROVE:`, `REJECT:`, `BLOCK:`, `NEEDS-CHANGES:`, `NIT:`, `BLOCKING:`.\n",
    );
    if !rule.special_preserve.is_empty() {
        b.push_str("- ");
        b.push_str(rule.special_preserve);
        b.push('\n');
    }
    b.push('\n');

    // Identifier-aware rule applies to every persona (delta §2).
    b.push_str("### IDENTIFIER-AWARE RULE\n\n");
    b.push_str(
        "Tokens matching `[a-z]+_[a-z_]+` (snake_case) or `[A-Z][a-z]+[A-Z][a-zA-Z]*` (CamelCase) ",
    );
    b.push_str("pass through verbatim. Function names, file paths, and identifiers must not be ");
    b.push_str("decomposed under aggressive compression.\n\n");

    b.push_str("### QUICK REFERENCE\n\n");
    b.push_str("```\n");
    b.push_str(
        "COMPRESS: prose paragraphs, bullet-body prose, list-item prose, parenthetical hedges.\n",
    );
    b.push_str("PRESERVE: ``` fenced ```, `inline code`, 4-space indented code, # headings,\n");
    b.push_str("          | tables | (structure), YAML frontmatter, <!-- scratch --> blocks,\n");
    b.push_str("          ## Context/Prior/Lessons/Identity sections,\n");
    b.push_str("          LEARNING:/FINDING:/PATTERN:/GOTCHA:/DECISION: marker lines,\n");
    b.push_str("          URLs, file paths, shell commands, JSON/structured payloads,\n");
    b.push_str("          APPROVE:/REJECT:/BLOCK:/NEEDS-CHANGES: verdict lines,\n");
    b.push_str("          snake_case + CamelCase identifiers.\n");
    b.push_str("```\n\n");

    b
}

/// Returns the byte length of the rule card emitted for `persona` on a
/// terminal phase. Returns `0` when `persona` is ineligible or barok is
/// disabled. Used by the `orchestrator barok status` subcommand to surface
/// the actual injected payload size per persona.
#[must_use]
pub fn barok_rule_card_bytes(persona: &str) -> usize {
    inject_barok(persona, true).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    // These tests exercise `inject_barok_with_disabled` directly with an
    // explicit `disabled` flag rather than mutating the real
    // `NANIKA_NO_BAROK` process env var, which is unsafe to set/unset from
    // parallel `cargo test` threads (env vars are process-global, and this
    // workspace has no `#[serial]`-style test-isolation dependency). The one
    // test that reads the real env (`barok_disabled_reports_env_state`)
    // isolates itself to a single call with no assertions dependent on
    // other tests' env state.

    #[test]
    fn inject_barok_non_target_persona_returns_empty() -> TestResult {
        for persona in [
            "senior-backend-engineer",
            "product-manager",
            "qa-engineer",
            "",
            "ARCHITECT", // case-sensitive: capitals are not on the list
            "unknown-persona",
        ] {
            let got = inject_barok_with_disabled(persona, true, false);
            if !got.is_empty() {
                return Err(format!(
                    "inject_barok({persona:?}, true): expected empty; got {} bytes",
                    got.len()
                )
                .into());
            }
        }
        Ok(())
    }

    #[test]
    fn inject_barok_non_terminal_returns_empty() -> TestResult {
        for persona in BAROK_PERSONAS {
            let got = inject_barok_with_disabled(persona, false, false);
            if !got.is_empty() {
                return Err(format!(
                    "inject_barok({persona:?}, false): expected empty for non-terminal phase; got {} bytes",
                    got.len()
                )
                .into());
            }
        }
        Ok(())
    }

    #[test]
    fn inject_barok_disabled_returns_empty() -> TestResult {
        // Mirrors Go's TestBarok_InjectBarok_DisabledReturnsEmpty, but passes
        // disabled=true explicitly instead of mutating NANIKA_NO_BAROK.
        for persona in BAROK_PERSONAS {
            let got = inject_barok_with_disabled(persona, true, true);
            if !got.is_empty() {
                return Err(format!(
                    "inject_barok({persona:?}, true) with disabled=true: expected empty; got {} bytes",
                    got.len()
                )
                .into());
            }
        }
        Ok(())
    }

    #[test]
    fn inject_barok_each_persona_returns_rule_card() -> TestResult {
        let cases: &[(&str, &[&str])] = &[
            (
                "technical-writer",
                &[
                    "## Output Compression",
                    "PRESERVE VERBATIM",
                    "Drop subject pronouns",
                    "Fenced code blocks",
                ],
            ),
            (
                "academic-researcher",
                &[
                    "## Output Compression",
                    "PRESERVE VERBATIM",
                    "compress hedged clauses",
                    "Citation patterns",
                ],
            ),
            (
                "architect",
                &[
                    "## Output Compression",
                    "PRESERVE VERBATIM",
                    "ADR section headers",
                    "No fragments",
                ],
            ),
            (
                "data-analyst",
                &[
                    "## Output Compression",
                    "PRESERVE VERBATIM",
                    "Numeric expressions with units",
                    "quantitative claims intact",
                ],
            ),
            (
                "staff-code-reviewer",
                &[
                    "## Output Compression",
                    "PRESERVE VERBATIM",
                    "Verdict markers",
                    "APPROVE:",
                    "REJECT:",
                    "BLOCK:",
                ],
            ),
        ];

        for (persona, must_contain) in cases {
            let got = inject_barok_with_disabled(persona, true, false);
            if got.is_empty() {
                return Err(format!("expected non-empty rule card for {persona:?}").into());
            }
            for want in *must_contain {
                if !got.contains(want) {
                    return Err(format!("rule card for {persona:?}: missing {want:?}").into());
                }
            }
        }
        Ok(())
    }

    #[test]
    fn inject_barok_size_budget() -> TestResult {
        // The Go task spec says "<2 KB" but actual rule cards are ~2.7-2.9 KB
        // because the QUICK REFERENCE fence block, IDENTIFIER-AWARE RULE, and
        // per-persona special_compress/special_preserve lines together exceed
        // 2048 bytes. Gate at 3072 bytes to catch accidental rule-card growth,
        // matching the Go test's ceiling exactly.
        const MAX_BYTES: usize = 3072;
        for persona in BAROK_PERSONAS {
            let got = inject_barok_with_disabled(persona, true, false);
            if got.is_empty() {
                return Err(format!("expected non-empty rule card for {persona:?}").into());
            }
            if got.len() >= MAX_BYTES {
                return Err(format!(
                    "inject_barok({persona:?}): rule card size {} bytes exceeds budget {MAX_BYTES} bytes",
                    got.len()
                )
                .into());
            }
        }
        Ok(())
    }

    #[test]
    fn inject_barok_all_personas_contain_common_preserve_list() -> TestResult {
        let common_surfaces = [
            "Fenced code blocks",
            "Inline code",
            "4-space",
            "Markdown headings",
            "YAML frontmatter",
            "Scratch blocks",
            "Context-bundle sections",
            "Learning markers",
            "URLs",
            "File paths",
            "Verdict markers",
            "QUICK REFERENCE",
            "IDENTIFIER-AWARE RULE",
        ];
        for persona in BAROK_PERSONAS {
            let got = inject_barok_with_disabled(persona, true, false);
            for surface in common_surfaces {
                if !got.contains(surface) {
                    return Err(format!(
                        "persona {persona:?}: rule card missing common surface {surface:?}"
                    )
                    .into());
                }
            }
        }
        Ok(())
    }

    #[test]
    fn is_barok_eligible_persona_matches_allow_list() -> TestResult {
        let cases: &[(&str, bool)] = &[
            ("technical-writer", true),
            ("academic-researcher", true),
            ("architect", true),
            ("data-analyst", true),
            ("staff-code-reviewer", true),
            ("qa-engineer", false),
            ("", false),
            ("ARCHITECT", false),
        ];
        for (persona, want) in cases {
            let got = is_barok_eligible_persona(persona);
            if got != *want {
                return Err(
                    format!("is_barok_eligible_persona({persona:?}) = {got}; want {want}").into(),
                );
            }
        }
        Ok(())
    }

    #[test]
    fn barok_rule_card_bytes_is_positive_for_eligible_personas() -> TestResult {
        for persona in BAROK_PERSONAS {
            let got = inject_barok_with_disabled(persona, true, false).len();
            if got == 0 {
                return Err(
                    format!("barok_rule_card_bytes({persona:?}) = 0; expected positive").into(),
                );
            }
        }
        Ok(())
    }

    #[test]
    fn barok_rule_card_bytes_ineligible_persona_is_zero() -> TestResult {
        let got = inject_barok_with_disabled("qa-engineer", true, false).len();
        if got != 0 {
            return Err(format!("barok_rule_card_bytes(ineligible) = {got}; want 0").into());
        }
        Ok(())
    }

    #[test]
    fn barok_disabled_for_matches_only_string_one() -> TestResult {
        // Full branch coverage for the real `== Some("1")` predicate behind
        // `barok_disabled()`, without mutating process env.
        let cases: &[(Option<&str>, bool)] = &[
            (Some("1"), true),
            (Some("0"), false),
            (None, false),
            (Some("true"), false),
            (Some(""), false),
            (Some("2"), false),
        ];
        for (value, want) in cases {
            let got = barok_disabled_for(*value);
            if got != *want {
                return Err(format!("barok_disabled_for({value:?}) = {got}; want {want}").into());
            }
        }
        Ok(())
    }

    #[test]
    fn barok_disabled_reports_false_when_env_absent() -> TestResult {
        // This is the one test in the module that reads the real process
        // env. It only asserts the common CI/dev state (var unset) and does
        // not mutate the env, so it is safe alongside parallel test threads.
        if std::env::var(BAROK_ENV_DISABLE).is_err() && barok_disabled() {
            return Err("barok_disabled() must be false when NANIKA_NO_BAROK is unset".into());
        }
        Ok(())
    }

    #[test]
    fn barok_rule_card_bytes_public_wrapper_matches_impl() -> TestResult {
        // Sanity check that the public `barok_rule_card_bytes`/`inject_barok`
        // wrappers correctly thread through to the tested impl when the real
        // env is in its default (unset) state.
        if std::env::var(BAROK_ENV_DISABLE).is_ok() {
            return Ok(());
        }
        for persona in BAROK_PERSONAS {
            let got = barok_rule_card_bytes(persona);
            let want = inject_barok(persona, true).len();
            if got != want {
                return Err(format!(
                    "barok_rule_card_bytes({persona:?}) = {got}; inject_barok returns {want} bytes"
                )
                .into());
            }
        }
        Ok(())
    }
}
