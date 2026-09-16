use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

const BAROK_ENABLED: &str = concat!(
    "barok output-compression status\n",
    "================================\n",
    "env:      enabled\n",
    "personas: 5 eligible\n",
    "\n",
    "per-persona rule card:\n",
    "  persona                  rule-card bytes (terminal phase)\n",
    "  -------                  --------------------------------\n",
    "  technical-writer         2742\n",
    "  academic-researcher      2857\n",
    "  architect                2868\n",
    "  data-analyst             2825\n",
    "  staff-code-reviewer      2898\n",
    "\n",
    "notes:\n",
    "  - bytes are zero when NANIKA_NO_BAROK=1 is set at invocation time.\n",
    "  - injection only fires for terminal phases in the mission DAG.\n",
    "  - non-terminal phases intentionally skip injection to preserve\n",
    "    prompt-prefix cache in downstream dependent workers.\n",
);

const BAROK_DISABLED: &str = concat!(
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
);

const DISCIPLINE_ENABLED: &str = concat!(
    "reasoning-discipline status\n",
    "===========================\n",
    "env:        enabled (default-on)\n",
    "card_bytes: 3014\n",
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
);

const DISCIPLINE_DISABLED: &str = concat!(
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
);

fn fixture_root() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "orchestrator-cli-system-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn snapshot_tree(
    root: &Path,
    directory: &Path,
) -> std::io::Result<Vec<(PathBuf, Option<Vec<u8>>)>> {
    let mut entries = Vec::new();
    let mut children = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let path = child.path();
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        if child.file_type()?.is_dir() {
            entries.push((relative, None));
            entries.extend(snapshot_tree(root, &path)?);
        } else {
            entries.push((relative, Some(fs::read(path)?)));
        }
    }
    Ok(entries)
}

const AUDIT_FIXTURE: &str = concat!(
    "{\"workspace_id\":\"ws-1\",\"domain\":\"dev\",\"audited_at\":\"2026-07-15T01:02:03Z\",",
    "\"scorecard\":{\"decomposition_quality\":1,\"persona_fit\":1,\"skill_utilization\":1,",
    "\"output_quality\":1,\"rule_compliance\":1,\"overall\":1}}\n",
    "{\"workspace_id\":\"ws-2\",\"domain\":\"work\",\"audited_at\":\"2026-07-16T01:02:03Z\",",
    "\"scorecard\":{\"decomposition_quality\":3,\"persona_fit\":3,\"skill_utilization\":3,",
    "\"output_quality\":3,\"rule_compliance\":3,\"overall\":3}}\n",
    "{\"workspace_id\":\"ws-3\",\"domain\":\"dev\",\"audited_at\":\"2026-07-17T01:02:03Z\",",
    "\"scorecard\":{\"decomposition_quality\":5,\"persona_fit\":5,\"skill_utilization\":5,",
    "\"output_quality\":5,\"rule_compliance\":5,\"overall\":5}}\n",
);

fn static_status_output(
    arguments: &[&str],
    disable_variable: Option<&str>,
) -> Result<std::process::Output, std::io::Error> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orchestrator"));
    command.args(arguments).env_clear();
    if let Some(variable) = disable_variable {
        command.env(variable, "1");
    }
    command.output()
}

fn assert_exact_static_status(
    arguments: &[&str],
    disable_variable: Option<&str>,
    expected: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let output = static_status_output(arguments, disable_variable)?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, expected.as_bytes());
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn binary_barok_status_is_exact_with_child_only_enabled_environment()
-> Result<(), Box<dyn std::error::Error>> {
    assert_exact_static_status(&["barok", "status"], None, BAROK_ENABLED)
}

#[test]
fn binary_barok_status_is_exact_with_child_only_disabled_environment()
-> Result<(), Box<dyn std::error::Error>> {
    assert_exact_static_status(
        &["barok", "status"],
        Some("NANIKA_NO_BAROK"),
        BAROK_DISABLED,
    )
}

#[test]
fn binary_discipline_status_is_exact_with_child_only_enabled_environment()
-> Result<(), Box<dyn std::error::Error>> {
    assert_exact_static_status(&["discipline", "status"], None, DISCIPLINE_ENABLED)
}

#[test]
fn binary_discipline_status_is_exact_with_child_only_disabled_environment()
-> Result<(), Box<dyn std::error::Error>> {
    assert_exact_static_status(
        &["discipline", "status"],
        Some("NANIKA_NO_DISCIPLINE"),
        DISCIPLINE_DISABLED,
    )
}

#[test]
fn binary_loads_fixture_home_personas_and_routing_without_writing_live_state()
-> Result<(), Box<dyn std::error::Error>> {
    let root = fixture_root();
    let home = root.join("home");
    let config = root.join("config");
    let personas = root.join("personas");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&config)?;
    fs::create_dir_all(&personas)?;
    fs::write(personas.join("architect.md"), "# Architect\n")?;
    fs::write(
        config.join("config.yaml"),
        "model_tiers:\n  work:\n    provider: openai\n    model: configured-model\n    runtime: codex\n",
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .args([
            "--domain",
            "dev",
            "run",
            "--dry-run",
            "PHASE: inspect | OBJECTIVE: inspect safely | PERSONA: architect",
        ])
        .env("HOME", &home)
        .env("ORCHESTRATOR_CONFIG_DIR", &config)
        .env("ORCHESTRATOR_PERSONAS_DIR", &personas)
        .env_remove("ALLUKA_HOME")
        .env_remove("VIA_HOME")
        .env("NANIKA_DEFAULT_RUNTIME", "codex")
        .output()?;

    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    fs::remove_dir_all(&root)?;

    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stdout.contains("persona=architect"));
    assert!(stdout.contains("runtime=codex"));
    assert!(stdout.contains("model=configured-model"));
    Ok(())
}

#[test]
fn binary_run_help_never_requires_a_runtime_home() -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .args(["--domain", "work", "run", "--help"])
        .env_remove("HOME")
        .env_remove("ORCHESTRATOR_CONFIG_DIR")
        .env_remove("ALLUKA_HOME")
        .env_remove("VIA_HOME")
        .output()?;

    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)?.contains("orchestrator run"));
    Ok(())
}

#[test]
fn home_backed_read_commands_use_explicit_config_without_requiring_home()
-> Result<(), Box<dyn std::error::Error>> {
    let root = fixture_root();
    let config = root.join("config");
    let alluka = root.join("alluka");
    let via = root.join("via");
    fs::create_dir_all(config.join("events"))?;
    fs::create_dir_all(&alluka)?;
    fs::create_dir_all(&via)?;
    fs::write(config.join("events/config-source.jsonl"), b"event\n")?;

    for arguments in [&["status"][..], &["events", "list"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
            .args(arguments)
            .env_clear()
            .env("ORCHESTRATOR_CONFIG_DIR", &config)
            .env("ALLUKA_HOME", &alluka)
            .env("VIA_HOME", &via)
            .output()?;
        let stderr = String::from_utf8(output.stderr)?;
        assert!(
            output.status.success(),
            "{arguments:?} failed without HOME: {stderr}"
        );
        assert!(!stderr.contains("HOME is unavailable"), "{stderr}");
        if arguments == ["events", "list"] {
            assert!(
                String::from_utf8(output.stdout)?.contains("config-source"),
                "ORCHESTRATOR_CONFIG_DIR did not win precedence"
            );
        }
    }

    fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn binary_audit_scorecard_uses_relative_config_shadowed_domain_and_go_int_forms_without_writes()
-> Result<(), Box<dyn std::error::Error>> {
    let root = fixture_root();
    let config = root.join("relative-config");
    fs::create_dir_all(&config)?;
    fs::write(config.join("audits.jsonl"), AUDIT_FIXTURE)?;
    for (name, bytes) in [
        ("metrics.db", b"database-sentinel\0\xff".as_slice()),
        ("metrics.db-wal", b"wal-sentinel\n".as_slice()),
        ("metrics.db-shm", b"shm-sentinel\0".as_slice()),
    ] {
        fs::write(config.join(name), bytes)?;
    }
    let before = snapshot_tree(&root, &root)?;

    let output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .args([
            "--domain",
            "work",
            "audit",
            "scorecard",
            "--domain",
            "dev",
            "--last=02",
            "--format=json",
            "ignored-positional",
        ])
        .current_dir(&root)
        .env_clear()
        .env("ORCHESTRATOR_CONFIG_DIR", "relative-config")
        .output()?;

    let stderr = String::from_utf8(output.stderr)?;
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let mut reports =
        orchestrator_app::load_reports(&fs::canonicalize(&config)?.join("audits.jsonl"))?;
    reports.retain(|report| report.domain == "dev");
    let mut expected =
        orchestrator_app::format_scorecard_json(&orchestrator_app::build_scorecard(&reports))?;
    expected.push('\n');
    assert_eq!(output.stdout, expected.as_bytes());
    assert_eq!(snapshot_tree(&root, &root)?, before);

    fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn binary_audit_scorecard_treats_missing_home_and_file_as_the_go_empty_store_without_writes()
-> Result<(), Box<dyn std::error::Error>> {
    const EMPTY: &[u8] = b"No audit reports found. Run `gyo evaluate` to generate one.\n";
    let root = fixture_root();
    fs::create_dir(&root)?;

    for config in ["missing-home", "existing-home"] {
        if config == "existing-home" {
            fs::create_dir(root.join(config))?;
            fs::write(root.join(config).join("sentinel"), b"unchanged\n")?;
        }
        let before = snapshot_tree(&root, &root)?;
        let output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
            .args(["audit", "scorecard"])
            .current_dir(&root)
            .env_clear()
            .env("ORCHESTRATOR_CONFIG_DIR", config)
            .output()?;
        let stderr = String::from_utf8(output.stderr)?;
        assert!(output.status.success(), "{config}: {stderr}");
        assert_eq!(output.stdout, EMPTY, "{config}");
        assert!(stderr.is_empty(), "{config}: {stderr}");
        assert_eq!(snapshot_tree(&root, &root)?, before, "{config}");
    }

    fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn binary_metrics_ignores_home_resolution_and_preserves_existing_sqlite_family()
-> Result<(), Box<dyn std::error::Error>> {
    let root = fixture_root();
    fs::create_dir(&root)?;
    let no_home_output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .args(["metrics", "trends"])
        .env_clear()
        .output()?;

    let no_home_stdout = String::from_utf8(no_home_output.stdout)?;
    let no_home_stderr = String::from_utf8(no_home_output.stderr)?;
    assert!(!no_home_output.status.success());
    assert!(
        no_home_stdout.is_empty(),
        "unexpected stdout: {no_home_stdout}"
    );
    assert!(
        no_home_stderr.contains("metrics-query-v1"),
        "{no_home_stderr}"
    );
    assert!(
        !no_home_stderr.contains("HOME is unavailable"),
        "metrics resolved HOME before consulting its query authority: {no_home_stderr}"
    );

    let home = root.join("runtime-home");
    fs::create_dir(&home)?;
    let sentinels = [
        ("metrics.db", b"database-sentinel\0\xff".as_slice()),
        ("metrics.db-wal", b"wal-sentinel\n".as_slice()),
        ("metrics.db-shm", b"shm-sentinel\0".as_slice()),
        ("operator-note", b"unrelated-entry\n".as_slice()),
    ];
    for (name, bytes) in sentinels {
        fs::write(home.join(name), bytes)?;
    }
    fs::create_dir(home.join("preserved-directory"))?;

    let entry_names = |directory: &std::path::Path| -> Result<Vec<String>, std::io::Error> {
        let mut names = fs::read_dir(directory)?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        names.sort_unstable();
        Ok(names)
    };
    let entries_before = entry_names(&home)?;
    let bytes_before = sentinels
        .iter()
        .map(|(name, _)| fs::read(home.join(name)).map(|bytes| ((*name).to_owned(), bytes)))
        .collect::<Result<Vec<_>, _>>()?;

    let output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .args(["metrics", "trends"])
        .env_clear()
        .env("ORCHESTRATOR_CONFIG_DIR", &home)
        .output()?;

    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!output.status.success());
    assert!(stdout.is_empty(), "unexpected stdout: {stdout}");
    assert!(stderr.contains("metrics-query-v1"), "{stderr}");
    assert!(stderr.contains("cooperative writer lock"), "{stderr}");
    assert_eq!(entry_names(&home)?, entries_before);
    let bytes_after = sentinels
        .iter()
        .map(|(name, _)| fs::read(home.join(name)).map(|bytes| ((*name).to_owned(), bytes)))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(bytes_after, bytes_before);

    fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn alluka_and_via_read_home_precedence_do_not_require_home()
-> Result<(), Box<dyn std::error::Error>> {
    let root = fixture_root();
    let alluka = root.join("alluka");
    let via = root.join("via");
    fs::create_dir_all(alluka.join("events"))?;
    fs::create_dir_all(via.join("orchestrator/events"))?;
    fs::write(alluka.join("events/alluka-source.jsonl"), b"event\n")?;
    fs::write(via.join("orchestrator/events/via-source.jsonl"), b"event\n")?;

    let alluka_output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .args(["events", "list"])
        .env_clear()
        .env("ALLUKA_HOME", &alluka)
        .env("VIA_HOME", &via)
        .output()?;
    let alluka_stderr = String::from_utf8(alluka_output.stderr)?;
    assert!(alluka_output.status.success(), "{alluka_stderr}");
    let alluka_stdout = String::from_utf8(alluka_output.stdout)?;
    assert!(alluka_stdout.contains("alluka-source"), "{alluka_stdout}");
    assert!(!alluka_stdout.contains("via-source"), "{alluka_stdout}");

    let via_output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .args(["events", "list"])
        .env_clear()
        .env("VIA_HOME", &via)
        .output()?;
    let via_stderr = String::from_utf8(via_output.stderr)?;
    assert!(via_output.status.success(), "{via_stderr}");
    let via_stdout = String::from_utf8(via_output.stdout)?;
    assert!(via_stdout.contains("via-source"), "{via_stdout}");

    fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn binary_refuses_non_dry_execution_without_enrollment_and_does_not_write_home()
-> Result<(), Box<dyn std::error::Error>> {
    let root = fixture_root();
    let home = root.join("home");
    let config = root.join("config");
    let personas = root.join("personas");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&config)?;
    fs::create_dir_all(&personas)?;
    fs::write(personas.join("generalist.md"), "# Generalist\n")?;
    let sentinel = home.join("sentinel");
    fs::write(&sentinel, b"unchanged\n")?;

    let output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .args(["run", "do not execute this task"])
        .env("HOME", &home)
        .env("ORCHESTRATOR_CONFIG_DIR", &config)
        .env("ORCHESTRATOR_PERSONAS_DIR", &personas)
        .env_remove("ALLUKA_HOME")
        .env_remove("VIA_HOME")
        .output()?;

    let stderr = String::from_utf8(output.stderr)?;
    assert!(!output.status.success());
    assert!(stderr.contains("Rust execution is not enrolled"));
    assert!(!stderr.contains("do not execute this task"));
    assert_eq!(fs::read(&sentinel)?, b"unchanged\n");
    fs::remove_dir_all(&root)?;
    Ok(())
}
