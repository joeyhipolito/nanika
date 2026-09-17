use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use orchestrator_process::CancellationToken;
use serde_json::Value;

use super::*;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const PASS: &str =
    r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#;
const REJECT: &str = r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"reject","blockers":["broken"],"warnings":[]}"#;
const CODE_WIRE: &str = "{\"type\":\"thread.started\",\"thread_id\":\"code\"}\n{\"type\":\"turn.started\"}\n{\"type\":\"item.completed\",\"item\":{\"id\":\"answer\",\"type\":\"agent_message\",\"text\":\"coding phase complete\"}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"cache_write_input_tokens\":0,\"output_tokens\":2,\"reasoning_output_tokens\":0}}";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> TestResult<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = fs::canonicalize(std::env::temp_dir())?.join(format!(
            "nanika-authored-cycle-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        Ok(Self(root))
    }

    fn file(&self, name: &str, contents: &str) -> TestResult<PathBuf> {
        let path = self.0.join(name);
        fs::write(&path, contents)?;
        Ok(path)
    }

    fn repo(&self) -> TestResult<PathBuf> {
        let repo = self.0.join("repo");
        fs::create_dir(&repo)?;
        git(&repo, &["init", "-q"])?;
        git(&repo, &["config", "user.name", "Pilot Fixture"])?;
        git(&repo, &["config", "user.email", "pilot@example.invalid"])?;
        fs::write(repo.join("base.txt"), "base\n")?;
        git(&repo, &["add", "base.txt"])?;
        git(&repo, &["commit", "-q", "-m", "fixture"])?;
        Ok(repo)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(repo: &Path, arguments: &[&str]) -> TestResult {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(arguments)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned().into())
    }
}

fn emit(text: &str) -> String {
    format!("cat <<'EOF'\n{text}\nEOF")
}

fn review_wire(answer: &str) -> TestResult<String> {
    Ok(format!(
        "{{\"type\":\"thread.started\",\"thread_id\":\"review\"}}\n{{\"type\":\"turn.started\"}}\n{{\"type\":\"item.completed\",\"item\":{{\"id\":\"answer\",\"type\":\"agent_message\",\"text\":{}}}}}\n{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":10,\"cached_input_tokens\":0,\"cache_write_input_tokens\":0,\"output_tokens\":2,\"reasoning_output_tokens\":0}}}}",
        serde_json::to_string(answer)?
    ))
}

fn options(
    scratch: &Scratch,
    mission: &str,
    verdict: &str,
    verifier: &str,
) -> TestResult<PilotOptions> {
    let repo = scratch.repo()?;
    let review_wire = review_wire(verdict)?;
    let executable = scratch.file(
        "fake-codex",
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.154.0'; exit 0; fi\ninput=$(cat)\ncase \"$input\" in\n*\"Review the implementation below\"*) test -f phase-one.txt && test -f phase-two.txt || exit 92; {}\n;;\n*\"Add the first change\"*) printf 'one\\n' > phase-one.txt; {}\n;;\n*\"Add the second change\"*) test -f phase-one.txt || exit 91; printf 'two\\n' > phase-two.txt; {}\n;;\n*) exit 93;;\nesac\n",
            emit(&review_wire),
            emit(CODE_WIRE),
            emit(CODE_WIRE),
        ),
    )?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    let verifier = scratch.file("verifier", &format!("#!/bin/sh\n{verifier}\n"))?;
    fs::set_permissions(&verifier, fs::Permissions::from_mode(0o755))?;
    Ok(PilotOptions {
        command: PilotCommand::Run,
        prompt_file: scratch.file("mission.md", mission)?,
        output_dir: scratch.0.join("out"),
        repo: Some(repo),
        model: String::new(),
        persona: None,
        timeout: Duration::from_secs(30),
        claude_executable: OsString::from("claude"),
        runtime: "codex".to_owned(),
        codex_executable: executable.into_os_string(),
        verification_argv: vec![verifier.into_os_string()],
        verification_timeout: Duration::from_secs(30),
        authored_mission: true,
        durable: false,
        stop_after_phase: None,
        mission_id: None,
        feature_requests: features::Requests::default(),
        progress_log: None,
        observe_follow: false,
        observe_format: observe::OutputFormat::Text,
    })
}

fn mission() -> &'static str {
    "PHASE: verify | OBJECTIVE: Run the operator checks | PERSONA: operator-verifier | ROLE: verification | DEPENDS: review\n\
PHASE: review | OBJECTIVE: Review both changes | PERSONA: staff-code-reviewer | ROLE: review | DEPENDS: code-two\n\
PHASE: code-two | OBJECTIVE: Add the second change | PERSONA: senior-frontend-engineer | ROLE: code | DEPENDS: code-one\n\
PHASE: code-one | OBJECTIVE: Add the first change | PERSONA: senior-backend-engineer | ROLE: code\n"
}

fn result(options: &PilotOptions) -> TestResult<Value> {
    Ok(serde_json::from_slice(&fs::read(
        options.output_dir.join("pilot-result.json"),
    )?)?)
}

#[test]
fn mission_file_selects_authored_input_and_rejects_global_persona() -> TestResult {
    let parsed = parse_arguments([
        "run",
        "--repo",
        "repo",
        "--mission-file",
        "mission.md",
        "--output-dir",
        "out",
        "--",
        "verify",
    ])?;
    assert!(parsed.authored_mission);
    assert_eq!(parsed.prompt_file, Path::new("mission.md"));
    assert!(
        parse_arguments([
            "run",
            "--repo",
            "repo",
            "--mission-file",
            "mission.md",
            "--output-dir",
            "out",
            "--persona",
            "reviewer",
            "--",
            "verify",
        ])
        .is_err()
    );
    assert!(
        parse_arguments([
            "run",
            "--durable",
            "--repo",
            "repo",
            "--task-file",
            "task.md",
            "--output-dir",
            "out",
            "--",
            "verify",
        ])
        .is_err()
    );
    Ok(())
}

#[test]
fn out_of_order_dependencies_share_one_workspace_and_route_each_persona() -> TestResult {
    let scratch = Scratch::new()?;
    let options = options(
        &scratch,
        mission(),
        PASS,
        "printf '%s\\n' '{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":2,\"executed\":2,\"passed\":2,\"failed\":0,\"required_skipped\":0}' > \"$NANIKA_VERIFICATION_REPORT\"",
    )?;
    let summary = authored_cycle::run(&options, CancellationToken::new())?;
    assert!(
        summary.completed,
        "{}: {}",
        summary.reason,
        format_args!(
            "{}\n{}",
            fs::read_to_string(&summary.result_path)?,
            fs::read_to_string(
                options
                    .output_dir
                    .join("phases/04-code-one/codex-stdout.jsonl")
            )?
        )
    );
    let plan: Value = serde_json::from_slice(&fs::read(options.output_dir.join("plan.json"))?)?;
    assert_eq!(
        plan["execution_order"],
        serde_json::json!(["code-one", "code-two", "review", "verify"])
    );
    assert!(options.output_dir.join("workspace/phase-one.txt").is_file());
    assert!(options.output_dir.join("workspace/phase-two.txt").is_file());
    let record = result(&options)?;
    assert_eq!(record["status"], "completed");
    assert_eq!(
        record["phases"][0]["route"]["persona"],
        "senior-backend-engineer"
    );
    assert_eq!(
        record["phases"][1]["route"]["persona"],
        "senior-frontend-engineer"
    );
    assert_eq!(
        record["phases"][2]["route"]["persona"],
        "staff-code-reviewer"
    );
    let prompt = fs::read_to_string(options.output_dir.join("phases/02-review/prompt.md"))?;
    assert!(prompt.contains("Add the first change"));
    assert!(prompt.contains("Add the second change"));
    assert!(prompt.contains("phase-one.txt"));
    assert!(prompt.contains("phase-two.txt"));
    Ok(())
}

#[test]
fn rejected_review_skips_verification() -> TestResult {
    let scratch = Scratch::new()?;
    let options = options(&scratch, mission(), REJECT, "touch verifier-ran; exit 99")?;
    let summary = authored_cycle::run(&options, CancellationToken::new())?;
    assert!(!summary.completed);
    assert!(!options.output_dir.join("workspace/verifier-ran").exists());
    let record = result(&options)?;
    assert_eq!(record["status"], "failed");
    assert_eq!(record["phases"][2]["status"], "failed");
    assert_eq!(record["phases"][3]["status"], "skipped");
    assert_eq!(record["tests_verified"], false);
    Ok(())
}

#[test]
fn verification_failure_remains_terminal_failure() -> TestResult {
    let scratch = Scratch::new()?;
    let options = options(
        &scratch,
        mission(),
        PASS,
        "printf '%s\\n' '{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":1,\"executed\":1,\"passed\":0,\"failed\":1,\"required_skipped\":0}' > \"$NANIKA_VERIFICATION_REPORT\"; exit 7",
    )?;
    let summary = authored_cycle::run(&options, CancellationToken::new())?;
    assert!(!summary.completed);
    let record = result(&options)?;
    assert_eq!(record["status"], "failed");
    assert_eq!(record["phases"][3]["status"], "failed");
    assert_eq!(record["tests_verified"], false);
    Ok(())
}

#[test]
fn invalid_graph_and_unsupported_authored_data_refuse_before_dispatch() -> TestResult {
    let invalid = [
        "PHASE: code | OBJECTIVE: one | PERSONA: engineer | ROLE: code\nPHASE: code | OBJECTIVE: two | PERSONA: engineer | ROLE: code\nPHASE: review | OBJECTIVE: review | PERSONA: reviewer | ROLE: review | DEPENDS: code\nPHASE: verify | OBJECTIVE: verify | PERSONA: verifier | ROLE: verification | DEPENDS: review",
        "PHASE: code | OBJECTIVE: code | PERSONA: engineer | ROLE: code | DEPENDS: missing\nPHASE: review | OBJECTIVE: review | PERSONA: reviewer | ROLE: review | DEPENDS: code\nPHASE: verify | OBJECTIVE: verify | PERSONA: verifier | ROLE: verification | DEPENDS: review",
        "PHASE: code | OBJECTIVE: code | PERSONA: engineer | ROLE: code | DEPENDS: code\nPHASE: review | OBJECTIVE: review | PERSONA: reviewer | ROLE: review | DEPENDS: code\nPHASE: verify | OBJECTIVE: verify | PERSONA: verifier | ROLE: verification | DEPENDS: review",
        "PHASE: code | OBJECTIVE: code | PERSONA: engineer | ROLE: code | DEPENDS: review\nPHASE: review | OBJECTIVE: review | PERSONA: reviewer | ROLE: review | DEPENDS: code\nPHASE: verify | OBJECTIVE: verify | PERSONA: verifier | ROLE: verification | DEPENDS: review",
        "PHASE: code | OBJECTIVE: code | PERSONA: engineer | ROLE: code\nPHASE: review | OBJECTIVE: review | PERSONA: reviewer | ROLE: review | DEPENDS: code\nPHASE: verify | OBJECTIVE: verify | PERSONA: verifier | ROLE: verification | DEPENDS: review\nPHASE: late | OBJECTIVE: unchecked | PERSONA: engineer | ROLE: code | DEPENDS: verify",
        "PHASE: code-one | OBJECTIVE: covered | PERSONA: engineer | ROLE: code\nPHASE: review | OBJECTIVE: review | PERSONA: reviewer | ROLE: review | DEPENDS: code-one\nPHASE: verify | OBJECTIVE: verify | PERSONA: verifier | ROLE: verification | DEPENDS: review\nPHASE: code-two | OBJECTIVE: disconnected | PERSONA: engineer | ROLE: code",
        "PHASE: code | OBJECTIVE: code | PERSONA: engineer | ROLE: code\nPHASE: review-one | OBJECTIVE: reviewed | PERSONA: reviewer | ROLE: review | DEPENDS: code\nPHASE: verify | OBJECTIVE: verify | PERSONA: verifier | ROLE: verification | DEPENDS: review-one\nPHASE: review-two | OBJECTIVE: unverified review | PERSONA: reviewer | ROLE: review | DEPENDS: code",
        "PHASE: code | OBJECTIVE: code | PERSONA: engineer | ROLE: planner\nPHASE: review | OBJECTIVE: review | PERSONA: reviewer | ROLE: review | DEPENDS: code\nPHASE: verify | OBJECTIVE: verify | PERSONA: verifier | ROLE: verification | DEPENDS: review",
        "PHASE: code | OBJECTIVE: code | PERSONA: engineer | ROLE: code | SKILLS: rust\nPHASE: review | OBJECTIVE: review | PERSONA: reviewer | ROLE: review | DEPENDS: code\nPHASE: verify | OBJECTIVE: verify | PERSONA: verifier | ROLE: verification | DEPENDS: review",
    ];
    for source in invalid {
        assert!(
            authored_cycle::validate_mission(source).is_err(),
            "{source}"
        );
    }

    let scratch = Scratch::new()?;
    let options = options(&scratch, invalid[3], PASS, "exit 99")?;
    let marker = scratch.0.join("provider-dispatched");
    let executable = PathBuf::from(&options.codex_executable);
    fs::write(
        &executable,
        format!("#!/bin/sh\ntouch '{}'\nexit 99\n", marker.display()),
    )?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    assert!(authored_cycle::run(&options, CancellationToken::new()).is_err());
    assert!(!marker.exists());
    assert!(!options.output_dir.exists());
    Ok(())
}

#[test]
fn incomplete_review_process_does_not_pass() -> TestResult {
    let scratch = Scratch::new()?;
    let options = options(&scratch, mission(), PASS, "exit 99")?;
    let review = review_wire(PASS)?;
    let executable = PathBuf::from(&options.codex_executable);
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.154.0'; exit 0; fi\nif [ ! -f phase-one.txt ]; then printf 'one\\n' > phase-one.txt; {}\nelif [ ! -f phase-two.txt ]; then printf 'two\\n' > phase-two.txt; {}\nelse {}\nexit 7\nfi\n",
            emit(CODE_WIRE),
            emit(CODE_WIRE),
            emit(&review),
        ),
    )?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;

    let summary = authored_cycle::run(&options, CancellationToken::new())?;
    assert!(!summary.completed);
    let record = result(&options)?;
    assert_eq!(record["phases"][2]["status"], "failed", "{record}");
    assert_eq!(record["phases"][2]["verdict_valid"], false);
    assert_eq!(record["phases"][2]["provider_dispatched"], true);
    assert_eq!(record["phases"][3]["status"], "skipped");
    assert!(
        !options
            .output_dir
            .join("phases/02-review/answer.md")
            .exists()
    );
    assert!(
        !options
            .output_dir
            .join("phases/02-review/verdict.json")
            .exists()
    );
    Ok(())
}

#[test]
fn post_dispatch_failure_preserves_route_process_and_observation() -> TestResult {
    let scratch = Scratch::new()?;
    let options = options(&scratch, mission(), PASS, "exit 99")?;
    let executable = PathBuf::from(&options.codex_executable);
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.154.0'; exit 0; fi\nprintf 'one\\n' > phase-one.txt\nmv \"$PWD\" \"${{PWD}}-moved\"\n{}\n",
            emit(CODE_WIRE),
        ),
    )?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;

    let summary = authored_cycle::run(&options, CancellationToken::new())?;
    assert!(!summary.completed);
    let record = result(&options)?;
    let failed = &record["phases"][0];
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["provider_dispatched"], true);
    assert_eq!(failed["route"]["persona"], "senior-backend-engineer");
    assert!(failed["process"].is_object());
    assert!(
        failed["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("unavailable"))
    );
    assert!(
        options
            .output_dir
            .join("phases/04-code-one/codex-stdout.jsonl")
            .is_file()
    );
    Ok(())
}

#[test]
fn authored_verification_rejects_workspace_loss_and_reviewed_source_change() -> TestResult {
    for mode in ["missing", "changed"] {
        let scratch = Scratch::new()?;
        let body = match mode {
            "missing" => "printf '%s\\n' '{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":1,\"executed\":1,\"passed\":1,\"failed\":0,\"required_skipped\":0}' > \"$NANIKA_VERIFICATION_REPORT\"; mv \"$PWD\" \"${PWD}-moved\"".to_owned(),
            "changed" => "printf '%s\\n' '{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":1,\"executed\":1,\"passed\":1,\"failed\":0,\"required_skipped\":0}' > \"$NANIKA_VERIFICATION_REPORT\"; printf 'changed\\n' > base.txt".to_owned(),
            _ => unreachable!(),
        };
        let options = options(&scratch, mission(), PASS, &body)?;
        let summary = authored_cycle::run(&options, CancellationToken::new())?;
        assert!(!summary.completed, "{mode}");
        let record = result(&options)?;
        assert_eq!(record["phases"][3]["status"], "failed", "{mode}");
        assert_eq!(
            record["phases"][3]["reviewed_workspace_preserved"], false,
            "{mode}"
        );
        assert_eq!(record["tests_verified"], false, "{mode}");
    }
    Ok(())
}

#[test]
fn authored_verification_reports_ignored_build_outputs() -> TestResult {
    let scratch = Scratch::new()?;
    let options = options(
        &scratch,
        mission(),
        PASS,
        "mkdir -p target/debug; printf 'object\\n' > target/debug/build.o; printf '%s\\n' '{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":1,\"executed\":1,\"passed\":1,\"failed\":0,\"required_skipped\":0}' > \"$NANIKA_VERIFICATION_REPORT\"",
    )?;
    let repo = options.repo.as_ref().ok_or("repo")?;
    fs::write(repo.join(".gitignore"), "/target/\n")?;
    git(repo, &["add", ".gitignore"])?;
    git(repo, &["commit", "-q", "-m", "ignore build output"])?;

    let summary = authored_cycle::run(&options, CancellationToken::new())?;
    assert!(summary.completed, "{}", summary.reason);
    let record = result(&options)?;
    assert_eq!(record["phases"][3]["reviewed_workspace_preserved"], true);
    assert_eq!(record["phases"][3]["generated_outputs"]["count"], 3);
    assert!(
        options
            .output_dir
            .join("phases/01-verify/generated-outputs.json")
            .is_file()
    );
    Ok(())
}
