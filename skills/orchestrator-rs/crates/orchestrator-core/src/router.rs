//! Provider routing brain, ported from the Go orchestrator snapshot
//! `go-orchestrator-20260717` (`internal/router/router.go` and
//! `internal/core/runtime.go`; PROVENANCE.txt: `dd5aeb27` + 98 uncommitted
//! dirty entries — see `PORT-ORDER.md` §0.0/§0.3 for why the snapshot, not a
//! bare commit, is the oracle for this package).
//!
//! Pure tier classification, built-in tier->model tables, effort resolution,
//! the escalation ladder, and the `SelectRuntime` policy. No I/O; the
//! `NANIKA_CODEX_AUTO` environment value is supplied by the caller so the
//! policy remains deterministic and testable.

use std::fmt;

/// Model complexity tier. `Apex` is the escalation-only ceiling and is never
/// assigned by [`classify_tier`]; phases reach it only via [`escalate_tier`].
/// `BudgetDeep` is likewise never assigned by [`classify_tier`] or the retry
/// ladder — operators opt into it explicitly in authored mission phases (Go
/// `router.go`'s `TierBudgetDeep` doc comment).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub enum ModelTier {
    Think,
    #[default]
    Work,
    General,
    Quick,
    BudgetDeep,
    Apex,
}

impl ModelTier {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Think => "think",
            Self::Work => "work",
            Self::General => "general",
            Self::Quick => "quick",
            Self::BudgetDeep => "budget-deep",
            Self::Apex => "apex",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "think" => Self::Think,
            "work" => Self::Work,
            "general" => Self::General,
            "quick" => Self::Quick,
            "budget-deep" => Self::BudgetDeep,
            "apex" => Self::Apex,
            _ => return None,
        })
    }
}

impl fmt::Display for ModelTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Returns the runtime after applying the Go zero-value rule: empty means claude.
#[must_use]
pub fn effective_runtime(runtime: &str) -> &str {
    if runtime.is_empty() {
        "claude"
    } else {
        runtime
    }
}

/// Built-in Claude tier->model table (Go `ClaudeModelID`, July 2026 lineup).
#[must_use]
pub const fn claude_model(tier: ModelTier) -> &'static str {
    match tier {
        ModelTier::Think | ModelTier::BudgetDeep | ModelTier::Apex => "opus",
        ModelTier::Work | ModelTier::General => "sonnet",
        ModelTier::Quick => "haiku",
    }
}

/// Built-in Codex tier->model table (Go `CodexModelID`, GPT-5.6 family).
#[must_use]
pub const fn codex_model(tier: ModelTier) -> &'static str {
    match tier {
        ModelTier::Think | ModelTier::Work | ModelTier::Apex => "gpt-5.6-sol",
        ModelTier::General => "gpt-5.6-terra",
        ModelTier::Quick | ModelTier::BudgetDeep => "gpt-5.6-luna",
    }
}

/// Go `ResolveForRuntime`: the Codex table when the effective runtime is codex,
/// otherwise the Claude table.
#[must_use]
pub fn resolve_model_for_runtime(tier: ModelTier, runtime: &str) -> &'static str {
    if effective_runtime(runtime) == "codex" {
        codex_model(tier)
    } else {
        claude_model(tier)
    }
}

/// Go `ResolveEffort` (Claude effort scale).
#[must_use]
pub fn resolve_effort(tier: ModelTier, persona: &str) -> &'static str {
    if is_high_effort_persona(persona) {
        return "high";
    }
    match tier {
        ModelTier::Think | ModelTier::Apex => "high",
        ModelTier::Quick => "low",
        ModelTier::Work | ModelTier::General | ModelTier::BudgetDeep => "medium",
    }
}

/// Go `ResolveEffortForRuntime`: for the Codex runtime this is a byte-exact
/// port of `router.go`'s Codex `switch tier` block, which — unlike
/// [`resolve_effort`] — has **no persona high-effort gate**; the `persona`
/// argument is only consulted on the non-Codex fallback path. Apex and
/// BudgetDeep both escalate to `xhigh`; Think is `high`; Quick is `low`; all
/// other tiers (Work, General) fall to the `default: return "medium"` arm.
#[must_use]
pub fn resolve_effort_for_runtime(tier: ModelTier, persona: &str, runtime: &str) -> &'static str {
    if effective_runtime(runtime) == "codex" {
        match tier {
            ModelTier::Apex | ModelTier::BudgetDeep => "xhigh",
            ModelTier::Think => "high",
            ModelTier::Quick => "low",
            ModelTier::Work | ModelTier::General => "medium",
        }
    } else {
        resolve_effort(tier, persona)
    }
}

/// Go `EscalateTier`: quick/general -> work -> think -> apex (terminal);
/// budget-deep and apex are both terminal at apex.
#[must_use]
pub const fn escalate_tier(tier: ModelTier) -> ModelTier {
    match tier {
        ModelTier::Quick | ModelTier::General => ModelTier::Work,
        ModelTier::Work => ModelTier::Think,
        ModelTier::Think | ModelTier::BudgetDeep | ModelTier::Apex => ModelTier::Apex,
    }
}

const THINK_SIGNALS: &[&str] = &[
    "architect",
    "design",
    "plan",
    "security",
    "audit",
    "threat model",
    "vulnerability",
    "review architecture",
    "system design",
    "data model",
    "api design",
];

const GENERAL_SIGNALS: &[&str] = &[
    "research",
    "summarize",
    "synthes",
    "write article",
    "write post",
    "write documentation",
    "document",
    "compare",
    "explain",
    "translate",
];

const QUICK_SIGNALS: &[&str] = &[
    "format",
    "rename",
    "fix typo",
    "update comment",
    "add docstring",
    "simple fix",
    "minor change",
];

/// Go `ClassifyTier`. Uses plain case-insensitive substring matching on the
/// task (not the heuristic-phrase matcher). Signal order matches
/// `router.go` exactly: think signals, then persona-based escalation, then
/// general-work signals (skipped when the task says "write code"), then
/// quick signals, defaulting to work.
#[must_use]
pub fn classify_tier(task: &str, persona: &str) -> ModelTier {
    let lower = task.to_ascii_lowercase();
    if THINK_SIGNALS.iter().any(|signal| lower.contains(signal)) {
        return ModelTier::Think;
    }
    match persona {
        "architect" | "security-auditor" | "staff-code-reviewer" => return ModelTier::Think,
        "reviewer" => {
            if lower.contains("security") || lower.contains("vulnerab") {
                return ModelTier::Think;
            }
            return ModelTier::Work;
        }
        _ => {}
    }
    if !lower.contains("write code") && GENERAL_SIGNALS.iter().any(|signal| lower.contains(signal))
    {
        return ModelTier::General;
    }
    if QUICK_SIGNALS.iter().any(|signal| lower.contains(signal)) {
        return ModelTier::Quick;
    }
    ModelTier::Work
}

/// Go `ClassifyComplexity`: long or multi-step tasks need orchestration.
#[must_use]
pub fn classify_complexity(task: &str) -> bool {
    let lower = task.to_ascii_lowercase();
    if task.split_whitespace().count() > 20 {
        return true;
    }
    const COMPLEX_SIGNALS: &[&str] = &[
        "and then",
        "then ",
        "after that",
        "first ",
        "finally ",
        "research and",
        "write and",
        "implement and",
        "build and",
        "multiple",
        "several",
        "phases",
        "steps",
        "plan and execute",
        "design and implement",
    ];
    COMPLEX_SIGNALS.iter().any(|signal| lower.contains(signal))
}

const NON_CODE_TASK_KEYWORDS: &[&str] = &[
    "research",
    "investigate",
    "analyze",
    "analyse",
    "explore",
    "study",
    "document",
    "documentation",
    "readme",
    "changelog",
    "design doc",
    "blog post",
    "newsletter",
    "article",
    "write blog",
    "draft post",
    "content creation",
    "narration",
    "linkedin post",
    "reddit post",
    "deploy",
    "release",
    "rollout",
];

const CODE_TASK_KEYWORDS: &[&str] = &[
    "implement",
    "build",
    "create",
    "fix",
    "refactor",
    "optimize",
    "write code",
    "add",
    "update",
    "parser",
    "handler",
    "endpoint",
    "api",
    "component",
    "service",
    "function",
    "integration",
    "test",
    "bug",
    "cache",
    "cli",
    "schema",
    "migration",
    "migrate",
];

const LANGUAGE_SPECIALIST_TOKENS: &[&str] = &[
    "golang",
    "go",
    "python",
    "typescript",
    "javascript",
    "rust",
    "java",
    "ruby",
    "swift",
    "kotlin",
    "cpp",
    "csharp",
];

/// Go `SelectRuntime`. `codex_auto_env` is the value of `NANIKA_CODEX_AUTO`.
/// Call only for phases whose RUNTIME field was not explicitly authored.
#[must_use]
pub fn select_runtime(role: &str, persona: &str, task: &str, codex_auto_env: &str) -> &'static str {
    match role {
        "reviewer" | "planner" => "claude",
        "implementer" => select_for_implementer(persona, task, codex_auto_env),
        _ => "claude",
    }
}

fn select_for_implementer(persona: &str, task: &str, auto_mode: &str) -> &'static str {
    if matches!(auto_mode, "" | "0" | "false" | "no") {
        return "claude";
    }
    if contains_any_heuristic_phrase(task, NON_CODE_TASK_KEYWORDS) {
        return "claude";
    }
    if matches!(auto_mode, "aggressive" | "2") {
        return if contains_any_heuristic_phrase(task, CODE_TASK_KEYWORDS) {
            "codex"
        } else {
            "claude"
        };
    }
    if contains_any_heuristic_phrase(persona, LANGUAGE_SPECIALIST_TOKENS)
        && contains_any_heuristic_phrase(task, CODE_TASK_KEYWORDS)
    {
        return "codex";
    }
    "claude"
}

fn is_high_effort_persona(persona: &str) -> bool {
    matches!(
        persona,
        "architect" | "security-auditor" | "staff-code-reviewer"
    )
}

fn contains_any_heuristic_phrase(text: &str, phrases: &[&str]) -> bool {
    phrases
        .iter()
        .any(|phrase| contains_heuristic_phrase(text, phrase))
}

/// Go `containsHeuristicPhrase`: word-boundary match on normalized text, so
/// "cli" does not match "client" while multi-word phrases like "blog post" do.
fn contains_heuristic_phrase(text: &str, phrase: &str) -> bool {
    let normalized_text = normalize_heuristic_text(text);
    let normalized_phrase = normalize_heuristic_text(phrase);
    if normalized_text.is_empty() || normalized_phrase.is_empty() {
        return false;
    }
    let mut padded = String::with_capacity(normalized_text.len() + 2);
    padded.push(' ');
    padded.push_str(&normalized_text);
    padded.push(' ');
    let needle = format!(" {} ", normalized_phrase);
    padded.contains(&needle)
}

fn normalize_heuristic_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_space = true;
    for rune in value.to_lowercase().chars() {
        if rune.is_alphanumeric() {
            out.push(rune);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_TIERS: [ModelTier; 6] = [
        ModelTier::Think,
        ModelTier::Work,
        ModelTier::General,
        ModelTier::Quick,
        ModelTier::BudgetDeep,
        ModelTier::Apex,
    ];

    #[test]
    fn tier_parse_and_display_round_trip_all_variants() {
        for tier in ALL_TIERS {
            assert_eq!(ModelTier::parse(tier.as_str()), Some(tier));
            assert_eq!(tier.to_string(), tier.as_str());
        }
        assert_eq!(ModelTier::parse("bogus"), None);
    }

    #[test]
    fn model_tables_match_go() {
        // ClaudeModelID (router.go:35-42).
        assert_eq!(claude_model(ModelTier::Think), "opus");
        assert_eq!(claude_model(ModelTier::Work), "sonnet");
        assert_eq!(claude_model(ModelTier::General), "sonnet");
        assert_eq!(claude_model(ModelTier::Quick), "haiku");
        assert_eq!(claude_model(ModelTier::BudgetDeep), "opus");
        assert_eq!(claude_model(ModelTier::Apex), "opus");

        // CodexModelID (router.go:48-55).
        assert_eq!(codex_model(ModelTier::Think), "gpt-5.6-sol");
        assert_eq!(codex_model(ModelTier::Work), "gpt-5.6-sol");
        assert_eq!(codex_model(ModelTier::General), "gpt-5.6-terra");
        assert_eq!(codex_model(ModelTier::Quick), "gpt-5.6-luna");
        assert_eq!(codex_model(ModelTier::BudgetDeep), "gpt-5.6-luna");
        assert_eq!(codex_model(ModelTier::Apex), "gpt-5.6-sol");
    }

    #[test]
    fn resolve_model_for_runtime_picks_the_right_table() {
        assert_eq!(
            resolve_model_for_runtime(ModelTier::Work, "codex"),
            "gpt-5.6-sol"
        );
        assert_eq!(resolve_model_for_runtime(ModelTier::Work, ""), "sonnet");
        assert_eq!(
            resolve_model_for_runtime(ModelTier::Quick, "claude"),
            "haiku"
        );
        assert_eq!(
            resolve_model_for_runtime(ModelTier::General, "codex"),
            "gpt-5.6-terra"
        );
        assert_eq!(
            resolve_model_for_runtime(ModelTier::BudgetDeep, "codex"),
            "gpt-5.6-luna"
        );
    }

    /// Table-driven parity test asserting every (tier, runtime, persona) ->
    /// (model, effort) pair against `router.go`'s values, byte-for-byte.
    #[test]
    fn tier_runtime_persona_parity_table_matches_snapshot_router_go() {
        struct Case {
            tier: ModelTier,
            runtime: &'static str,
            persona: &'static str,
            model: &'static str,
            effort: &'static str,
        }
        let cases = [
            // Claude runtime — ResolveEffort (router.go:108-122).
            Case {
                tier: ModelTier::Think,
                runtime: "claude",
                persona: "",
                model: "opus",
                effort: "high",
            },
            Case {
                tier: ModelTier::Work,
                runtime: "claude",
                persona: "",
                model: "sonnet",
                effort: "medium",
            },
            Case {
                tier: ModelTier::General,
                runtime: "claude",
                persona: "",
                model: "sonnet",
                effort: "medium",
            },
            Case {
                tier: ModelTier::Quick,
                runtime: "claude",
                persona: "",
                model: "haiku",
                effort: "low",
            },
            Case {
                tier: ModelTier::BudgetDeep,
                runtime: "claude",
                persona: "",
                model: "opus",
                effort: "medium",
            },
            Case {
                tier: ModelTier::Apex,
                runtime: "claude",
                persona: "",
                model: "opus",
                effort: "high",
            },
            // Claude runtime, high-effort persona overrides tier (router.go:109-112).
            Case {
                tier: ModelTier::Work,
                runtime: "claude",
                persona: "architect",
                model: "sonnet",
                effort: "high",
            },
            Case {
                tier: ModelTier::Quick,
                runtime: "claude",
                persona: "security-auditor",
                model: "haiku",
                effort: "high",
            },
            Case {
                tier: ModelTier::General,
                runtime: "claude",
                persona: "staff-code-reviewer",
                model: "sonnet",
                effort: "high",
            },
            // Codex runtime — ResolveEffortForRuntime (router.go:128-146): no persona gate.
            Case {
                tier: ModelTier::Think,
                runtime: "codex",
                persona: "",
                model: "gpt-5.6-sol",
                effort: "high",
            },
            Case {
                tier: ModelTier::Work,
                runtime: "codex",
                persona: "",
                model: "gpt-5.6-sol",
                effort: "medium",
            },
            Case {
                tier: ModelTier::General,
                runtime: "codex",
                persona: "",
                model: "gpt-5.6-terra",
                effort: "medium",
            },
            Case {
                tier: ModelTier::Quick,
                runtime: "codex",
                persona: "",
                model: "gpt-5.6-luna",
                effort: "low",
            },
            Case {
                tier: ModelTier::BudgetDeep,
                runtime: "codex",
                persona: "",
                model: "gpt-5.6-luna",
                effort: "xhigh",
            },
            Case {
                tier: ModelTier::Apex,
                runtime: "codex",
                persona: "",
                model: "gpt-5.6-sol",
                effort: "xhigh",
            },
            // Codex runtime, persona is ignored (would be high-effort on Claude).
            Case {
                tier: ModelTier::Work,
                runtime: "codex",
                persona: "architect",
                model: "gpt-5.6-sol",
                effort: "medium",
            },
            Case {
                tier: ModelTier::Quick,
                runtime: "codex",
                persona: "staff-code-reviewer",
                model: "gpt-5.6-luna",
                effort: "low",
            },
            // Empty runtime resolves to claude (router.go's Runtime.Effective zero rule).
            Case {
                tier: ModelTier::Work,
                runtime: "",
                persona: "",
                model: "sonnet",
                effort: "medium",
            },
        ];
        for case in cases {
            assert_eq!(
                resolve_model_for_runtime(case.tier, case.runtime),
                case.model,
                "model mismatch for tier={:?} runtime={} persona={}",
                case.tier,
                case.runtime,
                case.persona
            );
            assert_eq!(
                resolve_effort_for_runtime(case.tier, case.persona, case.runtime),
                case.effort,
                "effort mismatch for tier={:?} runtime={} persona={}",
                case.tier,
                case.runtime,
                case.persona
            );
        }
    }

    #[test]
    fn escalate_climbs_and_terminates() {
        assert_eq!(escalate_tier(ModelTier::Quick), ModelTier::Work);
        assert_eq!(escalate_tier(ModelTier::General), ModelTier::Work);
        assert_eq!(escalate_tier(ModelTier::Work), ModelTier::Think);
        assert_eq!(escalate_tier(ModelTier::Think), ModelTier::Apex);
        assert_eq!(escalate_tier(ModelTier::BudgetDeep), ModelTier::Apex);
        assert_eq!(escalate_tier(ModelTier::Apex), ModelTier::Apex);
    }

    #[test]
    fn classify_tier_matches_go_signals() {
        assert_eq!(classify_tier("design the API", ""), ModelTier::Think);
        assert_eq!(classify_tier("audit security model", ""), ModelTier::Think);
        assert_eq!(
            classify_tier("write a function", "architect"),
            ModelTier::Think
        );
        assert_eq!(classify_tier("review the pr", "reviewer"), ModelTier::Work);
        assert_eq!(
            classify_tier("review for vulnerability", "reviewer"),
            ModelTier::Think
        );
        assert_eq!(classify_tier("fix typo in docs", ""), ModelTier::Quick);
        assert_eq!(classify_tier("format the file", ""), ModelTier::Quick);
        assert_eq!(classify_tier("implement the handler", ""), ModelTier::Work);
        // General signals (router.go:178-188).
        assert_eq!(
            classify_tier("please research the topic", ""),
            ModelTier::General
        );
        assert_eq!(
            classify_tier("write documentation for this", ""),
            ModelTier::General
        );
        assert_eq!(
            classify_tier("please explain the results", ""),
            ModelTier::General
        );
        // "write code" suppresses the general-signal match even if another
        // general signal is also present.
        assert_eq!(
            classify_tier("write code to compare two files", ""),
            ModelTier::Work
        );
    }

    #[test]
    fn classify_complexity_matches_go() {
        assert!(classify_complexity("research and then write a report"));
        assert!(classify_complexity("first do this finally do that"));
        let long = "word ".repeat(21);
        assert!(classify_complexity(&long));
        assert!(!classify_complexity("fix the bug"));
    }

    #[test]
    fn select_runtime_reviews_and_plans_stay_on_claude() {
        assert_eq!(select_runtime("reviewer", "", "fix bug", "1"), "claude");
        assert_eq!(
            select_runtime("planner", "", "plan the work", "1"),
            "claude"
        );
        assert_eq!(select_runtime("unknown", "", "fix bug", "1"), "claude");
    }

    #[test]
    fn select_runtime_env_gate_forces_claude() {
        // Disabled by default and by explicit opt-out.
        assert_eq!(
            select_runtime("implementer", "golang", "implement parser", ""),
            "claude"
        );
        assert_eq!(
            select_runtime("implementer", "golang", "implement parser", "0"),
            "claude"
        );
    }

    #[test]
    fn select_runtime_default_needs_language_persona_and_code_signal() {
        assert_eq!(
            select_runtime("implementer", "golang", "implement the parser", "1"),
            "codex"
        );
        // Generic persona stays on claude even with a code signal.
        assert_eq!(
            select_runtime(
                "implementer",
                "backend-engineer",
                "implement the parser",
                "1"
            ),
            "claude"
        );
        // Language persona without a code signal stays on claude.
        assert_eq!(
            select_runtime("implementer", "golang", "research the options", "1"),
            "claude"
        );
    }

    #[test]
    fn select_runtime_aggressive_routes_any_code_signal() {
        assert_eq!(
            select_runtime(
                "implementer",
                "backend-engineer",
                "fix the bug",
                "aggressive"
            ),
            "codex"
        );
        // Non-code tasks stay on claude even in aggressive mode.
        assert_eq!(
            select_runtime(
                "implementer",
                "backend-engineer",
                "research options",
                "aggressive"
            ),
            "claude"
        );
    }

    #[test]
    fn heuristic_phrase_respects_word_boundaries() {
        // "cli" must not match "client".
        assert!(!contains_heuristic_phrase("build a client form", "cli"));
        assert!(contains_heuristic_phrase("build a cli tool", "cli"));
        // Multi-word phrase.
        assert!(contains_heuristic_phrase(
            "write a blog post now",
            "blog post"
        ));
        // Normalization: punctuation splits tokens.
        assert!(contains_heuristic_phrase("fix, the bug", "fix"));
    }
}
