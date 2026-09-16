//! Reasoning discipline injection.
//!
//! The discipline layer encodes the meta-cognitive protocol observed in
//! frontier reasoning models (e.g., Claude Fable 5's system prompt). It is
//! not about WHAT to produce (that's the persona's job) or HOW MUCH to write
//! (that's barok's job) — it's about HOW TO THINK before acting.
//!
//! Five gates, executed in order before any work begins:
//!
//!  1. Scope — understand the problem boundary before touching code
//!  2. Evidence — read the actual state before forming opinions
//!  3. Adversarial — challenge your own first instinct
//!  4. Verify — run it, don't assume it
//!  5. Calibrate — match effort to task weight
//!
//! Default-on (like barok). Set `NANIKA_NO_DISCIPLINE=1` to disable. The
//! discipline section is injected immediately after the persona identity so
//! it frames all subsequent reasoning. Unlike barok (terminal-only), the
//! discipline gates apply to every phase — they are universal.
//!
//! Ported from the Go orchestrator's `internal/worker/discipline.go`. The
//! Go test file also exercises `BuildCLAUDEmd` (defined in the sibling file
//! `claudemd.go`, which pulls in `internal/core`'s `ContextBundle`) — that
//! integration is out of scope for this pure module and is not ported here.

/// Env var that, when set to `"1"`, short-circuits discipline injection.
/// Mirrors barok's `NANIKA_NO_BAROK` pattern.
pub const DISCIPLINE_ENV_DISABLE: &str = "NANIKA_NO_DISCIPLINE";

/// Reports whether the discipline layer is explicitly disabled via
/// `NANIKA_NO_DISCIPLINE=1`.
#[must_use]
pub fn discipline_disabled() -> bool {
    discipline_disabled_for(std::env::var(DISCIPLINE_ENV_DISABLE).ok().as_deref())
}

/// Pure predicate behind [`discipline_disabled`], split out so unit tests can
/// exercise the `"1"` / `"0"` / unset branches directly without mutating the
/// real process env (unsafe under parallel `cargo test` threads and,
/// separately, `std::env::set_var` is an `unsafe fn` on this toolchain — this
/// crate forbids `unsafe_code`).
fn discipline_disabled_for(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Returns the reasoning-discipline section when enabled and not skipped.
/// Returns `""` when `NANIKA_NO_DISCIPLINE=1` is set or `skip` is true.
///
/// The section is designed to be model-agnostic: it encodes the thinking
/// habits of frontier reasoning models without relying on any specific
/// model's system prompt. The gates are sequenced so each one compounds on
/// the previous — scope constrains evidence, evidence grounds the
/// adversarial check, the adversarial check prevents premature closure,
/// verification catches what reasoning missed, and calibration ensures the
/// effort matches the stakes.
#[must_use]
pub fn inject_discipline(skip: bool) -> String {
    inject_discipline_with_disabled(skip, discipline_disabled())
}

/// Implementation behind [`inject_discipline`] with the disable predicate
/// passed explicitly, so unit tests can exercise every branch without
/// mutating process-global environment state (unsafe under parallel
/// `cargo test` threads without a `#[serial]`-style crate — not a workspace
/// dependency).
fn inject_discipline_with_disabled(skip: bool, disabled: bool) -> String {
    if skip || disabled {
        return String::new();
    }

    let mut b = String::new();
    b.push_str("## Reasoning Discipline\n\n");
    b.push_str("Before writing any output, run through these five gates in order. ");
    b.push_str("Each gate constrains the next — do not skip ahead.\n\n");

    // Gate 1: Scope
    b.push_str("### 1. Scope before working\n\n");
    b.push_str("State what the task is asking, what it is NOT asking, and what constraints ");
    b.push_str("already exist (existing patterns, prior phase output, project conventions). ");
    b.push_str("If the task is ambiguous, make the most reasonable interpretation and proceed — ");
    b.push_str("do not ask for clarification unless the ambiguity is truly blocking. ");
    b.push_str("One question maximum, and only after attempting an answer.\n\n");

    // Gate 2: Evidence
    b.push_str("### 2. Evidence before reasoning\n\n");
    b.push_str("Read the actual files, code, or data before forming opinions. ");
    b.push_str(
        "Check that referenced things exist (imports, functions, config keys, prior phase artifacts). ",
    );
    b.push_str("A prompt implying a file is present does not mean one is — verify. ");
    b.push_str("Partial recognition from training does not mean current knowledge — confirm. ");
    b.push_str("Ground every claim in something you just read, not something you remember.\n\n");

    // Gate 3: Adversarial
    b.push_str("### 3. Reason adversarially\n\n");
    b.push_str("Challenge your first instinct. Before committing to an approach, ask: ");
    b.push_str("\"What's the simplest thing that could work? What would break if I'm wrong? ");
    b.push_str("What am I assuming that might not be true?\" ");
    b.push_str("If two approaches seem equally valid, pick the one that is easier to undo. ");
    b.push_str("Prefer the boring solution that you understand over the clever one you don't. ");
    b.push_str(
        "If a Prior Attempt Failures note is present, treat every approach it lists as proven dead — ",
    );
    b.push_str(
        "diagnose why it failed and take a genuinely different path, do not retry it with cosmetic changes.\n\n",
    );

    // Gate 4: Verify
    b.push_str("### 4. Verify before declaring done\n\n");
    b.push_str("Run the code. Execute the commands. Check the output matches your claims. ");
    b.push_str("Do not describe what should happen — show what did happen. ");
    b.push_str("If you wrote code, compile it. If you wrote queries, run them. ");
    b.push_str("If you made a claim about behavior, demonstrate it. ");
    b.push_str(
        "Acknowledge what went wrong and fix it — stay on the problem until it actually works, ",
    );
    b.push_str(
        "with one exception: if the root cause lives in an earlier phase's decision (wrong tool, ",
    );
    b.push_str(
        "incompatible dependency), do not build shims around it — signal `blocked_by_upstream` ",
    );
    b.push_str("with a concrete remedy instead. Persistence on your own work is a virtue; ");
    b.push_str("persistence against an upstream mistake is waste.\n\n");

    // Gate 5: Calibrate
    b.push_str("### 5. Calibrate effort to task weight\n\n");
    b.push_str("Match your effort to the task's complexity and stakes. ");
    b.push_str("Signal facts need one check. Medium tasks need three to five supporting checks. ");
    b.push_str("Deep research or comparison needs five to ten sources or angles. ");
    b.push_str("Do not over-invest in trivial tasks; do not under-invest in critical ones. ");
    b.push_str("When in doubt, spend more time understanding and less time producing.\n\n");

    // Standing habits
    b.push_str("### Standing habits\n\n");
    b.push_str("- Answer first, then provide context. Lead with the finding, not the process.\n");
    b.push_str(
        "- When something breaks, acknowledge it plainly. No deflection, no blame-shifting.\n",
    );
    b.push_str("- Maintain self-respect in corrections: fix the error, don't grovel about it.\n");
    b.push_str("- If you don't know something, say so and then find out. Do not confabulate.\n");
    b.push_str(
        "- Leave a check behind for non-trivial logic — the smallest thing that fails if the logic breaks.\n\n",
    );

    b
}

/// Returns the byte length of the discipline rule card. Returns `0` when
/// disabled via `NANIKA_NO_DISCIPLINE=1`.
#[must_use]
pub fn discipline_rule_card_bytes() -> usize {
    inject_discipline(false).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    // As in barok.rs, these tests exercise `inject_discipline_with_disabled`
    // directly rather than mutating the real `NANIKA_NO_DISCIPLINE` process
    // env var (unsafe under parallel `cargo test` threads).

    #[test]
    fn inject_discipline_default_on() -> TestResult {
        let section = inject_discipline_with_disabled(false, false);
        if section.is_empty() {
            return Err(
                "inject_discipline must return non-empty by default — discipline is default-on"
                    .into(),
            );
        }
        Ok(())
    }

    #[test]
    fn inject_discipline_disabled_via_env() -> TestResult {
        if !inject_discipline_with_disabled(false, true).is_empty() {
            return Err("inject_discipline must return empty when disabled=true".into());
        }
        Ok(())
    }

    #[test]
    fn inject_discipline_content() -> TestResult {
        let section = inject_discipline_with_disabled(false, false);

        let required = [
            "## Reasoning Discipline",
            "Scope before working",
            "Evidence before reasoning",
            "Reason adversarially",
            "Verify before declaring done",
            "Calibrate effort",
            "Standing habits",
            "do not ask for clarification",
            "Run the code",
            "Do not confabulate",
        ];
        for s in required {
            if !section.contains(s) {
                return Err(format!("discipline section missing: {s:?}").into());
            }
        }
        Ok(())
    }

    #[test]
    fn inject_discipline_skip_flag() -> TestResult {
        if !inject_discipline_with_disabled(true, false).is_empty() {
            return Err(
                "must return empty when skip=true, even though discipline is default-on".into(),
            );
        }
        Ok(())
    }

    #[test]
    fn inject_discipline_skip_flag_overrides_disabled() -> TestResult {
        if !inject_discipline_with_disabled(true, true).is_empty() {
            return Err("must return empty when both skip=true and disabled=true".into());
        }
        Ok(())
    }

    #[test]
    fn inject_discipline_gate_order() -> TestResult {
        let section = inject_discipline_with_disabled(false, false);

        let gates = [
            "Scope before working",
            "Evidence before reasoning",
            "Reason adversarially",
            "Verify before declaring done",
            "Calibrate effort",
        ];
        let mut pos: Option<usize> = None;
        for (i, gate) in gates.iter().enumerate() {
            let idx = section
                .find(gate)
                .ok_or_else(|| format!("gate {i} ({gate:?}) not found"))?;
            if i > 0 {
                if let Some(prev) = pos {
                    if idx <= prev {
                        return Err(format!(
                            "gate {gate:?} (pos {idx}) must appear after gate {:?} (pos {prev})",
                            gates[i - 1]
                        )
                        .into());
                    }
                }
            }
            pos = Some(idx);
        }
        Ok(())
    }

    #[test]
    fn discipline_disabled_for_matches_only_string_one() -> TestResult {
        // Full branch coverage for the real `== Some("1")` predicate behind
        // `discipline_disabled()`, without mutating process env.
        let cases: &[(Option<&str>, bool)] = &[
            (Some("1"), true),
            (Some("0"), false),
            (None, false),
            (Some("true"), false),
            (Some(""), false),
            (Some("2"), false),
        ];
        for (value, want) in cases {
            let got = discipline_disabled_for(*value);
            if got != *want {
                return Err(
                    format!("discipline_disabled_for({value:?}) = {got}; want {want}").into(),
                );
            }
        }
        Ok(())
    }

    #[test]
    fn discipline_disabled_reports_false_when_env_absent() -> TestResult {
        // Sole test in this module reading the real process env — asserts
        // only the common CI/dev state (var unset), never mutates it.
        if std::env::var(DISCIPLINE_ENV_DISABLE).is_err() && discipline_disabled() {
            return Err(
                "discipline_disabled() must be false when NANIKA_NO_DISCIPLINE is unset".into(),
            );
        }
        Ok(())
    }

    #[test]
    fn discipline_rule_card_bytes_is_not_too_small() -> TestResult {
        let n = inject_discipline_with_disabled(false, false).len();
        if n < 1000 {
            return Err(format!("discipline card seems too small: {n} bytes").into());
        }
        Ok(())
    }

    #[test]
    fn discipline_rule_card_bytes_is_zero_when_disabled() -> TestResult {
        let n = inject_discipline_with_disabled(false, true).len();
        if n != 0 {
            return Err(format!("must be 0 when disabled; got {n}").into());
        }
        Ok(())
    }

    #[test]
    fn public_wrappers_match_impl_when_env_default() -> TestResult {
        if std::env::var(DISCIPLINE_ENV_DISABLE).is_ok() {
            return Ok(());
        }
        let got = discipline_rule_card_bytes();
        let want = inject_discipline(false).len();
        if got != want {
            return Err(format!(
                "discipline_rule_card_bytes() = {got}; inject_discipline(false) returns {want} bytes"
            )
            .into());
        }
        Ok(())
    }
}
