//! Deterministic supplied-text routing; no quota feedback, retries or escalation.
use orchestrator_core::{
    ModelTier, classify_tier, resolve_effort_for_runtime, resolve_model_for_runtime,
};
use orchestrator_exec::Effort;
use serde_json::{Value, json};

use crate::{PilotCommand, PilotOptions};

pub(crate) struct Route {
    pub tier: ModelTier,
    pub persona: String,
    pub model: String,
    pub effort: Effort,
    pub selection_reason: &'static str,
}

/// Removes explicitly prohibited instruction clauses before tier classification.
/// Clause boundaries are deliberately limited to sentence punctuation, semicolons,
/// and line breaks; this is a routing guard, not natural-language parsing.
fn classification_task(task: &str) -> String {
    task.split_inclusive(['.', '!', '?', ';', '\n', '\r'])
        .filter(|clause| !starts_with_negative_instruction(clause))
        .collect()
}

fn starts_with_negative_instruction(clause: &str) -> bool {
    let start = strip_markdown_bullet(clause.trim_start()).trim_start();
    starts_with_words(start, "do", Some("not"))
        || starts_with_words(start, "never", None)
        || starts_with_words(start, "don't", None)
}

fn strip_markdown_bullet(text: &str) -> &str {
    if let Some(rest) = text
        .get(1..)
        .filter(|_| matches!(text.as_bytes().first(), Some(b'-' | b'*' | b'+')))
    {
        if rest.starts_with(char::is_whitespace) {
            return rest;
        }
    }

    let digit_count = text.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count > 0 {
        let rest = &text[digit_count..];
        if let Some(after_marker) = rest
            .strip_prefix('.')
            .or_else(|| rest.strip_prefix(')'))
            .filter(|rest| rest.starts_with(char::is_whitespace))
        {
            return after_marker;
        }
    }
    text
}

fn starts_with_words(text: &str, first: &str, second: Option<&str>) -> bool {
    let Some(mut rest) = strip_ascii_case_prefix(text, first) else {
        return false;
    };
    if let Some(second) = second {
        if !rest.starts_with(char::is_whitespace) {
            return false;
        }
        rest = rest.trim_start();
        let Some(after_second) = strip_ascii_case_prefix(rest, second) else {
            return false;
        };
        rest = after_second;
    }
    match rest.chars().next() {
        Some(character) => !character.is_alphanumeric(),
        None => true,
    }
}

fn strip_ascii_case_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let candidate = text.get(..prefix.len())?;
    candidate
        .eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

pub(crate) fn select(options: &PilotOptions, task: &str) -> Route {
    let codex = options.runtime == "codex";
    let coding = options.command == PilotCommand::Code;
    let persona = options.persona.as_deref().unwrap_or(if codex || coding {
        "general-purpose"
    } else {
        "reviewer"
    });
    let tier = classify_tier(&classification_task(task), persona);
    let effort = if codex || coding {
        match resolve_effort_for_runtime(tier, persona, &options.runtime) {
            "low" => Effort::Low,
            "medium" => Effort::Medium,
            "high" => Effort::High,
            "xhigh" => Effort::XHigh,
            _ => unreachable!("core router returned an unsupported effort"),
        }
    } else {
        Effort::High
    };
    let (model, selection_reason) = if !options.model.is_empty() {
        (
            options.model.clone(),
            "explicit --model override; tier classified by existing deterministic supplied-text heuristic",
        )
    } else if codex || coding {
        (
            resolve_model_for_runtime(tier, &options.runtime).to_owned(),
            "existing deterministic supplied-text heuristic and persona policy; core runtime model and effort resolution",
        )
    } else {
        (
            String::new(),
            "Claude CLI default model and pilot high effort preserved; tier is descriptive only",
        )
    };
    Route {
        tier,
        persona: persona.to_owned(),
        model,
        effort,
        selection_reason,
    }
}

impl Route {
    pub fn record(&self) -> Value {
        json!({
            "tier": self.tier.as_str(), "persona": self.persona,
            "model": self.model, "effort": self.effort.as_str(),
            "selection_reason": self.selection_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_arguments;

    fn options(extra: &[&str]) -> Result<PilotOptions, crate::PilotError> {
        parse_arguments(
            [
                "code",
                "--repo",
                "repo",
                "--prompt-file",
                "prompt",
                "--output-dir",
                "output",
            ]
            .into_iter()
            .chain(extra.iter().copied()),
        )
    }

    #[test]
    fn negative_formatter_clause_does_not_make_retry_handler_quick() -> Result<(), crate::PilotError>
    {
        let route = select(
            &options(&[])?,
            "Implement a retry handler. Do not run formatters.",
        );

        assert_eq!(route.tier, ModelTier::Work);
        assert_eq!(route.model, "gpt-5.6-sol");
        assert_eq!(route.effort, Effort::Medium);
        Ok(())
    }

    #[test]
    fn positive_work_after_leading_negative_clause_is_preserved() -> Result<(), crate::PilotError> {
        let route = select(
            &options(&[])?,
            "  - DON'T format the source. Implement a retry handler.",
        );

        assert_eq!(route.tier, ModelTier::Work);
        assert_eq!(route.model, "gpt-5.6-sol");
        assert_eq!(route.effort, Effort::Medium);
        Ok(())
    }

    #[test]
    fn genuinely_positive_signals_still_route_normally() -> Result<(), crate::PilotError> {
        for (task, tier, model, effort) in [
            (
                "Format the source.",
                ModelTier::Quick,
                "gpt-5.6-luna",
                Effort::Low,
            ),
            ("Simple fix.", ModelTier::Quick, "gpt-5.6-luna", Effort::Low),
            (
                "Explain the retry behavior.",
                ModelTier::General,
                "gpt-5.6-terra",
                Effort::Medium,
            ),
            (
                "Review architecture.",
                ModelTier::Think,
                "gpt-5.6-sol",
                Effort::High,
            ),
        ] {
            let route = select(&options(&[])?, task);

            assert_eq!(route.tier, tier, "task: {task}");
            assert_eq!(route.model, model, "task: {task}");
            assert_eq!(route.effort, effort, "task: {task}");
        }
        Ok(())
    }

    #[test]
    fn negative_architecture_review_does_not_escalate_coding() -> Result<(), crate::PilotError> {
        let route = select(&options(&[])?, "  * NeVeR review architecture.");

        assert_eq!(route.tier, ModelTier::Work);
        assert_eq!(route.model, "gpt-5.6-sol");
        assert_eq!(route.effort, Effort::Medium);
        Ok(())
    }

    #[test]
    fn staff_code_reviewer_persona_still_selects_think() -> Result<(), crate::PilotError> {
        let route = select(
            &options(&["--persona", "staff-code-reviewer"])?,
            "Simple fix.",
        );

        assert_eq!(route.tier, ModelTier::Think);
        assert_eq!(route.model, "gpt-5.6-sol");
        assert_eq!(route.effort, Effort::High);
        Ok(())
    }

    #[test]
    fn explicit_model_override_remains_exact() -> Result<(), crate::PilotError> {
        let route = select(
            &options(&["--model", "operator/model-exact"])?,
            "Implement a retry handler. Do not format the source.",
        );

        assert_eq!(route.tier, ModelTier::Work);
        assert_eq!(route.model, "operator/model-exact");
        assert_eq!(route.effort, Effort::Medium);
        Ok(())
    }
}
