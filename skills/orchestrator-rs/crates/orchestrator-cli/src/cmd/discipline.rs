//! `orchestrator discipline status` output.

use std::io::{self, Write};

use orchestrator_core::{DISCIPLINE_ENV_DISABLE, discipline_disabled, discipline_rule_card_bytes};

pub(crate) fn run(output: &mut impl Write) -> io::Result<()> {
    let disabled = discipline_disabled();
    write_status(
        output,
        disabled,
        (!disabled).then(discipline_rule_card_bytes),
    )
}

fn write_status(
    output: &mut impl Write,
    disabled: bool,
    rule_card_bytes: Option<usize>,
) -> io::Result<()> {
    writeln!(output, "reasoning-discipline status")?;
    writeln!(output, "===========================")?;
    if disabled {
        writeln!(
            output,
            "env:        DISABLED via {DISCIPLINE_ENV_DISABLE}=1"
        )?;
    } else {
        writeln!(output, "env:        enabled (default-on)")?;
        if let Some(bytes) = rule_card_bytes {
            writeln!(output, "card_bytes: {bytes}")?;
        }
    }
    writeln!(output)?;
    writeln!(output, "gates:")?;
    writeln!(
        output,
        "  1. Scope — understand the problem boundary before touching code"
    )?;
    writeln!(
        output,
        "  2. Evidence — read the actual state before forming opinions"
    )?;
    writeln!(
        output,
        "  3. Adversarial — challenge your own first instinct"
    )?;
    writeln!(output, "  4. Verify — run it, don't assume it")?;
    writeln!(output, "  5. Calibrate — match effort to task weight")?;
    writeln!(output)?;
    writeln!(output, "notes:")?;
    writeln!(
        output,
        "  - default-on; set NANIKA_NO_DISCIPLINE=1 to disable"
    )?;
    writeln!(
        output,
        "  - applies to ALL phases (unlike barok which is terminal-only)"
    )?;
    writeln!(
        output,
        "  - injects after persona identity, before task objective"
    )
}

#[cfg(test)]
mod tests {
    use super::write_status;

    #[test]
    fn write_status_renders_enabled_go_output_without_process_environment()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut output = Vec::new();
        write_status(&mut output, false, Some(1234))?;

        assert_eq!(
            String::from_utf8(output)?,
            concat!(
                "reasoning-discipline status\n",
                "===========================\n",
                "env:        enabled (default-on)\n",
                "card_bytes: 1234\n",
                "\n",
                "gates:\n",
                "  1. Scope — understand the problem boundary before touching code\n",
                "  2. Evidence — read the actual state before forming opinions\n",
                "  3. Adversarial — challenge your own first instinct\n",
                "  4. Verify — run it, don't assume it\n",
                "  5. Calibrate — match effort to task weight\n",
                "\n",
                "notes:\n",
                "  - default-on; set NANIKA_NO_DISCIPLINE=1 to disable\n",
                "  - applies to ALL phases (unlike barok which is terminal-only)\n",
                "  - injects after persona identity, before task objective\n",
            )
        );
        Ok(())
    }

    #[test]
    fn write_status_renders_disabled_go_output_without_process_environment()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut output = Vec::new();
        write_status(&mut output, true, None)?;

        assert_eq!(
            String::from_utf8(output)?,
            concat!(
                "reasoning-discipline status\n",
                "===========================\n",
                "env:        DISABLED via NANIKA_NO_DISCIPLINE=1\n",
                "\n",
                "gates:\n",
                "  1. Scope — understand the problem boundary before touching code\n",
                "  2. Evidence — read the actual state before forming opinions\n",
                "  3. Adversarial — challenge your own first instinct\n",
                "  4. Verify — run it, don't assume it\n",
                "  5. Calibrate — match effort to task weight\n",
                "\n",
                "notes:\n",
                "  - default-on; set NANIKA_NO_DISCIPLINE=1 to disable\n",
                "  - applies to ALL phases (unlike barok which is terminal-only)\n",
                "  - injects after persona identity, before task objective\n",
            )
        );
        Ok(())
    }
}
