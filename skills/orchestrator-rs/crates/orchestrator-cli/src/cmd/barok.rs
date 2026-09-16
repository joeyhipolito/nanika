//! `orchestrator barok status` output.

use std::io::{self, Write};

use orchestrator_core::{BAROK_ENV_DISABLE, BAROK_PERSONAS, barok_disabled, barok_rule_card_bytes};

pub(crate) fn run(output: &mut impl Write) -> io::Result<()> {
    let disabled = barok_disabled();
    let env_value = std::env::var(BAROK_ENV_DISABLE).unwrap_or_default();
    let rule_card_bytes = std::array::from_fn(|index| barok_rule_card_bytes(BAROK_PERSONAS[index]));
    write_status(output, disabled, &env_value, &rule_card_bytes)
}

fn write_status(
    output: &mut impl Write,
    disabled: bool,
    env_value: &str,
    rule_card_bytes: &[usize; BAROK_PERSONAS.len()],
) -> io::Result<()> {
    writeln!(output, "barok output-compression status")?;
    writeln!(output, "================================")?;
    if disabled {
        writeln!(
            output,
            "env:      DISABLED via {BAROK_ENV_DISABLE}={env_value}"
        )?;
    } else {
        writeln!(output, "env:      enabled")?;
    }
    writeln!(output, "personas: {} eligible", BAROK_PERSONAS.len())?;
    writeln!(output)?;
    writeln!(output, "per-persona rule card:")?;
    writeln!(
        output,
        "  {:<24} rule-card bytes (terminal phase)",
        "persona"
    )?;
    writeln!(
        output,
        "  {:<24} --------------------------------",
        "-------"
    )?;
    for (persona, bytes) in BAROK_PERSONAS.iter().zip(rule_card_bytes) {
        writeln!(output, "  {persona:<24} {bytes}")?;
    }
    writeln!(output)?;
    writeln!(output, "notes:")?;
    writeln!(
        output,
        "  - bytes are zero when NANIKA_NO_BAROK=1 is set at invocation time."
    )?;
    writeln!(
        output,
        "  - injection only fires for terminal phases in the mission DAG."
    )?;
    writeln!(
        output,
        "  - non-terminal phases intentionally skip injection to preserve"
    )?;
    writeln!(
        output,
        "    prompt-prefix cache in downstream dependent workers."
    )
}

#[cfg(test)]
mod tests {
    use super::write_status;

    #[test]
    fn write_status_renders_enabled_go_output_without_process_environment()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut output = Vec::new();
        write_status(&mut output, false, "", &[11, 22, 33, 44, 55])?;

        assert_eq!(
            String::from_utf8(output)?,
            concat!(
                "barok output-compression status\n",
                "================================\n",
                "env:      enabled\n",
                "personas: 5 eligible\n",
                "\n",
                "per-persona rule card:\n",
                "  persona                  rule-card bytes (terminal phase)\n",
                "  -------                  --------------------------------\n",
                "  technical-writer         11\n",
                "  academic-researcher      22\n",
                "  architect                33\n",
                "  data-analyst             44\n",
                "  staff-code-reviewer      55\n",
                "\n",
                "notes:\n",
                "  - bytes are zero when NANIKA_NO_BAROK=1 is set at invocation time.\n",
                "  - injection only fires for terminal phases in the mission DAG.\n",
                "  - non-terminal phases intentionally skip injection to preserve\n",
                "    prompt-prefix cache in downstream dependent workers.\n",
            )
        );
        Ok(())
    }

    #[test]
    fn write_status_renders_disabled_go_output_without_process_environment()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut output = Vec::new();
        write_status(&mut output, true, "1", &[0; 5])?;

        assert_eq!(
            String::from_utf8(output)?,
            concat!(
                "barok output-compression status\n",
                "================================\n",
                "env:      DISABLED via NANIKA_NO_BAROK=1\n",
                "personas: 5 eligible\n",
                "\n",
                "per-persona rule card:\n",
                "  persona                  rule-card bytes (terminal phase)\n",
                "  -------                  --------------------------------\n",
                "  technical-writer         0\n",
                "  academic-researcher      0\n",
                "  architect                0\n",
                "  data-analyst             0\n",
                "  staff-code-reviewer      0\n",
                "\n",
                "notes:\n",
                "  - bytes are zero when NANIKA_NO_BAROK=1 is set at invocation time.\n",
                "  - injection only fires for terminal phases in the mission DAG.\n",
                "  - non-terminal phases intentionally skip injection to preserve\n",
                "    prompt-prefix cache in downstream dependent workers.\n",
            )
        );
        Ok(())
    }
}
