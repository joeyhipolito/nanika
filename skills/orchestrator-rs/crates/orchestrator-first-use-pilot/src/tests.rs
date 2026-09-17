use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const PILOT_INIT: &str = r#"{"type":"system","subtype":"init","tools":[],"mcp_servers":[],"slash_commands":[],"agents":[],"skills":[],"plugins":[],"claude_code_version":"2.1.269","permissionMode":"dontAsk","output_style":"default","capabilities":[],"fast_mode_state":"off"}"#;

/// A unique scratch directory under the system temp dir, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> TestResult<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = fs::canonicalize(std::env::temp_dir())?;
        let path = base.join(format!(
            "nanika-first-use-pilot-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn file(&self, name: &str, contents: &str) -> TestResult<PathBuf> {
        let path = self.0.join(name);
        fs::write(&path, contents)?;
        Ok(path)
    }

    /// A fake `claude` answering `--version` with `version` and otherwise
    /// running `body` as a POSIX shell script.
    fn fake_claude(&self, version: &str, body: &str) -> TestResult<PathBuf> {
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo '{version} (Claude Code)'; exit 0; fi\n{body}\n"
        );
        let path = self.file("fake-claude", &script)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        Ok(path)
    }

    fn options(&self, claude: &Path, timeout_secs: u64) -> TestResult<PilotOptions> {
        Ok(PilotOptions {
            command: PilotCommand::Review,
            prompt_file: self.file("prompt.md", "Review this excerpt: fn main() {}\n")?,
            output_dir: self.0.join("out"),
            repo: None,
            model: String::new(),
            persona: None,
            runtime: "claude".to_owned(),
            codex_executable: OsString::from("codex"),
            verification_argv: Vec::new(),
            verification_timeout: Duration::from_secs(timeout_secs),
            authored_mission: false,
            durable: false,
            stop_after_phase: None,
            mission_id: None,
            feature_requests: features::Requests::default(),
            progress_log: None,
            observe_follow: false,
            observe_format: observe::OutputFormat::Text,
            timeout: Duration::from_secs(timeout_secs),
            claude_executable: claude.as_os_str().to_owned(),
        })
    }

    fn git_repo(&self, name: &str) -> TestResult<PathBuf> {
        let repo = self.0.join(name);
        fs::create_dir(&repo)?;
        git(&repo, &["init", "-q"])?;
        git(&repo, &["config", "user.name", "Pilot Fixture"])?;
        git(&repo, &["config", "user.email", "pilot@example.invalid"])?;
        fs::write(
            repo.join("math_utils.py"),
            "def clamp(value, low, high):\n    return min(value, low)\n",
        )?;
        git(&repo, &["add", "math_utils.py"])?;
        git(&repo, &["commit", "-q", "-m", "fixture"])?;
        Ok(repo)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn emit(lines: &[&str]) -> String {
    let mut body = String::from("cat <<'EOF'\n");
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    body.push_str("EOF");
    body
}

fn git(repo: &Path, arguments: &[&str]) -> TestResult<String> {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {:?}: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

#[test]
fn changed_source_context_ignores_oversized_unchanged_files() -> TestResult {
    let scratch = Scratch::new()?;
    let baseline = scratch.0.join("baseline");
    let workspace = scratch.0.join("workspace");
    fs::create_dir(&baseline)?;
    fs::create_dir(&workspace)?;
    let unchanged = vec![b'x'; 1024];
    fs::write(baseline.join("unchanged.bin"), &unchanged)?;
    fs::write(workspace.join("unchanged.bin"), &unchanged)?;
    fs::write(baseline.join("changed.txt"), "before\n")?;
    fs::write(workspace.join("changed.txt"), "after\n")?;
    let snapshot =
        snapshot::RepositorySnapshot::from_paths_for_test(scratch.0.clone(), baseline, workspace)?;

    let context = String::from_utf8(snapshot.changed_source_context(256)?)?;

    assert!(context.contains(
        "--- complete final contents of modified file \"changed.txt\" (6 bytes) ---\nafter\n"
    ));
    assert!(!context.contains("before\n"));
    assert!(!context.contains("unchanged.bin"));
    Ok(())
}

#[test]
fn changed_source_context_distinguishes_added_deleted_empty_and_missing_newline_files() -> TestResult
{
    let scratch = Scratch::new()?;
    let baseline = scratch.0.join("baseline");
    let workspace = scratch.0.join("workspace");
    fs::create_dir(&baseline)?;
    fs::create_dir(&workspace)?;
    fs::write(baseline.join("deleted-empty.txt"), "")?;
    fs::write(baseline.join("deleted.txt"), "original without newline")?;
    fs::write(workspace.join("added-empty.txt"), "")?;
    fs::write(workspace.join("added.txt"), "final without newline")?;
    let snapshot =
        snapshot::RepositorySnapshot::from_paths_for_test(scratch.0.clone(), baseline, workspace)?;

    let context = String::from_utf8(snapshot.changed_source_context(2048)?)?;

    assert!(
        context.contains("complete final contents of added file \"added-empty.txt\" (0 bytes)")
    );
    assert!(context.contains(
        "complete final contents of added file \"added.txt\" (21 bytes) ---\nfinal without newline\n--- end complete file contents; no final newline ---"
    ));
    assert!(
        context
            .contains("complete original contents of deleted file \"deleted-empty.txt\" (0 bytes)")
    );
    assert!(context.contains(
        "complete original contents of deleted file \"deleted.txt\" (24 bytes) ---\noriginal without newline\n--- end complete file contents; no final newline ---"
    ));
    Ok(())
}

#[test]
fn changed_source_context_rejects_non_utf8_contents() -> TestResult {
    let scratch = Scratch::new()?;
    let baseline = scratch.0.join("baseline");
    let workspace = scratch.0.join("workspace");
    fs::create_dir(&baseline)?;
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("binary.dat"), [0xff])?;
    let snapshot =
        snapshot::RepositorySnapshot::from_paths_for_test(scratch.0.clone(), baseline, workspace)?;

    let error = snapshot
        .changed_source_context(2048)
        .err()
        .ok_or("non-UTF-8 contents were accepted")?;

    assert!(error.contains("not UTF-8"));
    Ok(())
}

#[test]
fn changed_source_context_rejects_non_utf8_original_replaced_with_text() -> TestResult {
    let scratch = Scratch::new()?;
    let baseline = scratch.0.join("baseline");
    let workspace = scratch.0.join("workspace");
    fs::create_dir(&baseline)?;
    fs::create_dir(&workspace)?;
    fs::write(baseline.join("converted.txt"), [0, 0xff])?;
    fs::write(workspace.join("converted.txt"), "valid final text\n")?;
    let snapshot =
        snapshot::RepositorySnapshot::from_paths_for_test(scratch.0.clone(), baseline, workspace)?;
    let error = snapshot
        .changed_source_context(2048)
        .err()
        .ok_or("non-UTF-8 original was accepted")?;
    assert!(error.contains("original changed source file"));
    assert!(error.contains("not UTF-8"));
    Ok(())
}

#[test]
fn changed_source_context_escapes_path_delimiters() -> TestResult {
    let scratch = Scratch::new()?;
    let baseline = scratch.0.join("baseline");
    let workspace = scratch.0.join("workspace");
    fs::create_dir(&baseline)?;
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("line\nbreak.txt"), "text\n")?;
    let snapshot =
        snapshot::RepositorySnapshot::from_paths_for_test(scratch.0.clone(), baseline, workspace)?;

    let context = String::from_utf8(snapshot.changed_source_context(2048)?)?;

    assert!(context.contains("\"line\\nbreak.txt\""));
    Ok(())
}

#[test]
fn changed_source_context_includes_final_contents_for_mode_only_changes() -> TestResult {
    let scratch = Scratch::new()?;
    let baseline = scratch.0.join("baseline");
    let workspace = scratch.0.join("workspace");
    fs::create_dir(&baseline)?;
    fs::create_dir(&workspace)?;
    fs::write(baseline.join("script.sh"), "#!/bin/sh\n")?;
    fs::write(workspace.join("script.sh"), "#!/bin/sh\n")?;
    fs::set_permissions(
        workspace.join("script.sh"),
        fs::Permissions::from_mode(0o755),
    )?;
    let snapshot =
        snapshot::RepositorySnapshot::from_paths_for_test(scratch.0.clone(), baseline, workspace)?;

    let context = String::from_utf8(snapshot.changed_source_context(2048)?)?;

    assert!(context.contains(
        "complete final contents of modified file \"script.sh\" (10 bytes) ---\n#!/bin/sh\n"
    ));
    Ok(())
}

#[test]
fn review_prompt_keeps_full_diff_and_one_complete_final_copy_for_large_modification() -> TestResult
{
    let scratch = Scratch::new()?;
    let baseline = scratch.0.join("baseline");
    let workspace = scratch.0.join("workspace");
    fs::create_dir(&baseline)?;
    fs::create_dir(&workspace)?;
    let before = (0..6000)
        .map(|line| format!("untouched-{line:04}\n"))
        .collect::<String>();
    let after = before.replace("untouched-3000\n", "added replacement line\n");
    fs::write(baseline.join("large.txt"), &before)?;
    fs::write(workspace.join("large.txt"), &after)?;
    let snapshot =
        snapshot::RepositorySnapshot::from_paths_for_test(scratch.0.clone(), baseline, workspace)?;
    let diff = concat!(
        "diff --git a/large.txt b/large.txt\n",
        "--- a/large.txt\n",
        "+++ b/large.txt\n",
        "@@ -2999,5 +2999,5 @@\n",
        " untouched-2998\n",
        " untouched-2999\n",
        "-untouched-3000\n",
        "+added replacement line\n",
        " untouched-3001\n",
        " untouched-3002\n",
    );

    let prompt = cycle::build_review_prompt(&snapshot, "bounded edit", diff.as_bytes())?;
    let old_before_and_after_bytes = prompt.len() + before.len();

    assert!(prompt.contains(diff));
    assert!(prompt.contains(&after));
    assert_eq!(prompt.matches(&after).count(), 1);
    assert_eq!(prompt.matches("untouched-3000\n").count(), 1);
    assert_eq!(prompt.matches("added replacement line\n").count(), 2);
    assert!(prompt.contains("untouched-0000\n"));
    assert!(prompt.contains("untouched-5999\n"));
    assert!(prompt.len() * 4 < old_before_and_after_bytes * 3);
    Ok(())
}

#[test]
fn review_prompt_refuses_oversized_complete_final_contents() -> TestResult {
    let scratch = Scratch::new()?;
    let baseline = scratch.0.join("baseline");
    let workspace = scratch.0.join("workspace");
    fs::create_dir(&baseline)?;
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("oversized.txt"), vec![b'x'; 512 * 1024])?;
    let snapshot =
        snapshot::RepositorySnapshot::from_paths_for_test(scratch.0.clone(), baseline, workspace)?;

    let error = cycle::build_review_prompt(&snapshot, "add file", b"diff\n")
        .err()
        .ok_or("oversized context was accepted")?;

    assert!(error.contains("review bound"));
    Ok(())
}

#[test]
fn snapshot_diff_rejects_missing_and_replaced_roots() -> TestResult {
    for root_name in ["baseline", "workspace"] {
        for mode in ["missing", "replaced"] {
            let scratch = Scratch::new()?;
            let baseline = scratch.0.join("baseline");
            let workspace = scratch.0.join("workspace");
            fs::create_dir(&baseline)?;
            fs::create_dir(&workspace)?;
            let snapshot = snapshot::RepositorySnapshot::from_paths_for_test(
                scratch.0.clone(),
                baseline.clone(),
                workspace.clone(),
            )?;
            let target = if root_name == "baseline" {
                baseline
            } else {
                workspace
            };
            fs::remove_dir(&target)?;
            if mode == "replaced" {
                fs::create_dir(&target)?;
            }
            let supervisor = ProcessSupervisor::process_wide()?;
            let error = match snapshot.diff(&supervisor) {
                Ok(_) => return Err("root mutation unexpectedly produced a diff".into()),
                Err(error) => error,
            };
            assert!(error.contains(root_name), "{root_name}/{mode}: {error}");
            assert!(
                error.contains(if mode == "missing" {
                    "unavailable"
                } else {
                    "replaced"
                }),
                "{root_name}/{mode}: {error}"
            );
        }
    }
    Ok(())
}

fn result_json(options: &PilotOptions) -> TestResult<Value> {
    let bytes = fs::read(options.output_dir.join("pilot-result.json"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[test]
fn version_parser_accepts_only_exact_semver_token() {
    assert_eq!(
        parse_claude_version(b"2.1.269 (Claude Code)\n").as_deref(),
        Some("2.1.269")
    );
    assert_eq!(parse_claude_version(b"2.1 (Claude Code)"), None);
    assert_eq!(parse_claude_version(b"2.1.269.1"), None);
    assert_eq!(parse_claude_version(b"v2.1.269"), None);
    assert_eq!(parse_claude_version(b""), None);
}

#[test]
fn arguments_require_explicit_command_inputs_and_bounded_timeout() -> TestResult {
    assert!(matches!(
        parse_arguments(Vec::<String>::new()),
        Err(PilotError::Usage(_))
    ));
    assert!(matches!(
        parse_arguments(["run"]),
        Err(PilotError::Usage(_))
    ));
    assert!(matches!(
        parse_arguments(["review", "--output-dir", "o"]),
        Err(PilotError::Usage(_))
    ));
    assert!(matches!(
        parse_arguments([
            "review",
            "--prompt-file",
            "p",
            "--output-dir",
            "o",
            "--tools",
            "x"
        ]),
        Err(PilotError::Usage(_))
    ));
    for bad in ["0", "1801", "ten"] {
        assert!(matches!(
            parse_arguments([
                "review",
                "--prompt-file",
                "p",
                "--output-dir",
                "o",
                "--timeout-secs",
                bad
            ]),
            Err(PilotError::Usage(_))
        ));
    }
    let options = parse_arguments(["review", "--prompt-file", "p", "--output-dir", "o"])?;
    assert_eq!(options.timeout, DEFAULT_TIMEOUT);
    assert_eq!(options.claude_executable, OsString::from("claude"));
    Ok(())
}

#[test]
fn resume_rejects_all_explicit_execution_options() {
    for explicit in [
        vec!["--codex", "codex"],
        vec!["--timeout-secs", "600"],
        vec!["--verification-timeout-secs", "600"],
        vec!["--model", ""],
    ] {
        assert!(matches!(
            parse_arguments(
                ["resume", "--output-dir", "out"]
                    .into_iter()
                    .chain(explicit)
            ),
            Err(PilotError::Usage(_))
        ));
    }
}

#[test]
fn missing_opt_in_refuses_before_touching_the_output_directory() -> TestResult {
    let scratch = Scratch::new()?;
    let prompt = scratch.file("prompt.md", "review")?;
    let out = scratch.0.join("out");
    let (mut output, mut errors) = (Vec::new(), Vec::new());
    let code = main_with(
        [
            "review".to_owned(),
            "--prompt-file".to_owned(),
            prompt.display().to_string(),
            "--output-dir".to_owned(),
            out.display().to_string(),
        ],
        Some("true".to_owned()),
        &mut output,
        &mut errors,
    );
    assert_eq!(code, EXIT_USAGE);
    assert!(String::from_utf8(errors)?.contains(OPT_IN_ENV));
    assert!(!out.exists());
    Ok(())
}

#[test]
fn prompt_must_be_non_empty_and_fit_the_argument_budget() -> TestResult {
    let scratch = Scratch::new()?;
    let empty = scratch.file("empty.md", "  \n")?;
    assert!(matches!(
        read_prompt(&empty),
        Err(PilotError::Prompt { .. })
    ));
    let oversized = "x".repeat(usize::try_from(MAX_PROMPT_BYTES)? + 1);
    let large = scratch.file("large.md", &oversized)?;
    assert!(matches!(
        read_prompt(&large),
        Err(PilotError::Prompt { .. })
    ));
    Ok(())
}

#[test]
fn output_directory_must_be_fresh_and_outside_git_checkouts() -> TestResult {
    let scratch = Scratch::new()?;
    let existing = scratch.0.join("existing");
    fs::create_dir(&existing)?;
    assert!(matches!(
        create_output_layout(&existing, PilotCommand::Review),
        Err(PilotError::OutputDirectory { .. })
    ));

    let checkout = scratch.0.join("checkout");
    fs::create_dir_all(checkout.join(".git"))?;
    fs::create_dir(checkout.join("nested"))?;
    let inside = checkout.join("nested").join("out");
    assert!(matches!(
        create_output_layout(&inside, PilotCommand::Review),
        Err(PilotError::OutputDirectory { .. })
    ));
    assert!(!inside.exists());

    let layout = create_output_layout(&scratch.0.join("fresh"), PilotCommand::Review)?;
    assert_eq!(
        fs::metadata(&layout.root)?.permissions().mode() & 0o777,
        0o700
    );
    assert!(fs::read_dir(&layout.worker)?.next().is_none());
    Ok(())
}

#[test]
fn successful_stream_saves_review_and_isolated_argv() -> TestResult {
    let scratch = Scratch::new()?;
    let claude = scratch.fake_claude(
        "2.1.269",
        &emit(&[
            PILOT_INIT,
            r#"{"type":"assistant","content":[{"type":"text","text":"LGTM: no findings"}]}"#,
            r#"{"type":"result","subtype":"success"}"#,
        ]),
    )?;
    let options = scratch.options(&claude, 30)?;
    let summary = run_review(&options, CancellationToken::new())?;
    assert!(summary.completed, "{}", summary.reason);

    let review = fs::read_to_string(options.output_dir.join("review.md"))?;
    assert!(review.starts_with("LGTM: no findings"));
    let record = result_json(&options)?;
    assert_eq!(record["status"], "completed");
    assert_eq!(record["claude_code_version_observed"], "2.1.269");
    let argv: Vec<String> =
        serde_json::from_value(record["process"]["argv_after_executable"].clone())?;
    for required in [
        "--safe-mode",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--no-session-persistence",
    ] {
        assert!(argv.iter().any(|a| a == required), "missing {required}");
    }
    let tools = argv
        .iter()
        .position(|a| a == "--tools")
        .ok_or("no --tools")?;
    assert_eq!(argv.get(tools + 1).map(String::as_str), Some(""));
    let prompt = argv.iter().position(|a| a == "-p").ok_or("no -p")?;
    assert_eq!(
        argv.get(prompt + 1).map(String::as_str),
        Some("<prompt.md>")
    );
    assert_eq!(record["process"]["cleanup_complete"], true);
    assert!(options.output_dir.join("claude-stdout.jsonl").is_file());
    Ok(())
}

#[test]
fn provider_reported_error_with_exit_zero_fails_nonzero() -> TestResult {
    let scratch = Scratch::new()?;
    let claude = scratch.fake_claude(
        "2.1.269",
        &emit(&[
            PILOT_INIT,
            r#"{"type":"assistant","content":[{"type":"text","text":"must not commit"}]}"#,
            r#"{"type":"result","subtype":"success","is_error":true}"#,
        ]),
    )?;
    let options = scratch.options(&claude, 30)?;
    let (mut output, mut errors) = (Vec::new(), Vec::new());
    let code = main_with(
        [
            "review".to_owned(),
            "--prompt-file".to_owned(),
            options.prompt_file.display().to_string(),
            "--output-dir".to_owned(),
            options.output_dir.display().to_string(),
            "--claude".to_owned(),
            claude.display().to_string(),
        ],
        Some("1".to_owned()),
        &mut output,
        &mut errors,
    );
    assert_eq!(code, EXIT_FAILED);
    assert!(!options.output_dir.join("review.md").exists());
    let record = result_json(&options)?;
    assert_eq!(record["status"], "failed");
    assert!(
        record["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("unsuccessful result")),
        "{record}"
    );
    Ok(())
}

#[test]
fn exit_zero_without_result_and_tool_use_both_fail() -> TestResult {
    for (lines, needle) in [
        (
            vec![
                PILOT_INIT,
                r#"{"type":"assistant","content":[{"type":"text","text":"partial"}]}"#,
            ],
            "without a successful result",
        ),
        (
            vec![
                PILOT_INIT,
                r#"{"type":"assistant","content":[{"type":"tool_use","id":"t","name":"Bash","input":{}}]}"#,
            ],
            "tool message",
        ),
    ] {
        let scratch = Scratch::new()?;
        let claude = scratch.fake_claude("2.1.269", &emit(&lines))?;
        let options = scratch.options(&claude, 30)?;
        let summary = run_review(&options, CancellationToken::new())?;
        assert!(!summary.completed);
        assert!(summary.reason.contains(needle), "{}", summary.reason);
    }
    Ok(())
}

#[test]
fn version_mismatch_refuses_without_dispatching_the_review() -> TestResult {
    let scratch = Scratch::new()?;
    let claude = scratch.fake_claude("2.1.211", &emit(&[PILOT_INIT]))?;
    let options = scratch.options(&claude, 30)?;
    let summary = run_review(&options, CancellationToken::new())?;
    assert!(!summary.completed);
    assert_eq!(summary.status, "refused_version");
    let record = result_json(&options)?;
    assert_eq!(record["review_dispatched"], false);
    assert!(!options.output_dir.join("claude-stdout.jsonl").exists());
    Ok(())
}

#[test]
fn hard_deadline_terminates_and_reaps_the_child() -> TestResult {
    let scratch = Scratch::new()?;
    let claude = scratch.fake_claude("2.1.269", "echo started; exec sleep 60")?;
    let options = scratch.options(&claude, 1)?;
    let started = Instant::now();
    let summary = run_review(&options, CancellationToken::new())?;
    assert!(!summary.completed);
    assert!(started.elapsed() < Duration::from_secs(20));
    let record = result_json(&options)?;
    assert_eq!(record["process"]["deadline_observed"], true);
    assert_eq!(record["process"]["cleanup_complete"], true);
    Ok(())
}

#[test]
fn cancellation_terminates_and_reaps_the_child() -> TestResult {
    let scratch = Scratch::new()?;
    let claude = scratch.fake_claude("2.1.269", "echo started; exec sleep 60")?;
    let options = scratch.options(&claude, 60)?;
    let token = CancellationToken::new();
    let canceller = {
        let token = token.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            token.cancel();
        })
    };
    let summary = run_review(&options, token)?;
    canceller.join().map_err(|_| "canceller panicked")?;
    assert!(!summary.completed);
    let record = result_json(&options)?;
    assert_eq!(record["process"]["cancellation_observed"], true, "{record}");
    assert_eq!(record["process"]["cleanup_complete"], true);
    Ok(())
}

const RECORDED_2_1_269_WIRE: &str = include_str!(
    "../../orchestrator-provider-claude/tests/fixtures/claude-2.1.269-first-use-pilot-success.jsonl"
);

#[test]
fn recorded_2_1_269_wire_completes_the_pilot_review() -> TestResult {
    let scratch = Scratch::new()?;
    let lines: Vec<&str> = RECORDED_2_1_269_WIRE.lines().collect();
    let claude = scratch.fake_claude("2.1.269", &emit(&lines))?;
    let options = scratch.options(&claude, 30)?;
    let summary = run_review(&options, CancellationToken::new())?;
    assert!(summary.completed, "{}", summary.reason);

    let review = fs::read_to_string(options.output_dir.join("review.md"))?;
    assert!(
        review.starts_with("Sanitized review: no findings."),
        "{review}"
    );
    let record = result_json(&options)?;
    assert_eq!(record["status"], "completed");
    assert_eq!(record["claude_code_version_observed"], "2.1.269");
    assert!(!record["cost"].is_null(), "{record}");
    assert_eq!(record["worker_usage"]["status"], "available");
    assert_eq!(record["worker_usage"]["portal_effective"], "off");
    let telemetry: Value =
        serde_json::from_slice(&fs::read(options.output_dir.join("worker-usage.json"))?)?;
    assert_eq!(telemetry["report"]["assistant_message_count"], 1);
    let lines = fs::read_to_string(options.output_dir.join("worker-usage-events.jsonl"))?;
    let events: Vec<Value> = lines
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["kind"], "worker.usage");
    assert_eq!(events[0]["delivery"], "attempt-final-snapshot");
    assert!(!telemetry.to_string().contains("Sanitized review"));

    Ok(())
}

#[test]
fn recorded_2_1_269_wire_with_hook_activity_fails_without_review() -> TestResult {
    let hook = r#"{"type":"system","subtype":"hook_started","hook_id":"h","hook_name":"SessionStart:startup","hook_event":"SessionStart"}"#;
    let mut lines: Vec<&str> = RECORDED_2_1_269_WIRE.lines().collect();
    lines.insert(1, hook);
    let scratch = Scratch::new()?;
    let claude = scratch.fake_claude("2.1.269", &emit(&lines))?;
    let options = scratch.options(&claude, 30)?;
    let summary = run_review(&options, CancellationToken::new())?;
    assert!(!summary.completed);
    assert!(!options.output_dir.join("review.md").exists());
    assert_eq!(result_json(&options)?["status"], "failed");
    Ok(())
}

const CODEX_WIRE: &str = include_str!("codex-success.jsonl");

fn codex_options(scratch: &Scratch, body: &str, seconds: u64) -> TestResult<PilotOptions> {
    let executable = scratch.file("fake-codex", &format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.154.0'; exit 0; fi\n{body}\n"
    ))?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    let mut options = scratch.options(&executable, seconds)?;
    options.runtime = "codex".into();
    options.codex_executable = executable.into_os_string();
    Ok(options)
}

#[test]
fn codex_recorded_wire_and_explicit_model_are_saved_truthfully() -> TestResult {
    let scratch = Scratch::new()?;
    let mut options = codex_options(&scratch, &emit(&CODEX_WIRE.lines().collect::<Vec<_>>()), 30)?;
    options.model = "explicit-model".into();
    let summary = run_review(&options, CancellationToken::new())?;
    assert!(summary.completed, "{}", summary.reason);
    let record = result_json(&options)?;
    assert_eq!(record["runtime"], "codex");
    assert_eq!(record["runtime_version_observed"], "0.154.0");
    assert_eq!(record["model"], "explicit-model");
    assert!(record["cost"].is_null());
    assert!(record["claude_code_version_observed"].is_null());
    assert_eq!(record["codex_protocol"]["usage"]["input_tokens"], 10430);
    assert_eq!(
        record["codex_protocol"]["warnings"]
            .as_array()
            .ok_or("warnings")?
            .len(),
        2
    );
    assert_eq!(record["process"]["cleanup_complete"], true);
    assert!(options.output_dir.join("codex-stdout.jsonl").is_file());
    assert!(options.output_dir.join("codex-stderr.txt").is_file());
    assert!(options.output_dir.join("review.md").is_file());
    let argv: Vec<String> =
        serde_json::from_value(record["process"]["argv_after_executable"].clone())?;
    assert_eq!(argv.first().map(String::as_str), Some("exec"));
    assert_eq!(argv.last().map(String::as_str), Some("-"));
    for required in [
        "--ignore-user-config",
        "--ignore-rules",
        "--ephemeral",
        "read-only",
        "approval_policy=\"never\"",
        "web_search=\"disabled\"",
        "project_doc_max_bytes=0",
        "suppress_unstable_features_warning=true",
        "explicit-model",
    ] {
        assert!(argv.iter().any(|arg| arg == required), "{required}");
    }
    for feature in [
        "shell_tool",
        "unified_exec",
        "hooks",
        "plugins",
        "apps",
        "multi_agent",
        "multi_agent_v2",
        "browser_use",
        "code_mode_host",
        "skill_search",
        "skill_mcp_dependency_install",
    ] {
        assert!(
            argv.windows(2).any(|pair| pair == ["--disable", feature]),
            "{feature}"
        );
    }
    assert!(!argv.iter().any(|arg| arg.contains("dangerously")));
    Ok(())
}

#[test]
fn codex_arguments_preserve_claude_default_and_select_codex_model() -> TestResult {
    let base = ["review", "--prompt-file", "p", "--output-dir", "o"];
    let claude = parse_arguments(base)?;
    assert_eq!(claude.runtime, "claude");
    assert_eq!(claude.model, "");
    let codex = parse_arguments(base.into_iter().chain([
        "--runtime",
        "codex",
        "--codex",
        "/custom/codex",
    ]))?;
    assert_eq!(codex.model, "");
    assert_eq!(codex.executable(), OsString::from("/custom/codex"));
    assert!(parse_arguments(base.into_iter().chain(["--runtime", "other"])).is_err());
    Ok(())
}

#[test]
fn codex_wire_rejects_ambiguous_errors_and_all_activity() -> TestResult {
    let lines: Vec<&str> = CODEX_WIRE.lines().collect();
    let good = [lines[0], lines[3], lines[4], lines[5]].join("\n");
    assert!(codex::parse(good.as_bytes()).is_ok());
    let mut bad = vec![
        lines[..5].join("\n"),
        format!("{good}\n{}", lines[5]),
        format!("{good}\n{}", lines[4]),
        good.replace("turn.completed", "turn.failed"),
        good.replace(
            "\"input_tokens\":10430",
            "\"input_tokens\":10430,\"input_tokens\":1",
        ),
        good.replace(
            "\"type\":\"agent_message\"",
            "\"type\":\"agent_message\",\"type\":\"agent_message\"",
        ),
        good.replace(
            "\"type\":\"turn.started\"",
            "\"type\":\"turn.started\",\"type\":\"turn.started\"",
        ),
        good.replace(
            "\"type\":\"turn.started\"",
            "\"type\":\"turn.started\",\"forbidden\":true",
        ),
        good.replace(
            "\"type\":\"turn.started\"",
            "\"type\":\"turn.started\",\"x\":1,\"x\":2",
        ),
        good.replace("\"id\":\"item_2\"", "\"id\":\"item_2\",\"id\":\"item_2\""),
        good.replace("\"usage\":", "\"unknown\":0,\"usage\":"),
        [lines[0], lines[3], lines[1], lines[4], lines[5]].join("\n"),
        [lines[0], lines[1], lines[1], lines[3], lines[4], lines[5]].join("\n"),
        [lines[3], lines[0], lines[4], lines[5]].join("\n"),
        [lines[0], lines[4], lines[3], lines[5]].join("\n"),
        [lines[0], lines[3], lines[5]].join("\n"),
        CODEX_WIRE.replace(
            "Under-development features enabled",
            "Unknown startup issue",
        ),
        format!("{good}\n{{broken"),
    ];
    let mut answer: Value = serde_json::from_str(lines[4])?;
    answer["item"]["text"] = json!("  ");
    bad.push([lines[0], lines[3], &answer.to_string(), lines[5]].join("\n"));
    for activity in [
        "command_execution",
        "file_change",
        "mcp_tool_call",
        "web_search",
        "collab_tool_call",
        "reasoning",
        "error",
    ] {
        bad.push(good.replace("agent_message", activity));
    }
    for (index, wire) in bad.iter().enumerate() {
        assert!(
            codex::parse(wire.as_bytes()).is_err(),
            "accepted mutation {index}"
        );
    }
    Ok(())
}

#[test]
fn codex_failure_timeout_and_cancel_never_save_review() -> TestResult {
    for mode in ["wire", "exit", "timeout", "cancel"] {
        let scratch = Scratch::new()?;
        let body = match mode {
            "wire" => emit(&["{\"type\":\"turn.failed\"}"]),
            "exit" => format!("{}\nexit 7", emit(&CODEX_WIRE.lines().collect::<Vec<_>>())),
            _ => "echo started; exec sleep 60".to_owned(),
        };
        let options = codex_options(&scratch, &body, if mode == "timeout" { 1 } else { 30 })?;
        let token = CancellationToken::new();
        let canceller = if mode == "cancel" {
            let token = token.clone();
            Some(thread::spawn(move || {
                thread::sleep(Duration::from_millis(500));
                token.cancel();
            }))
        } else {
            None
        };
        let summary = run_review(&options, token)?;
        if let Some(handle) = canceller {
            handle.join().map_err(|_| "canceller")?;
        }
        assert!(!summary.completed, "{mode}");
        assert!(!options.output_dir.join("review.md").exists());
        let record = result_json(&options)?;
        assert_eq!(record["process"]["cleanup_complete"], true, "{mode}");
        assert_eq!(record["route"]["model"], "gpt-5.6-sol");
        assert_eq!(record["route"]["effort"], "medium");
        assert_eq!(record["route"]["persona"], "general-purpose");
        assert_eq!(record["route"]["tier"], "work");
        assert!(record["route"]["selection_reason"].is_string());
        if mode == "timeout" {
            assert_eq!(record["process"]["deadline_observed"], true);
        }
        if mode == "cancel" {
            assert_eq!(record["process"]["cancellation_observed"], true);
        }
    }
    Ok(())
}

#[test]
fn codex_wrong_or_ambiguous_version_refuses_before_review() -> TestResult {
    for version in ["codex-cli 0.153.0", "codex-cli 0.154.0 extra"] {
        let scratch = Scratch::new()?;
        let options = codex_options(&scratch, "exit 99", 30)?;
        fs::write(
            &options.codex_executable,
            format!("#!/bin/sh\necho '{version}'\n"),
        )?;
        let summary = run_review(&options, CancellationToken::new())?;
        assert!(!summary.completed);
        let record = result_json(&options)?;
        assert_eq!(record["runtime"], "codex");
        assert_eq!(record["runtime_version_required"], "0.154.0");
        assert_eq!(record["route"]["model"], "gpt-5.6-sol");
        assert_eq!(record["route"]["effort"], "medium");
        assert_eq!(record["review_dispatched"], false);
        assert!(!options.output_dir.join("codex-stdout.jsonl").exists());
    }
    Ok(())
}

#[test]
fn automatic_routes_reach_fake_codex_argv_and_saved_records() -> TestResult {
    for (task, persona, override_model, tier, model, effort) in [
        ("fix typo", None, "", "quick", "gpt-5.6-luna", "low"),
        (
            "summarize this text",
            None,
            "",
            "general",
            "gpt-5.6-terra",
            "medium",
        ),
        ("design an API", None, "", "think", "gpt-5.6-sol", "high"),
        (
            "fix typo",
            Some("reviewer"),
            "",
            "work",
            "gpt-5.6-sol",
            "medium",
        ),
        (
            "fix typo",
            Some("security-auditor"),
            "",
            "think",
            "gpt-5.6-sol",
            "high",
        ),
        (
            "fix typo",
            None,
            "custom-model",
            "quick",
            "custom-model",
            "low",
        ),
        (
            "design an API",
            Some("reviewer"),
            "custom-model",
            "think",
            "custom-model",
            "high",
        ),
    ] {
        let scratch = Scratch::new()?;
        let body = format!(
            "printf '%s\\n' \"$@\" > actual-argv.txt\n{}",
            emit(&CODEX_WIRE.lines().collect::<Vec<_>>())
        );
        let mut options = codex_options(&scratch, &body, 30)?;
        fs::write(&options.prompt_file, task)?;
        options.persona = persona.map(str::to_owned);
        options.model = override_model.to_owned();
        assert!(run_review(&options, CancellationToken::new())?.completed);
        let record = result_json(&options)?;
        let route = &record["route"];
        assert_eq!(route["tier"], tier);
        assert_eq!(route["persona"], persona.unwrap_or("general-purpose"));
        assert_eq!(route["model"], model);
        assert_eq!(route["effort"], effort);
        assert_eq!(record["model"], model);
        assert!(
            route["selection_reason"]
                .as_str()
                .ok_or("reason")?
                .contains(if override_model.is_empty() {
                    "deterministic"
                } else {
                    "override"
                })
        );
        let actual = fs::read_to_string(options.output_dir.join("worker/actual-argv.txt"))?;
        let argv: Vec<&str> = actual.lines().collect();
        assert!(argv.windows(2).any(|p| p == ["-m", model]));
        let expected = format!("model_reasoning_effort=\"{effort}\"");
        assert!(argv.windows(2).any(|p| p == ["-c", expected.as_str()]));
        assert_eq!(
            argv.iter()
                .filter(|a| a.starts_with("model_reasoning_effort="))
                .count(),
            1
        );
    }
    Ok(())
}

#[test]
fn persona_option_and_claude_compatibility() -> TestResult {
    let base = ["review", "--prompt-file", "p", "--output-dir", "o"];
    let options = parse_arguments(base)?;
    for task in ["fix typo", "summarize text", "design an API"] {
        let route = routing::select(&options, task);
        assert_eq!(route.persona, "reviewer");
        assert_eq!(route.model, "");
        assert_eq!(route.effort, orchestrator_exec::Effort::High);
    }
    let options = parse_arguments(base.into_iter().chain([
        "--persona",
        "security-auditor",
        "--model",
        "opus",
    ]))?;
    let route = routing::select(&options, "fix typo");
    assert_eq!(route.model, "opus");
    assert_eq!(route.persona, "security-auditor");
    assert_eq!(route.effort, orchestrator_exec::Effort::High);
    for extra in [vec!["--persona"], vec!["--persona", " "]] {
        assert!(parse_arguments(base.into_iter().chain(extra)).is_err());
    }
    Ok(())
}

#[test]
fn observed_model_probes_replay_without_expanding_wire_contract() -> TestResult {
    for wire in [
        include_str!("codex-gpt-5.6-luna-success.jsonl"),
        include_str!("codex-gpt-5.6-sol-success.jsonl"),
    ] {
        assert!(codex::parse(wire.as_bytes()).is_ok());
        assert!(codex::parse(wire.replace("agent_message", "reasoning").as_bytes()).is_err());
        assert!(codex::parse(wire.replace("turn.completed", "turn.failed").as_bytes()).is_err());
        let scratch = Scratch::new()?;
        let options = codex_options(&scratch, &emit(&wire.lines().collect::<Vec<_>>()), 30)?;
        assert!(run_review(&options, CancellationToken::new())?.completed);
    }
    Ok(())
}

#[test]
fn composition_failure_still_saves_selected_route() -> TestResult {
    let scratch = Scratch::new()?;
    let mut options = codex_options(&scratch, "exit 99", 30)?;
    options.model = "m".repeat(257);
    assert!(matches!(
        run_review(&options, CancellationToken::new()),
        Err(PilotError::Composition(_))
    ));
    let record = result_json(&options)?;
    assert_eq!(record["status"], "failed");
    assert_eq!(record["route"]["model"], options.model);
    assert_eq!(record["route"]["tier"], "work");
    assert_eq!(record["route"]["persona"], "general-purpose");
    assert_eq!(record["route"]["effort"], "medium");
    assert!(
        record["route"]["selection_reason"]
            .as_str()
            .ok_or("reason")?
            .contains("override")
    );
    assert!(!options.output_dir.join("review.md").exists());
    Ok(())
}

const CODEX_CODE_WIRE: &str = include_str!("codex-code-success.jsonl");
const CLAUDE_CODE_WIRE: &str = include_str!(
    "../../orchestrator-provider-claude/tests/fixtures/claude-2.1.269-coding-success.jsonl"
);
const CLAUDE_CODE_DENIED_WIRE: &str = include_str!(
    "../../orchestrator-provider-claude/tests/fixtures/claude-2.1.269-coding-denied.jsonl"
);
// Exact six-frame excerpt from OBSERVED-EMPTY-WAIT.json. Source SHA-256:
// 56e1d76ceea7b7fce740d6797a3cc34bdef67be8e4b6a998f52fa05740bd2901.
// The source run failed ContractViolation; this fixture does not relabel that run.
const OBSERVED_EMPTY_WAIT_WIRE: &str = include_str!("codex-code-empty-wait-observed.jsonl");

fn code_wire(workspace: &Path) -> String {
    CODEX_CODE_WIRE.replace("/workspace", &workspace.to_string_lossy())
}

fn claude_code_wire(wire: &str, workspace: &Path) -> String {
    wire.replace("/workspace", &workspace.to_string_lossy())
}

fn observed_empty_wait_wire(wait_count: usize) -> String {
    let observed: Vec<&str> = OBSERVED_EMPTY_WAIT_WIRE.lines().collect();
    let code: Vec<&str> = CODEX_CODE_WIRE.lines().collect();
    observed[..2]
        .iter()
        .chain(observed[2..2 + wait_count * 2].iter())
        .chain([code[8], code[9]].iter())
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

fn code_progress_interleaving(workspace: &Path, tool: &str) -> String {
    let wire = code_wire(workspace);
    let lines: Vec<&str> = wire.lines().collect();
    let selected = match tool {
        "command" => [lines[0], lines[1], lines[3], lines[2], lines[4], lines[9]],
        "file-change" => [lines[0], lines[1], lines[6], lines[5], lines[7], lines[9]],
        _ => unreachable!(),
    };
    selected.join("\n")
}

fn code_options(
    scratch: &Scratch,
    repo: &Path,
    body: &str,
    seconds: u64,
) -> TestResult<PilotOptions> {
    let mut options = codex_options(scratch, body, seconds)?;
    options.command = PilotCommand::Code;
    options.repo = Some(repo.to_path_buf());
    fs::write(&options.prompt_file, "fix clamp")?;
    Ok(options)
}

#[test]
fn code_arguments_require_repository_and_preserve_review_compatibility() -> TestResult {
    let code = parse_arguments([
        "code",
        "--repo",
        "/local/repo",
        "--prompt-file",
        "p",
        "--output-dir",
        "o",
        "--model",
        "chosen",
        "--persona",
        "reviewer",
    ])?;
    assert_eq!(code.command, PilotCommand::Code);
    assert_eq!(code.runtime, "codex");
    assert_eq!(code.repo.as_deref(), Some(Path::new("/local/repo")));
    assert_eq!(code.model, "chosen");
    assert_eq!(code.persona.as_deref(), Some("reviewer"));
    for bad in [
        vec!["code", "--prompt-file", "p", "--output-dir", "o"],
        vec![
            "review",
            "--repo",
            "r",
            "--prompt-file",
            "p",
            "--output-dir",
            "o",
        ],
    ] {
        assert!(parse_arguments(bad).is_err());
    }
    let claude_code = parse_arguments([
        "code",
        "--repo",
        "r",
        "--prompt-file",
        "p",
        "--output-dir",
        "o",
        "--runtime",
        "claude",
        "--claude",
        "/supported/claude",
    ])?;
    assert_eq!(claude_code.runtime, "claude");
    assert_eq!(
        claude_code.claude_executable,
        OsString::from("/supported/claude")
    );
    for conflicting in [
        vec![
            "code",
            "--repo",
            "r",
            "--prompt-file",
            "p",
            "--output-dir",
            "o",
            "--runtime",
            "claude",
        ],
        vec![
            "code",
            "--repo",
            "r",
            "--prompt-file",
            "p",
            "--output-dir",
            "o",
            "--runtime",
            "claude",
            "--claude",
            "claude",
            "--codex",
            "codex",
        ],
        vec![
            "code",
            "--repo",
            "r",
            "--prompt-file",
            "p",
            "--output-dir",
            "o",
            "--claude",
            "claude",
        ],
    ] {
        assert!(parse_arguments(conflicting).is_err());
    }
    let review = parse_arguments(["review", "--prompt-file", "p", "--output-dir", "o"])?;
    assert_eq!(review.command, PilotCommand::Review);
    assert_eq!(review.runtime, "claude");
    let run = parse_arguments([
        "run",
        "--repo",
        "/local/repo",
        "--task-file",
        "task.md",
        "--output-dir",
        "out",
        "--verification-timeout-secs",
        "42",
        "--",
        "cargo",
        "test",
        "--all",
    ])?;
    assert_eq!(run.command, PilotCommand::Run);
    assert_eq!(run.runtime, "codex");
    assert_eq!(run.verification_timeout, Duration::from_secs(42));
    assert_eq!(
        run.verification_argv,
        ["cargo", "test", "--all"].map(OsString::from)
    );
    for bad in [
        vec![
            "run",
            "--repo",
            "r",
            "--task-file",
            "t",
            "--output-dir",
            "o",
        ],
        vec![
            "run",
            "--repo",
            "r",
            "--prompt-file",
            "t",
            "--output-dir",
            "o",
            "--",
            "verify",
        ],
    ] {
        assert!(parse_arguments(bad).is_err());
    }
    Ok(())
}

#[test]
fn cancel_arguments_require_exact_mission_and_saved_output_only() -> TestResult {
    let cancel = parse_arguments([
        "cancel",
        "--output-dir",
        "saved-run",
        "--mission",
        "rust-durable-00000000000000",
    ])?;
    assert_eq!(cancel.command, PilotCommand::Cancel);
    assert_eq!(cancel.output_dir, PathBuf::from("saved-run"));
    assert_eq!(
        cancel.mission_id.as_deref(),
        Some("rust-durable-00000000000000")
    );
    assert!(parse_arguments(["cancel", "--output-dir", "saved-run"]).is_err());
    assert!(parse_arguments(["cancel", "--output-dir", "saved-run", "--mission", "",]).is_err());
    assert!(
        parse_arguments([
            "cancel",
            "--output-dir",
            "saved-run",
            "--mission",
            "mission",
            "--runtime",
            "codex",
        ])
        .is_err()
    );
    Ok(())
}

#[test]
fn coding_snapshot_preserves_source_and_saves_diff_route_and_actual_argv() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.git_repo("repo")?;
    let source_before = fs::read(repo.join("math_utils.py"))?;
    let workspace = scratch.0.join("out/workspace");
    let wire = code_wire(&workspace);
    let body = format!(
        "test \"$ZDOTDIR\" = \"${{PWD%/workspace}}/shell-config\" || exit 91\ntest \"$BASH_ENV\" = /dev/null || exit 92\ntest \"$ENV\" = /dev/null || exit 93\nprintf 'def clamp(value, low, high):\\n    return max(low, min(value, high))\\n' > math_utils.py\nprintf 'new note\\n' > notes.txt\n{}",
        emit(&wire.lines().collect::<Vec<_>>())
    );
    let mut options = code_options(&scratch, &repo, &body, 30)?;
    options.model = "explicit-code-model".to_owned();
    options.persona = Some("reviewer".to_owned());
    let summary = run_review(&options, CancellationToken::new())?;
    assert!(summary.completed, "{}", summary.reason);

    assert_eq!(fs::read(repo.join("math_utils.py"))?, source_before);
    assert!(git(&repo, &["status", "--porcelain=v1"])?.is_empty());
    assert_eq!(
        fs::read_to_string(workspace.join("notes.txt"))?,
        "new note\n"
    );
    let diff = fs::read_to_string(options.output_dir.join("changes.diff"))?;
    assert!(diff.contains("math_utils.py"), "{diff}");
    assert!(diff.contains("notes.txt"), "{diff}");
    let record = result_json(&options)?;
    assert_eq!(record["status"], "completed");
    assert_eq!(record["provider_completed"], true);
    assert_eq!(record["tests_verified"], false);
    assert_eq!(record["source_repository"]["preserved"], true);
    assert_eq!(record["route"]["model"], "explicit-code-model");
    assert_eq!(record["route"]["persona"], "reviewer");
    assert_eq!(record["route"]["effort"], "medium");
    assert_eq!(
        record["process"]["working_root"],
        workspace.display().to_string()
    );
    let argv: Vec<String> =
        serde_json::from_value(record["process"]["argv_after_executable"].clone())?;
    for required in [
        "workspace-write",
        "sandbox_workspace_write.network_access=false",
        "explicit-code-model",
        "model_reasoning_effort=\"medium\"",
    ] {
        assert!(
            argv.iter().any(|argument| argument == required),
            "{required}"
        );
    }
    for feature in ["shell_tool", "unified_exec", "code_mode_host"] {
        assert!(
            argv.windows(2).any(|pair| pair == ["--enable", feature]),
            "{feature}"
        );
    }
    for feature in [
        "hooks",
        "plugins",
        "apps",
        "multi_agent",
        "multi_agent_v2",
        "skill_search",
    ] {
        assert!(
            argv.windows(2).any(|pair| pair == ["--disable", feature]),
            "{feature}"
        );
    }
    assert!(options.output_dir.join("answer.md").is_file());
    assert!(!options.output_dir.join("review.md").exists());
    Ok(())
}

#[test]
fn claude_coding_uses_restricted_file_tools_and_preserves_source() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.git_repo("claude-repo")?;
    let source_before = fs::read(repo.join("math_utils.py"))?;
    let workspace = scratch.0.join("out/workspace");
    let wire = claude_code_wire(CLAUDE_CODE_WIRE, &workspace);
    let body = format!(
        "printf 'def clamp(value, low, high):\\n    return max(low, min(value, high))\\n' > math_utils.py\n{}",
        emit(&wire.lines().collect::<Vec<_>>())
    );
    let claude = scratch.fake_claude("2.1.269", &body)?;
    let mut options = scratch.options(&claude, 30)?;
    options.command = PilotCommand::Code;
    options.repo = Some(repo.clone());
    options.runtime = "claude".to_owned();
    fs::write(&options.prompt_file, "fix clamp")?;

    let summary = run_review(&options, CancellationToken::new())?;
    assert!(summary.completed, "{}", summary.reason);
    assert_eq!(fs::read(repo.join("math_utils.py"))?, source_before);
    assert!(options.output_dir.join("answer.md").is_file());
    let record = result_json(&options)?;
    assert_eq!(record["provider_completed"], true);
    assert_eq!(record["source_repository"]["preserved"], true);
    assert_eq!(record["route"]["model"], "sonnet");
    assert_eq!(record["route"]["effort"], "medium");
    let argv: Vec<String> =
        serde_json::from_value(record["process"]["argv_after_executable"].clone())?;
    for required in [
        "--restricted",
        "--safe-mode",
        "--strict-mcp-config",
        "--no-session-persistence",
        "acceptEdits",
        "Read,Edit,Write,Glob,Grep",
        "sonnet",
        "medium",
        "5",
    ] {
        assert!(
            argv.iter().any(|argument| argument == required),
            "{required}"
        );
    }
    for pair in [
        ["--setting-sources", ""],
        ["--permission-mode", "acceptEdits"],
        ["--tools", "Read,Edit,Write,Glob,Grep"],
        ["--model", "sonnet"],
        ["--effort", "medium"],
        ["--max-turns", "5"],
        ["--mcp-config", r#"{"mcpServers":{}}"#],
    ] {
        assert!(argv.windows(2).any(|actual| actual == pair), "{pair:?}");
    }
    assert!(!argv.iter().any(|argument| argument.contains("dangerously")));
    for forbidden in ["--add-dir", "--settings", "--plugin-dir", "--chrome"] {
        assert!(!argv.iter().any(|argument| argument == forbidden));
    }
    Ok(())
}

#[test]
fn claude_coding_denial_and_missing_snapshot_change_fail_honestly() -> TestResult {
    for (wire, reason) in [
        (CLAUDE_CODE_DENIED_WIRE, "denied"),
        (CLAUDE_CODE_WIRE, "snapshot did not change"),
    ] {
        let scratch = Scratch::new()?;
        let repo = scratch.git_repo("repo")?;
        let source_before = fs::read(repo.join("math_utils.py"))?;
        let workspace = scratch.0.join("out/workspace");
        let wire = claude_code_wire(wire, &workspace);
        let body = emit(&wire.lines().collect::<Vec<_>>());
        let claude = scratch.fake_claude("2.1.269", &body)?;
        let mut options = scratch.options(&claude, 30)?;
        options.command = PilotCommand::Code;
        options.repo = Some(repo.clone());
        options.runtime = "claude".to_owned();
        fs::write(&options.prompt_file, "fix clamp")?;

        let summary = run_review(&options, CancellationToken::new())?;
        assert!(!summary.completed);
        assert!(summary.reason.contains(reason), "{}", summary.reason);
        assert_eq!(fs::read(repo.join("math_utils.py"))?, source_before);
        assert!(!options.output_dir.join("answer.md").exists());
        assert_eq!(result_json(&options)?["provider_completed"], false);
    }
    Ok(())
}

#[test]
fn coding_parser_accepts_exact_observed_empty_wait_pairs_before_the_final_answer() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = scratch.0.join("workspace");
    fs::create_dir(&workspace)?;

    let single = codex::parse_code(observed_empty_wait_wire(1).as_bytes(), &workspace)?;
    assert_eq!(
        single.output,
        "Updated math_utils.py. Verification was not run."
    );
    assert_eq!(single.command_count, 0);
    assert_eq!(single.file_change_count, 0);
    assert!(!single.tool_failed);

    let repeated = observed_empty_wait_wire(2);
    let repeated_parsed = codex::parse_code(repeated.as_bytes(), &workspace)?;
    assert_eq!(repeated_parsed.output, single.output);
    assert_eq!(repeated_parsed.command_count, 0);
    assert_eq!(repeated_parsed.file_change_count, 0);
    assert!(!repeated_parsed.tool_failed);
    assert!(
        codex::parse(repeated.as_bytes()).is_err(),
        "review parsing must remain collaboration-free"
    );
    Ok(())
}

#[test]
fn coding_parser_rejects_nonempty_or_non_wait_collaboration() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = scratch.0.join("workspace");
    fs::create_dir(&workspace)?;
    let good = observed_empty_wait_wire(1);
    let thread_id = "01a09b5e-9036-79c2-bba6-c7eee5ae2ad7";

    let rejected = [
        (
            "unknown item field",
            good.replacen(
                "\"status\":\"in_progress\"}",
                "\"status\":\"in_progress\",\"unknown\":true}",
                1,
            ),
        ),
        (
            "unknown event field",
            good.replacen(
                "{\"type\":\"turn.started\"}",
                "{\"type\":\"turn.started\",\"unknown\":true}",
                1,
            ),
        ),
        (
            "nonempty receivers",
            good.replacen(
                "\"receiver_thread_ids\":[]",
                "\"receiver_thread_ids\":[\"receiver\"]",
                1,
            ),
        ),
        (
            "nonempty agent state",
            good.replacen(
                "\"agents_states\":{}",
                "\"agents_states\":{\"child\":{}}",
                1,
            ),
        ),
        (
            "non-null prompt",
            good.replacen("\"prompt\":null", "\"prompt\":\"wake\"", 1),
        ),
        (
            "foreign sender",
            good.replace(
                &format!("\"sender_thread_id\":\"{thread_id}\""),
                "\"sender_thread_id\":\"foreign-thread\"",
            ),
        ),
        (
            "missing required prompt",
            good.replacen(",\"prompt\":null", "", 1),
        ),
    ];
    for (name, wire) in rejected {
        assert!(
            codex::parse_code(wire.as_bytes(), &workspace).is_err(),
            "accepted {name}"
        );
    }
    for tool in ["spawn", "send", "close"] {
        let wire = good.replace("\"tool\":\"wait\"", &format!("\"tool\":\"{tool}\""));
        assert!(
            codex::parse_code(wire.as_bytes(), &workspace).is_err(),
            "accepted collab tool {tool}"
        );
    }
    Ok(())
}

#[test]
fn coding_parser_requires_a_unique_ordered_completed_empty_wait_pair() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = scratch.0.join("workspace");
    fs::create_dir(&workspace)?;
    let good = observed_empty_wait_wire(1);
    let lines: Vec<&str> = good.lines().collect();

    let missing_completion = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| (index != 3).then_some(*line))
        .collect::<Vec<_>>()
        .join("\n");
    let mut duplicate_completion_lines = lines.clone();
    duplicate_completion_lines.insert(4, lines[3]);
    let duplicate_completion = duplicate_completion_lines.join("\n");
    let mut out_of_order_lines = lines.clone();
    out_of_order_lines.swap(2, 3);
    let out_of_order_completion = out_of_order_lines.join("\n");
    let malformed_completion = good.replacen(
        "\"agents_states\":{},\"status\":\"completed\"",
        "\"agents_states\":{}",
        1,
    );
    let failed_completion = good.replacen("\"status\":\"completed\"", "\"status\":\"failed\"", 1);
    let mismatched_completion = lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 3 {
                line.replace("\"id\":\"item_41\"", "\"id\":\"other-item\"")
            } else {
                (*line).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let repeated = observed_empty_wait_wire(2);
    let repeated_lines: Vec<&str> = repeated.lines().collect();
    let overlapping_out_of_order = [
        repeated_lines[0],
        repeated_lines[1],
        repeated_lines[2],
        repeated_lines[4],
        repeated_lines[5],
        repeated_lines[3],
        repeated_lines[6],
        repeated_lines[7],
    ]
    .join("\n");
    let duplicate_id = repeated.replace("item_52", "item_41");

    for (name, wire) in [
        ("started item left at terminal", missing_completion),
        ("duplicate completion", duplicate_completion),
        ("completion before start", out_of_order_completion),
        ("malformed completion", malformed_completion),
        ("failed completion", failed_completion),
        ("mismatched completion", mismatched_completion),
        ("overlapping out-of-order waits", overlapping_out_of_order),
        ("duplicate item id", duplicate_id),
    ] {
        assert!(
            codex::parse_code(wire.as_bytes(), &workspace).is_err(),
            "accepted {name}"
        );
    }
    Ok(())
}

#[test]
fn coding_parser_accepts_command_failures_and_rejects_malformed_activity() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = scratch.0.join("workspace");
    fs::create_dir(&workspace)?;
    let good = code_wire(&workspace);
    let parsed = codex::parse_code(good.as_bytes(), &workspace)?;
    assert!(!parsed.tool_failed);
    assert_eq!(parsed.command_count, 1);
    assert_eq!(parsed.file_change_count, 1);
    let add_delete = include_str!("codex-code-add-delete-success.jsonl")
        .replace("/workspace", &workspace.to_string_lossy());
    let parsed = codex::parse_code(add_delete.as_bytes(), &workspace)?;
    assert_eq!(parsed.file_change_count, 1);

    let stale_progress = good
        .lines()
        .filter(|line| !line.contains("\"id\":\"item_4\""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(codex::parse_code(stale_progress.as_bytes(), &workspace).is_err());

    let tool_failed = good.replacen("\"exit_code\":0", "\"exit_code\":7", 1);
    assert!(codex::parse_code(tool_failed.as_bytes(), &workspace)?.tool_failed);
    let native_failed = good
        .lines()
        .map(|line| {
            if line.contains("\"type\":\"item.completed\"")
                && line.contains("\"command_execution\"")
            {
                line.replace("\"status\":\"completed\"", "\"status\":\"failed\"")
                    .replace("\"exit_code\":0", "\"exit_code\":1")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let parsed = codex::parse_code(native_failed.as_bytes(), &workspace)?;
    assert_eq!(
        parsed.output,
        "Updated math_utils.py. Verification was not run."
    );
    assert!(parsed.tool_failed);
    let failed_zero = native_failed.replace("\"exit_code\":1", "\"exit_code\":0");
    assert!(codex::parse_code(failed_zero.as_bytes(), &workspace).is_err());
    let failed_file_change = good
        .lines()
        .map(|line| {
            if line.contains("\"type\":\"item.completed\"") && line.contains("\"file_change\"") {
                line.replace("\"status\":\"completed\"", "\"status\":\"failed\"")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(codex::parse_code(failed_file_change.as_bytes(), &workspace).is_err());
    let missing_answer = good
        .lines()
        .filter(|line| !line.contains("\"type\":\"agent_message\""))
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(missing_answer, good);
    for bad in [
        good.lines()
            .take(good.lines().count() - 1)
            .collect::<Vec<_>>()
            .join("\n"),
        good.replace(
            &format!("{}/math_utils.py", workspace.display()),
            "/outside/math_utils.py",
        ),
        good.replace("\"status\":\"completed\"", "\"status\":\"in_progress\""),
        missing_answer,
        format!("{good}\n{{broken"),
    ] {
        assert!(codex::parse_code(bad.as_bytes(), &workspace).is_err());
    }
    Ok(())
}

#[test]
fn coding_parser_requires_a_final_answer_after_each_tool_completion() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = scratch.0.join("workspace");
    fs::create_dir(&workspace)?;
    let wire = code_wire(&workspace);
    let final_answer = wire.lines().nth(8).ok_or("final answer")?;
    for tool in ["command", "file-change"] {
        let progress_only = code_progress_interleaving(&workspace, tool);
        assert!(
            codex::parse_code(progress_only.as_bytes(), &workspace).is_err(),
            "accepted progress before {tool} completion as the final answer"
        );
        let with_final = progress_only.replace(
            "{\"type\":\"turn.completed\"",
            &format!("{final_answer}\n{{\"type\":\"turn.completed\""),
        );
        assert_eq!(
            codex::parse_code(with_final.as_bytes(), &workspace)?.output,
            "Updated math_utils.py. Verification was not run.",
            "rejected final answer after {tool} completion"
        );
    }
    Ok(())
}

#[test]
fn coding_fake_provider_does_not_save_progress_before_tool_completion() -> TestResult {
    for tool in ["command", "file-change"] {
        let scratch = Scratch::new()?;
        let repo = scratch.git_repo("repo")?;
        let workspace = scratch.0.join("out/workspace");
        let wire = code_progress_interleaving(&workspace, tool);
        let options = code_options(
            &scratch,
            &repo,
            &emit(&wire.lines().collect::<Vec<_>>()),
            30,
        )?;
        let summary = run_review(&options, CancellationToken::new())?;
        assert!(!summary.completed, "{tool}");
        assert!(!options.output_dir.join("answer.md").exists(), "{tool}");
    }
    Ok(())
}

#[test]
fn coding_native_failed_command_can_recover_and_complete() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.git_repo("repo")?;
    let workspace = scratch.0.join("out/workspace");
    let wire = code_wire(&workspace)
        .lines()
        .map(|line| {
            if line.contains("\"type\":\"item.completed\"")
                && line.contains("\"command_execution\"")
            {
                line.replace("\"status\":\"completed\"", "\"status\":\"failed\"")
                    .replace("\"exit_code\":0", "\"exit_code\":9")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let options = code_options(
        &scratch,
        &repo,
        &emit(&wire.lines().collect::<Vec<_>>()),
        30,
    )?;

    let summary = run_review(&options, CancellationToken::new())?;

    assert!(summary.completed, "{}", summary.reason);
    assert_eq!(
        summary.reason,
        "coding completed with failed command observations; independent verification pending"
    );
    assert!(options.output_dir.join("answer.md").is_file());
    let record = result_json(&options)?;
    assert_eq!(record["status"], "completed");
    assert_eq!(record["codex_protocol"]["tool_failed"], true);
    assert_eq!(record["provider_completed"], true);
    assert_eq!(record["tests_verified"], false);
    assert_eq!(record["source_repository"]["preserved"], true);
    assert_eq!(record["process"]["cleanup_complete"], true);
    Ok(())
}

#[test]
fn coding_malformed_process_timeout_and_cancel_are_not_success() -> TestResult {
    for mode in ["stale", "malformed", "exit", "timeout", "cancel"] {
        let scratch = Scratch::new()?;
        let repo = scratch.git_repo("repo")?;
        let workspace = scratch.0.join("out/workspace");
        let good = code_wire(&workspace);
        let provider_ready = scratch.0.join("provider-ready");
        let body = match mode {
            "stale" => emit(
                &good
                    .lines()
                    .filter(|line| !line.contains("\"id\":\"item_4\""))
                    .collect::<Vec<_>>(),
            ),
            "malformed" => emit(&["{\"type\":\"turn.completed\"}"]),
            "exit" => format!("{}\nexit 7", emit(&good.lines().collect::<Vec<_>>())),
            _ => format!(
                ": > '{}'; echo started; exec sleep 60",
                provider_ready.display()
            ),
        };
        let options = code_options(
            &scratch,
            &repo,
            &body,
            if mode == "timeout" { 1 } else { 30 },
        )?;
        let token = CancellationToken::new();
        let canceller = if mode == "cancel" {
            let token = token.clone();
            Some(thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(15);
                while !provider_ready.exists() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
                token.cancel();
                assert!(
                    provider_ready.exists(),
                    "provider did not start before cancellation deadline"
                );
            }))
        } else {
            None
        };
        let summary = run_review(&options, token)?;
        if let Some(handle) = canceller {
            handle.join().map_err(|_| "canceller")?;
        }
        assert!(!summary.completed, "{mode}");
        assert!(!options.output_dir.join("answer.md").exists(), "{mode}");
        assert!(options.output_dir.join("workspace").is_dir(), "{mode}");
        assert!(options.output_dir.join("changes.diff").is_file(), "{mode}");
        let record = result_json(&options)?;
        assert_eq!(record["status"], "failed", "{mode}");
        assert_eq!(record["process"]["cleanup_complete"], true, "{mode}");
        if mode == "timeout" {
            assert_eq!(record["process"]["deadline_observed"], true);
        }
        if mode == "cancel" {
            assert_eq!(record["process"]["cancellation_observed"], true);
        }
    }
    Ok(())
}

#[test]
fn coding_rejects_dirty_symlink_and_submodule_sources() -> TestResult {
    for mode in ["dirty", "symlink", "submodule"] {
        let scratch = Scratch::new()?;
        let repo = scratch.git_repo("repo")?;
        match mode {
            "dirty" => fs::write(repo.join("untracked.txt"), "dirty\n")?,
            "symlink" => {
                std::os::unix::fs::symlink("math_utils.py", repo.join("linked.py"))?;
                git(&repo, &["add", "linked.py"])?;
                git(&repo, &["commit", "-q", "-m", "symlink"])?;
            }
            "submodule" => {
                let head = git(&repo, &["rev-parse", "HEAD"])?.trim().to_owned();
                let cache = format!("160000,{head},vendor");
                git(&repo, &["update-index", "--add", "--cacheinfo", &cache])?;
                git(&repo, &["commit", "-q", "-m", "gitlink"])?;
            }
            _ => return Err("unknown fixture mode".into()),
        }
        let options = code_options(&scratch, &repo, "exit 99", 30)?;
        assert!(matches!(
            run_review(&options, CancellationToken::new()),
            Err(PilotError::Repository { .. })
        ));
        assert!(!options.output_dir.join("codex-stdout.jsonl").exists());
    }
    Ok(())
}

fn cycle_options(
    scratch: &Scratch,
    review_answer: &str,
    verification_body: &str,
    verification_timeout: u64,
) -> TestResult<PilotOptions> {
    cycle_options_with_review_exit(
        scratch,
        review_answer,
        verification_body,
        verification_timeout,
        0,
    )
}

fn cycle_options_with_review_exit(
    scratch: &Scratch,
    review_answer: &str,
    verification_body: &str,
    verification_timeout: u64,
    review_exit: u8,
) -> TestResult<PilotOptions> {
    let repo = scratch.git_repo("cycle-repo")?;
    let workspace = scratch.0.join("out/workspace");
    let coding_wire = code_wire(&workspace);
    let review_wire = format!(
        "{{\"type\":\"thread.started\",\"thread_id\":\"review\"}}\n{{\"type\":\"turn.started\"}}\n{{\"type\":\"item.completed\",\"item\":{{\"id\":\"answer\",\"type\":\"agent_message\",\"text\":{}}}}}\n{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":10,\"cached_input_tokens\":0,\"cache_write_input_tokens\":0,\"output_tokens\":2,\"reasoning_output_tokens\":0}}}}",
        serde_json::to_string(review_answer)?
    );
    let body = format!(
        "input=$(cat)\ncase \"$input\" in\n  *\"Review the implementation below\"*)\n{}\n    exit {}\n    ;;\n  *) printf 'def clamp(value, low, high):\\n    return max(low, min(value, high))\\n' > math_utils.py\n{}\n    ;;\nesac",
        emit(&review_wire.lines().collect::<Vec<_>>()),
        review_exit,
        emit(&coding_wire.lines().collect::<Vec<_>>())
    );
    let mut options = codex_options(scratch, &body, 30)?;
    options.command = PilotCommand::Run;
    options.repo = Some(repo);
    fs::write(
        &options.prompt_file,
        "Fix clamp so it observes both bounds.",
    )?;
    let verifier = scratch.file("verifier", &format!("#!/bin/sh\n{verification_body}\n"))?;
    fs::set_permissions(&verifier, fs::Permissions::from_mode(0o755))?;
    options.verification_argv = vec![verifier.into_os_string()];
    options.verification_timeout = Duration::from_secs(verification_timeout);
    Ok(options)
}

const PASS_VERDICT: &str =
    r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#;

#[test]
fn fixed_cycle_succeeds_with_structured_review_and_positive_verification() -> TestResult {
    let scratch = Scratch::new()?;
    let options = cycle_options(
        &scratch,
        PASS_VERDICT,
        "printf '%s\\n' '{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":2,\"executed\":2,\"passed\":2,\"failed\":0,\"required_skipped\":0}' > \"$NANIKA_VERIFICATION_REPORT\"",
        30,
    )?;
    let source = options.repo.clone().ok_or("repo")?;
    let source_before = fs::read(source.join("math_utils.py"))?;
    let summary = cycle::run(&options, CancellationToken::new())?;
    assert!(summary.completed, "{}", summary.reason);
    assert_eq!(fs::read(source.join("math_utils.py"))?, source_before);
    assert!(git(&source, &["status", "--porcelain=v1"])?.is_empty());
    let result = result_json(&options)?;
    assert_eq!(result["status"], "completed");
    assert_eq!(result["tests_verified"], true);
    assert_eq!(result["phases"].as_array().ok_or("phases")?.len(), 3);
    for phase in ["code", "review", "verification"] {
        assert!(
            options
                .output_dir
                .join(phase)
                .join("phase-result.json")
                .is_file()
        );
    }
    let review_prompt = fs::read_to_string(options.output_dir.join("review/prompt.md"))?;
    assert!(review_prompt.contains("Fix clamp"));
    assert!(review_prompt.contains("diff --git"));
    assert!(review_prompt.contains("complete final contents of modified file \"math_utils.py\""));
    assert!(!review_prompt.contains("complete original contents of modified file"));
    Ok(())
}

#[test]
fn fixed_cycle_rejects_incomplete_review_process() -> TestResult {
    let scratch = Scratch::new()?;
    let options =
        cycle_options_with_review_exit(&scratch, PASS_VERDICT, "touch verifier-was-run", 30, 7)?;
    let summary = cycle::run(&options, CancellationToken::new())?;
    assert!(!summary.completed);
    let result = result_json(&options)?;
    assert_eq!(result["phases"][1]["status"], "failed");
    assert_eq!(result["phases"][1]["verdict_valid"], false);
    assert!(!options.output_dir.join("review/answer.md").exists());
    assert!(!options.output_dir.join("review/verdict.json").exists());
    assert!(
        !options
            .output_dir
            .join("workspace/verifier-was-run")
            .exists()
    );
    Ok(())
}

#[test]
fn partial_pass_verdict_from_incomplete_attempt_is_not_valid_review_output() -> TestResult {
    let partial = orchestrator_exec::PartialWork::new(
        Some(PASS_VERDICT.to_owned()),
        orchestrator_exec::AttemptEvidence::new(),
    )?;
    let outcome = AttemptOutcome::incomplete(
        orchestrator_exec::MechanicalTermination::Cancelled,
        None,
        partial,
        Duration::from_millis(1),
    );
    assert_eq!(outcome.output(), Some(PASS_VERDICT));
    assert!(cycle::completed_review_answer(&outcome).is_err());
    Ok(())
}

#[test]
fn fixed_cycle_rejects_workspace_loss_replacement_and_reviewed_source_change() -> TestResult {
    for mode in ["missing", "replaced", "changed", "unchecked"] {
        let scratch = Scratch::new()?;
        let mutation = match mode {
            "missing" => "mv \"$PWD\" \"${PWD}-moved\"",
            "replaced" => "mv \"$PWD\" \"${PWD}-moved\"; mkdir \"$PWD\"",
            "changed" => "printf 'changed\\n' > math_utils.py",
            "unchecked" => "printf 'unchecked\\n' > generated_source.rs",
            _ => unreachable!(),
        };
        let body = format!(
            "printf '%s\\n' '{{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":1,\"executed\":1,\"passed\":1,\"failed\":0,\"required_skipped\":0}}' > \"$NANIKA_VERIFICATION_REPORT\"; {mutation}"
        );
        let options = cycle_options(&scratch, PASS_VERDICT, &body, 30)?;
        let summary = cycle::run(&options, CancellationToken::new())?;
        assert!(!summary.completed, "{mode}");
        let result = result_json(&options)?;
        assert_eq!(result["phases"][2]["status"], "failed", "{mode}");
        assert_eq!(
            result["phases"][2]["reviewed_workspace_preserved"], false,
            "{mode}"
        );
        assert_eq!(result["tests_verified"], false, "{mode}");
    }
    Ok(())
}

#[test]
fn fixed_cycle_allows_and_reports_ignored_build_outputs() -> TestResult {
    let scratch = Scratch::new()?;
    let options = cycle_options(
        &scratch,
        PASS_VERDICT,
        "mkdir -p target/debug; printf 'object\\n' > target/debug/build.o; printf '%s\\n' '{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":1,\"executed\":1,\"passed\":1,\"failed\":0,\"required_skipped\":0}' > \"$NANIKA_VERIFICATION_REPORT\"",
        30,
    )?;
    let repo = options.repo.as_ref().ok_or("repo")?;
    fs::write(repo.join(".gitignore"), "/target/\n")?;
    git(repo, &["add", ".gitignore"])?;
    git(repo, &["commit", "-q", "-m", "ignore build output"])?;

    let summary = cycle::run(&options, CancellationToken::new())?;
    assert!(summary.completed, "{}", summary.reason);
    let result = result_json(&options)?;
    assert_eq!(result["phases"][2]["reviewed_workspace_preserved"], true);
    assert_eq!(result["phases"][2]["generated_outputs"]["count"], 3);
    assert!(
        options
            .output_dir
            .join("verification/generated-outputs.json")
            .is_file()
    );
    Ok(())
}

#[test]
fn fixed_cycle_sends_large_review_context_to_codex_stdin_not_argv() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.git_repo("large-review-context-repo")?;
    let workspace = scratch.0.join("out/workspace");
    let coding_wire = code_wire(&workspace);
    let review_wire = format!(
        "{{\"type\":\"thread.started\",\"thread_id\":\"review\"}}\n{{\"type\":\"turn.started\"}}\n{{\"type\":\"item.completed\",\"item\":{{\"id\":\"answer\",\"type\":\"agent_message\",\"text\":{}}}}}\n{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":10,\"cached_input_tokens\":0,\"cache_write_input_tokens\":0,\"output_tokens\":2,\"reasoning_output_tokens\":0}}}}",
        serde_json::to_string(PASS_VERDICT)?
    );
    let context = "review-context-marker-".repeat(512);
    let body = format!(
        "capture_dir=$(dirname \"$0\")\ncat > \"$capture_dir/review-stdin.bin\"\nprintf '%s\\n' \"$@\" > \"$capture_dir/review-argv.txt\"\nif grep -q 'Review the implementation below' \"$capture_dir/review-stdin.bin\"; then\n{}\nelse\nprintf '%s' '{}' > review-context.txt\n{}\nfi",
        emit(&review_wire.lines().collect::<Vec<_>>()),
        context,
        emit(&coding_wire.lines().collect::<Vec<_>>()),
    );
    let mut options = codex_options(&scratch, &body, 30)?;
    options.command = PilotCommand::Run;
    options.repo = Some(repo);
    fs::write(
        &options.prompt_file,
        "Make the large review context fixture.",
    )?;
    let verifier = scratch.file(
        "verifier",
        "#!/bin/sh\nprintf '%s\\n' '{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":1,\"executed\":1,\"passed\":1,\"failed\":0,\"required_skipped\":0}' > \"$NANIKA_VERIFICATION_REPORT\"\n",
    )?;
    fs::set_permissions(&verifier, fs::Permissions::from_mode(0o755))?;
    options.verification_argv = vec![verifier.into_os_string()];

    let summary = cycle::run(&options, CancellationToken::new())?;
    assert!(summary.completed, "{}", summary.reason);
    let review_prompt = fs::read(options.output_dir.join("review/prompt.md"))?;
    assert!(review_prompt.len() > 8 * 1024);
    assert_eq!(fs::read(scratch.0.join("review-stdin.bin"))?, review_prompt);
    let argv = fs::read_to_string(scratch.0.join("review-argv.txt"))?;
    assert!(argv.lines().any(|argument| argument == "--"));
    assert!(argv.lines().any(|argument| argument == "-"));
    assert!(!argv.contains("review-context-marker-"));
    Ok(())
}

#[test]
fn fixed_cycle_rejects_blocker_and_malformed_review_before_verification() -> TestResult {
    for answer in [
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"reject","blockers":["wrong clamp"],"warnings":[]}"#,
        "Looks good to me",
    ] {
        let scratch = Scratch::new()?;
        let options = cycle_options(&scratch, answer, "touch verifier-was-run", 30)?;
        let summary = cycle::run(&options, CancellationToken::new())?;
        assert!(!summary.completed);
        assert!(
            !options
                .output_dir
                .join("workspace/verifier-was-run")
                .exists()
        );
        let result = result_json(&options)?;
        assert_eq!(result["phases"][1]["status"], "failed");
        assert_eq!(result["phases"][2]["status"], "skipped");
    }
    Ok(())
}

#[test]
fn fixed_cycle_rejects_failed_empty_skipped_invalid_and_missing_verification() -> TestResult {
    for (name, body) in [
        (
            "failed",
            "printf '%s\\n' '{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":1,\"executed\":1,\"passed\":0,\"failed\":1,\"required_skipped\":0}' > \"$NANIKA_VERIFICATION_REPORT\"; exit 7",
        ),
        (
            "empty",
            "printf '%s\\n' '{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":0,\"executed\":0,\"passed\":0,\"failed\":0,\"required_skipped\":0}' > \"$NANIKA_VERIFICATION_REPORT\"",
        ),
        (
            "skipped",
            "printf '%s\\n' '{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":2,\"executed\":1,\"passed\":1,\"failed\":0,\"required_skipped\":1}' > \"$NANIKA_VERIFICATION_REPORT\"",
        ),
        (
            "invalid",
            "printf '%s\\n' '{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":2,\"executed\":1,\"passed\":2,\"failed\":0,\"required_skipped\":0}' > \"$NANIKA_VERIFICATION_REPORT\"",
        ),
        ("missing", ":"),
    ] {
        let scratch = Scratch::new()?;
        let options = cycle_options(&scratch, PASS_VERDICT, body, 30)?;
        let summary = cycle::run(&options, CancellationToken::new())?;
        assert!(!summary.completed, "{name}");
        assert_eq!(
            result_json(&options)?["phases"][2]["status"],
            "failed",
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn fixed_cycle_rejects_coder_seeded_report_and_verifier_timeout() -> TestResult {
    for mode in ["stale", "timeout"] {
        let scratch = Scratch::new()?;
        let mut options = cycle_options(
            &scratch,
            PASS_VERDICT,
            if mode == "timeout" {
                "exec sleep 60"
            } else {
                ":"
            },
            if mode == "timeout" { 1 } else { 30 },
        )?;
        if mode == "stale" {
            let workspace = scratch.0.join("out/workspace");
            let wire = code_wire(&workspace);
            let stale = "{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":1,\"executed\":1,\"passed\":1,\"failed\":0,\"required_skipped\":0}";
            let review_wire = format!(
                "{{\"type\":\"thread.started\",\"thread_id\":\"review\"}}\n{{\"type\":\"turn.started\"}}\n{{\"type\":\"item.completed\",\"item\":{{\"id\":\"answer\",\"type\":\"agent_message\",\"text\":{}}}}}\n{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"cache_write_input_tokens\":0,\"output_tokens\":1,\"reasoning_output_tokens\":0}}}}",
                serde_json::to_string(PASS_VERDICT)?
            );
            let body = format!(
                "input=$(cat)\ncase \"$input\" in\n  *\"Review the implementation below\"*)\n{}\n    ;;\n  *) printf '%s\\n' '{}' > ../verification/report.json; printf 'def clamp(value, low, high):\\n    return max(low, min(value, high))\\n' > math_utils.py\n{}\n    ;;\nesac",
                emit(&review_wire.lines().collect::<Vec<_>>()),
                stale,
                emit(&wire.lines().collect::<Vec<_>>())
            );
            let executable = scratch.file("stale-codex", &format!("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.154.0'; exit 0; fi\n{body}\n"))?;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
            options.codex_executable = executable.into_os_string();
        }
        let summary = cycle::run(&options, CancellationToken::new())?;
        assert!(!summary.completed, "{mode}");
        let record = result_json(&options)?;
        assert_eq!(record["phases"][2]["status"], "failed", "{mode}");
        if mode == "timeout" {
            assert_eq!(record["phases"][2]["process"]["deadline_observed"], true);
        }
    }
    Ok(())
}

#[test]
fn usage_artifact_collision_is_diagnostic_and_never_overwrites() -> TestResult {
    let scratch = Scratch::new()?;
    fs::write(scratch.0.join("worker-usage.json"), b"existing")?;
    let observation = Observation {
        stdout: RECORDED_2_1_269_WIRE.as_bytes().to_vec(),
        stderr: Vec::new(),
        summary: json!({"stdout_discarded_bytes":0}),
    };
    let usage = record_worker_usage(&scratch.0, "claude", Some(&observation), None);
    assert_eq!(usage["status"], "unavailable");
    assert!(usage.get("report_artifact").is_none());
    assert!(usage.get("summary").is_none());
    assert_eq!(fs::read(scratch.0.join("worker-usage.json"))?, b"existing");
    Ok(())
}

#[test]
fn usage_capture_cannot_turn_a_failed_provider_into_success() -> TestResult {
    let scratch = Scratch::new()?;
    let lines: Vec<&str> = RECORDED_2_1_269_WIRE.lines().collect();
    let claude = scratch.fake_claude("2.1.269", &(emit(&lines) + "\nexit 1\n"))?;
    let options = scratch.options(&claude, 30)?;
    let summary = run_review(&options, CancellationToken::new())?;
    assert!(!summary.completed);
    let result = result_json(&options)?;
    assert_eq!(result["provider_completed"], false);
    assert_eq!(result["worker_usage"]["status"], "available");
    assert!(!options.output_dir.join("review.md").exists());
    Ok(())
}

#[test]
fn feature_catalog_is_read_only_and_needs_no_opt_in() -> TestResult {
    let mut out = Vec::new();
    let mut err = Vec::new();
    assert_eq!(main_with(["features"], None, &mut out, &mut err), 0);
    let catalog: Value = serde_json::from_slice(&out)?;
    assert_eq!(catalog["scope"], "rust-pilot-worker");
    assert_eq!(catalog["features"].as_array().ok_or("features")?.len(), 8);
    assert!(err.is_empty());
    assert_eq!(
        main_with(
            ["features", "--feature", "kb=on"],
            None,
            &mut Vec::new(),
            &mut err
        ),
        2
    );
    Ok(())
}

#[test]
fn feature_requests_are_refused_before_output_or_provider_execution() -> TestResult {
    let scratch = Scratch::new()?;
    let marker = scratch.0.join("provider-invoked");
    let provider = scratch.fake_claude("2.1.269", &format!("touch '{}'", marker.display()))?;
    let prompt = scratch.file("feature-prompt", "review")?;
    let output = scratch.0.join("feature-output");
    for request in ["kb=on", "unknown=off", "kb=maybe", "kb=off=on"] {
        let args = vec![
            "review".to_owned(),
            "--prompt-file".into(),
            prompt.display().to_string(),
            "--output-dir".into(),
            output.display().to_string(),
            "--claude".into(),
            provider.display().to_string(),
            "--feature".into(),
            request.into(),
        ];
        assert_eq!(
            main_with(args, Some("1".into()), &mut Vec::new(), &mut Vec::new()),
            2
        );
        assert!(!output.exists());
        assert!(!marker.exists());
    }
    for command in ["resume", "status", "cancel", "observe", "view"] {
        assert!(parse_arguments([command, "--feature", "kb=off"]).is_err());
    }
    assert!(parse_arguments(["review", "--feature", "kb=off", "--feature", "kb=off"]).is_err());
    Ok(())
}

#[test]
fn feature_receipt_survives_provider_version_refusal() -> TestResult {
    let scratch = Scratch::new()?;
    let provider = scratch.fake_claude("0.0.0", "exit 99")?;
    let mut options = scratch.options(&provider, 10)?;
    options.feature_requests.request("portal-output-cap=off")?;
    let summary = run_review(&options, CancellationToken::new())?;
    assert!(!summary.completed);
    let receipt: Value =
        serde_json::from_slice(&fs::read(options.output_dir.join("run-features.json"))?)?;
    assert_eq!(receipt["runtime"], "claude");
    assert_eq!(receipt["command"], "review");
    assert_eq!(receipt["entries"][3]["requested"], "off");
    assert_eq!(receipt["entries"][3]["source"], "explicit-run-option");
    assert!(receipt["entries"][0]["requested"].is_null());
    assert!(receipt["entries"][3]["applied"].is_null());
    Ok(())
}

#[test]
fn phase_usage_persistence_failure_is_unavailable_without_aggregates() -> TestResult {
    let scratch = Scratch::new()?;
    let not_directory = scratch.file("not-directory", "original")?;
    let observation = Observation {
        summary: json!({"stdout_discarded_bytes":0}),
        stdout: include_bytes!("codex-success.jsonl").to_vec(),
        stderr: Vec::new(),
    };
    let usage =
        record_worker_usage_for_phase(&not_directory, "codex", Some(&observation), "phase-9");
    assert_eq!(usage["status"], "unavailable");
    assert_eq!(usage["phase_id"], "phase-9");
    assert!(usage.get("summary").is_none());
    assert!(usage.get("provider_turn_count").is_none());
    assert!(usage.get("report_artifact").is_none());
    assert_eq!(fs::read_to_string(not_directory)?, "original");
    Ok(())
}

#[test]
fn phase_usage_with_missing_observation_stays_unknown() -> TestResult {
    let scratch = Scratch::new()?;
    let usage = record_worker_usage_for_phase(&scratch.0, "codex", None, "phase-7");
    assert_eq!(usage["status"], "unavailable");
    assert_eq!(usage["phase_id"], "phase-7");
    assert!(usage.get("provider_turn_count").is_none());
    let saved: Value = serde_json::from_slice(&fs::read(scratch.0.join("worker-usage.json"))?)?;
    assert_eq!(saved["status"], "unavailable");
    assert!(fs::read(scratch.0.join("worker-usage-events.jsonl"))?.is_empty());
    Ok(())
}
