use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

#[derive(Deserialize, Serialize)]
struct SavedEnvelope {
    previous_digest: String,
    digest: String,
    record: SavedRecord,
}

#[derive(Deserialize, Serialize)]
struct SavedRecord {
    schema: String,
    mission_id: String,
    sequence: i64,
    event_id: String,
    timestamp: String,
    kind: String,
    phase_id: Option<String>,
    role: Option<String>,
    status: Option<String>,
    reason: Option<String>,
    result: Option<Value>,
    baseline_digest: Option<String>,
    workspace_digest: Option<String>,
}

const MISSION: &str = "PHASE: code-one | OBJECTIVE: Add the first change | PERSONA: engineer | ROLE: code\n\
PHASE: code-two | OBJECTIVE: Add the second change | PERSONA: engineer | ROLE: code | DEPENDS: code-one\n\
PHASE: review | OBJECTIVE: Review both changes | PERSONA: reviewer | ROLE: review | DEPENDS: code-two\n\
PHASE: verify | OBJECTIVE: Verify both changes | PERSONA: operator-verifier | ROLE: verification | DEPENDS: review\n";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> TestResult<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = fs::canonicalize(std::env::temp_dir())?.join(format!(
            "nanika-durable-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        fs::create_dir(root.join("home"))?;
        Ok(Self(root))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn file(&self, name: &str, contents: &str, executable: bool) -> TestResult<PathBuf> {
        let path = self.path(name);
        fs::write(&path, contents)?;
        if executable {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(path)
    }

    fn repo(&self) -> TestResult<PathBuf> {
        let repo = self.path("repo");
        self.create_repo(repo)
    }

    fn nested_repo(&self, parent: &str) -> TestResult<PathBuf> {
        let parent = self.path(parent);
        fs::create_dir(&parent)?;
        self.create_repo(parent.join("repo"))
    }

    fn create_repo(&self, repo: PathBuf) -> TestResult<PathBuf> {
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

fn git(repo: &Path, args: &[&str]) -> TestResult {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned().into())
    }
}

fn fixture(
    scratch: &Scratch,
    verdict: &str,
    slow_provider_pattern: Option<&str>,
    slow_verifier: bool,
) -> TestResult<(PathBuf, PathBuf, PathBuf)> {
    let provider_count = scratch.path("provider-count");
    let provider_ready = scratch.path("provider-ready");
    let slow_case = slow_provider_pattern.map_or_else(String::new, |pattern| {
        format!(
            "*\"{pattern}\"*) printf '%s\\n' provider >> '{}'; : > '{}'; sleep 1; printf 'slow\\n' > slow.txt; printf 'two\\n' > phase-two.txt; emit_code;;",
            provider_count.display(),
            provider_ready.display()
        )
    });
    let review_json = serde_json::to_string(verdict)?;
    let provider = scratch.file(
        "fake-codex",
        &format!(
            "#!/bin/sh\n\
if [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.154.0'; exit 0; fi\n\
input=$(cat)\n\
emit_code() {{ cat <<'EOF'\n\
{{\"type\":\"thread.started\",\"thread_id\":\"code\"}}\n\
{{\"type\":\"turn.started\"}}\n\
{{\"type\":\"item.completed\",\"item\":{{\"id\":\"answer\",\"type\":\"agent_message\",\"text\":\"done\"}}}}\n\
{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"cache_write_input_tokens\":0,\"output_tokens\":1,\"reasoning_output_tokens\":0}}}}\n\
EOF\n\
}}\n\
emit_review() {{ cat <<EOF\n\
{{\"type\":\"thread.started\",\"thread_id\":\"review\"}}\n\
{{\"type\":\"turn.started\"}}\n\
{{\"type\":\"item.completed\",\"item\":{{\"id\":\"answer\",\"type\":\"agent_message\",\"text\":{review_json}}}}}\n\
{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"cache_write_input_tokens\":0,\"output_tokens\":1,\"reasoning_output_tokens\":0}}}}\n\
EOF\n\
}}\n\
case \"$input\" in\n\
*\"Review the implementation below\"*) test -f phase-two.txt || exit 92; printf '%s\\n' review >> '{}'; emit_review;;\n\
{slow_case}\n\
*\"Add the first change\"*) printf '%s\\n' code-one >> '{}'; printf 'one\\n' > phase-one.txt; emit_code;;\n\
*\"Add the second change\"*) test -f phase-one.txt || exit 91; printf '%s\\n' code-two >> '{}'; printf 'two\\n' > phase-two.txt; emit_code;;\n\
*) exit 93;;\n\
esac\n",
            provider_count.display(),
            provider_count.display(),
            provider_count.display(),
        ),
        true,
    )?;
    let verifier_count = scratch.path("verifier-count");
    let verifier_ready = scratch.path("verifier-ready");
    let delay = if slow_verifier {
        format!(": > '{}'; sleep 1\n", verifier_ready.display())
    } else {
        String::new()
    };
    let verifier = scratch.file(
        "verifier",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' verify >> '{}'\n{delay}printf '%s\\n' '{{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":2,\"executed\":2,\"passed\":2,\"failed\":0,\"required_skipped\":0}}' > \"$NANIKA_VERIFICATION_REPORT\"\n",
            verifier_count.display()
        ),
        true,
    )?;
    Ok((provider, verifier, provider_ready))
}

fn pilot(scratch: &Scratch) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orchestrator-first-use-pilot"));
    command
        .env("NANIKA_RUST_FIRST_USE_PILOT", "1")
        .env("HOME", scratch.path("home"))
        .env("USER", "pilot")
        .env("LOGNAME", "pilot")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn run_args(
    command: &mut Command,
    repo: &Path,
    mission: &Path,
    output: &Path,
    provider: &Path,
    verifier: &Path,
) {
    command.args(["run", "--durable"]);
    run_args_tail(command, repo, mission, output, provider, verifier);
}

fn run_args_tail(
    command: &mut Command,
    repo: &Path,
    mission: &Path,
    output: &Path,
    provider: &Path,
    verifier: &Path,
) {
    command.args([
        "--repo",
        repo.to_str().unwrap_or_default(),
        "--mission-file",
        mission.to_str().unwrap_or_default(),
        "--output-dir",
        output.to_str().unwrap_or_default(),
        "--codex",
        provider.to_str().unwrap_or_default(),
        "--timeout-secs",
        "20",
        "--verification-timeout-secs",
        "20",
        "--",
        verifier.to_str().unwrap_or_default(),
    ]);
}

fn run_args_with_timeouts(
    command: &mut Command,
    repo: &Path,
    mission: &Path,
    output: &Path,
    provider: &Path,
    verifier: &Path,
    timeouts: (&str, &str),
) {
    let (provider_timeout, verifier_timeout) = timeouts;
    command.args([
        "run",
        "--durable",
        "--repo",
        repo.to_str().unwrap_or_default(),
        "--mission-file",
        mission.to_str().unwrap_or_default(),
        "--output-dir",
        output.to_str().unwrap_or_default(),
        "--codex",
        provider.to_str().unwrap_or_default(),
        "--timeout-secs",
        provider_timeout,
        "--verification-timeout-secs",
        verifier_timeout,
        "--",
        verifier.to_str().unwrap_or_default(),
    ]);
}

fn resume(scratch: &Scratch, output: &Path) -> TestResult<Output> {
    Ok(pilot(scratch)
        .args([
            "resume",
            "--output-dir",
            output.to_str().ok_or("output path")?,
        ])
        .output()?)
}

fn status(scratch: &Scratch, output: &Path) -> TestResult<Output> {
    Ok(pilot(scratch)
        .args([
            "status",
            "--output-dir",
            output.to_str().ok_or("output path")?,
        ])
        .output()?)
}

fn status_report(output: &Output) -> TestResult<Value> {
    if !output.status.success() {
        return Err(format!(
            "status refused: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn journal_records(output: &Path) -> TestResult<Vec<SavedEnvelope>> {
    let mut paths = fs::read_dir(output.join("journal"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    paths
        .into_iter()
        .map(|path| -> TestResult<SavedEnvelope> { Ok(serde_json::from_slice(&fs::read(path)?)?) })
        .collect()
}

fn append_journal_record(output: &Path, mut record: SavedRecord) -> TestResult {
    let records = journal_records(output)?;
    let previous = records.last().ok_or("journal is empty")?;
    record.sequence = previous.record.sequence + 1;
    record.event_id = format!("durable-event-{:06}", record.sequence);
    let record_bytes = serde_json::to_vec(&record)?;
    let mut digest = Sha256::new();
    digest.update(previous.digest.as_bytes());
    digest.update([0]);
    digest.update(record_bytes);
    let envelope = SavedEnvelope {
        previous_digest: previous.digest.clone(),
        digest: format!("{:x}", digest.finalize()),
        record,
    };
    let mut bytes = serde_json::to_vec_pretty(&envelope)?;
    bytes.push(b'\n');
    let path = output
        .join("journal")
        .join(format!("{:06}.json", envelope.record.sequence));
    fs::write(&path, bytes)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn mission_id(output: &Path) -> TestResult<String> {
    let manifest: Value = serde_json::from_slice(&fs::read(output.join("manifest.json"))?)?;
    manifest["mission_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "manifest mission id is missing".into())
}

fn cancel(scratch: &Scratch, output: &Path, mission: &str) -> TestResult<Output> {
    Ok(pilot(scratch)
        .args([
            "cancel",
            "--output-dir",
            output.to_str().ok_or("output path")?,
            "--mission",
            mission,
        ])
        .output()?)
}

fn lines(path: &Path) -> TestResult<Vec<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text.lines().map(str::to_owned).collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn latest_result(output: &Output) -> TestResult<Value> {
    let stdout = String::from_utf8(output.stdout.clone())?;
    let path = stdout
        .split_whitespace()
        .find_map(|part| part.strip_prefix("result="))
        .ok_or("missing result path")?;
    let envelope: Value = serde_json::from_slice(&fs::read(path)?)?;
    Ok(envelope["record"]["result"].clone())
}

fn wait_for(path: &Path) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {}", path.display()).into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn kill_and_wait(mut child: Child) -> TestResult {
    child.kill()?;
    let _ = child.wait()?;
    thread::sleep(Duration::from_millis(1200));
    Ok(())
}

fn bounded_output(command: &mut Command, timeout: Duration) -> TestResult<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            return Err(format!(
                "pilot did not exit within {timeout:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn paused_durable_run(scratch: &Scratch) -> TestResult<PathBuf> {
    let repo = scratch.repo()?;
    paused_durable_run_with_repo(scratch, &repo)
}

fn paused_durable_run_with_repo(scratch: &Scratch, repo: &Path) -> TestResult<PathBuf> {
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let output = scratch.path("out");
    let mut command = pilot(scratch);
    command.args(["run", "--durable", "--stop-after-phase", "code-one"]);
    run_args_tail(&mut command, repo, &mission, &output, &provider, &verifier);
    let paused = command.output()?;
    if paused.status.code() != Some(3) {
        return Err(format!(
            "fixture did not pause: {}",
            String::from_utf8_lossy(&paused.stderr)
        )
        .into());
    }
    Ok(output)
}

fn replace_with_private_file(path: &Path, bytes: &[u8]) -> TestResult {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::write(path, bytes)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn rewrite_json(path: &Path, edit: impl FnOnce(&mut Value) -> TestResult) -> TestResult {
    let mut value: Value = serde_json::from_slice(&fs::read(path)?)?;
    edit(&mut value)?;
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    replace_with_private_file(path, &bytes)
}

fn regular_file_snapshot(root: &Path) -> TestResult<Vec<(PathBuf, Vec<u8>, SystemTime)>> {
    fn visit(
        root: &Path,
        directory: &Path,
        snapshot: &mut Vec<(PathBuf, Vec<u8>, SystemTime)>,
    ) -> TestResult {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                visit(root, &entry.path(), snapshot)?;
            } else if metadata.is_file() {
                snapshot.push((
                    entry.path().strip_prefix(root)?.to_path_buf(),
                    fs::read(entry.path())?,
                    metadata.modified()?,
                ));
            }
        }
        Ok(())
    }

    let mut snapshot = Vec::new();
    visit(root, root, &mut snapshot)?;
    snapshot.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(snapshot)
}

fn last_record_path(output: &Path) -> TestResult<PathBuf> {
    let mut names = fs::read_dir(output.join("journal"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    names.sort();
    names.pop().ok_or_else(|| "missing journal record".into())
}

fn replace_journal_directory_with_copy(output: &Path) -> TestResult {
    let journal = output.join("journal");
    let displaced = output.join("displaced-journal");
    fs::rename(&journal, &displaced)?;
    fs::create_dir(&journal)?;
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o700))?;
    for entry in fs::read_dir(&displaced)? {
        let entry = entry?;
        let destination = journal.join(entry.file_name());
        fs::copy(entry.path(), &destination)?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn downgrade_to_original_v1_manifest(output: &Path) -> TestResult {
    let manifest_path = output.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    let object = manifest
        .as_object_mut()
        .ok_or("manifest is not an object")?;
    object.remove("execution_environment");
    manifest["provider"]
        .as_object_mut()
        .ok_or("provider binding is not an object")?
        .remove("executable_id");
    manifest["verifier"]
        .as_object_mut()
        .ok_or("verifier binding is not an object")?
        .remove("executable_id");
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    manifest_bytes.push(b'\n');
    fs::write(&manifest_path, &manifest_bytes)?;

    let mut previous = format!("{:x}", Sha256::digest(&manifest_bytes));
    let mut records = fs::read_dir(output.join("journal"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    records.sort();
    for path in records {
        let mut envelope: SavedEnvelope = serde_json::from_slice(&fs::read(&path)?)?;
        envelope.previous_digest = previous.clone();
        let record_bytes = serde_json::to_vec(&envelope.record)?;
        let mut digest = Sha256::new();
        digest.update(previous.as_bytes());
        digest.update([0]);
        digest.update(record_bytes);
        envelope.digest = format!("{:x}", digest.finalize());
        previous.clone_from(&envelope.digest);
        let mut bytes = serde_json::to_vec_pretty(&envelope)?;
        bytes.push(b'\n');
        fs::write(path, bytes)?;
    }
    Ok(())
}

#[test]
fn durable_manifest_serialization_includes_additive_process_bindings() -> TestResult {
    let scratch = Scratch::new()?;
    let output = paused_durable_run(&scratch)?;
    let manifest: Value = serde_json::from_slice(&fs::read(output.join("manifest.json"))?)?;
    let actual = manifest
        .as_object()
        .ok_or("manifest is not an object")?
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected = BTreeSet::from([
        "schema",
        "mission_id",
        "mission_text",
        "mission_digest",
        "source",
        "plan",
        "routes",
        "provider",
        "verifier",
        "output_root",
        "output_directory",
        "snapshot",
        "execution_environment",
    ]);
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn status_and_resume_accept_a_valid_old_format_saved_run() -> TestResult {
    let scratch = Scratch::new()?;
    let output = paused_durable_run(&scratch)?;
    downgrade_to_original_v1_manifest(&output)?;
    let manifest: Value = serde_json::from_slice(&fs::read(output.join("manifest.json"))?)?;
    assert!(manifest.get("journal_directory").is_none());
    assert!(manifest.get("execution_environment").is_none());
    assert!(manifest["provider"].get("executable_id").is_none());

    let report = status_report(&status(&scratch, &output)?)?;
    assert_eq!(report["recorded_status"], "paused");
    assert_eq!(report["durability"], "not_attested");

    let resumed = resume(&scratch, &output)?;
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    Ok(())
}

#[test]
fn fresh_durable_run_completes_and_releases_its_writer_lease() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    let completed = command.output()?;
    assert!(
        completed.status.success(),
        "{}",
        String::from_utf8_lossy(&completed.stderr)
    );
    let reopened = resume(&scratch, &output)?;
    assert!(
        reopened.status.success(),
        "{}",
        String::from_utf8_lossy(&reopened.stderr)
    );
    assert_eq!(lines(&scratch.path("provider-count"))?.len(), 3);
    assert_eq!(lines(&scratch.path("verifier-count"))?.len(), 1);
    Ok(())
}

#[test]
fn fresh_pause_resume_and_completed_resume_execute_each_effect_once() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    command.args(["run", "--durable", "--stop-after-phase", "code-one"]);
    run_args_tail(&mut command, &repo, &mission, &output, &provider, &verifier);
    let paused = command.output()?;
    assert_eq!(
        paused.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&paused.stderr)
    );
    assert!(output.join("workspace/phase-one.txt").is_file());
    assert!(!output.join("workspace/phase-two.txt").exists());

    let completed = resume(&scratch, &output)?;
    assert!(
        completed.status.success(),
        "{}",
        String::from_utf8_lossy(&completed.stderr)
    );
    assert_eq!(latest_result(&completed)?["status"], "completed");
    assert_eq!(
        lines(&scratch.path("provider-count"))?,
        ["code-one", "code-two", "review"]
    );
    assert_eq!(lines(&scratch.path("verifier-count"))?, ["verify"]);

    let repeated = resume(&scratch, &output)?;
    assert!(
        repeated.status.success(),
        "{}",
        String::from_utf8_lossy(&repeated.stderr)
    );
    assert_eq!(lines(&scratch.path("provider-count"))?.len(), 3);
    assert_eq!(lines(&scratch.path("verifier-count"))?.len(), 1);
    let first_metrics = status_report(&status(&scratch, &output)?)?["metrics"].clone();
    assert_eq!(first_metrics["mission"]["phases_total"], 4);
    assert_eq!(first_metrics["mission"]["phases_completed"], 4);
    assert!(first_metrics["mission"]["cost_usd"].is_null());
    assert_eq!(first_metrics["phases"].as_array().map(Vec::len), Some(4));
    for phase in first_metrics["phases"].as_array().ok_or("metric phases")? {
        assert_eq!(phase["parsed_skills"], serde_json::json!([]));
        assert!(phase["cost_usd"].is_null());
        assert_eq!(phase["target_released"], true);
    }
    let verification = first_metrics["phases"]
        .as_array()
        .and_then(|phases| phases.iter().find(|phase| phase["phase"] == "verify"))
        .ok_or("verification metric")?;
    assert_eq!(verification["gate_passed"], true);
    assert!(verification["tokens"].is_null());

    let repeated = resume(&scratch, &output)?;
    assert!(repeated.status.success());
    let repeated_metrics = status_report(&status(&scratch, &output)?)?["metrics"].clone();
    assert_eq!(repeated_metrics, first_metrics);
    Ok(())
}

#[test]
fn skill_less_phase_metric_is_visible_before_mission_terminal() -> TestResult {
    let scratch = Scratch::new()?;
    let output = paused_durable_run(&scratch)?;
    let report = status_report(&status(&scratch, &output)?)?;
    assert!(report["metrics"]["mission"].is_null());
    let phases = report["metrics"]["phases"]
        .as_array()
        .ok_or("metric phases")?;
    assert_eq!(phases.len(), 1);
    assert_eq!(phases[0]["phase"], "code-one");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["parsed_skills"], serde_json::json!([]));
    assert_eq!(phases[0]["tokens"]["input"], 1);
    assert_eq!(phases[0]["tokens"]["output"], 1);
    assert!(phases[0]["cost_usd"].is_null());
    Ok(())
}

#[test]
fn failed_review_is_terminal_across_resume() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"reject","blockers":["broken"],"warnings":[]}"#,
        None,
        false,
    )?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    let failed = command.output()?;
    assert_eq!(failed.status.code(), Some(1));
    let before = lines(&scratch.path("provider-count"))?;
    let resumed = resume(&scratch, &output)?;
    assert_eq!(resumed.status.code(), Some(1));
    assert_eq!(lines(&scratch.path("provider-count"))?, before);
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    Ok(())
}

#[test]
fn failed_verification_is_terminal_and_is_not_repeated() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    fs::write(
        &verifier,
        format!(
            "#!/bin/sh\nprintf '%s\\n' verify >> '{}'\nprintf '%s\\n' '{{\"schema\":\"nanika.rust-first-use-verification.v1\",\"discovered\":2,\"executed\":2,\"passed\":1,\"failed\":1,\"required_skipped\":0}}' > \"$NANIKA_VERIFICATION_REPORT\"\nexit 7\n",
            scratch.path("verifier-count").display()
        ),
    )?;
    fs::set_permissions(&verifier, fs::Permissions::from_mode(0o700))?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    let failed = command.output()?;
    assert_eq!(failed.status.code(), Some(1));
    assert_eq!(lines(&scratch.path("provider-count"))?.len(), 3);
    assert_eq!(lines(&scratch.path("verifier-count"))?, ["verify"]);

    assert_eq!(resume(&scratch, &output)?.status.code(), Some(1));
    assert_eq!(lines(&scratch.path("verifier-count"))?, ["verify"]);
    let report = status_report(&status(&scratch, &output)?)?;
    let verification = report["metrics"]["phases"]
        .as_array()
        .and_then(|phases| phases.iter().find(|phase| phase["phase"] == "verify"))
        .ok_or("verification metric")?;
    assert_eq!(verification["status"], "failed");
    assert_eq!(verification["gate_passed"], false);
    Ok(())
}

#[test]
fn verification_metric_requires_a_positive_executed_pass() -> TestResult {
    let cases = [
        (
            "positive",
            r#"printf '%s\n' '{"schema":"nanika.rust-first-use-verification.v1","discovered":2,"executed":2,"passed":2,"failed":0,"required_skipped":0}' > "$NANIKA_VERIFICATION_REPORT"
"#,
            true,
            "20",
        ),
        (
            "zero-tests",
            r#"printf '%s\n' '{"schema":"nanika.rust-first-use-verification.v1","discovered":0,"executed":0,"passed":0,"failed":0,"required_skipped":0}' > "$NANIKA_VERIFICATION_REPORT"
"#,
            false,
            "20",
        ),
        (
            "required-skip",
            r#"printf '%s\n' '{"schema":"nanika.rust-first-use-verification.v1","discovered":2,"executed":1,"passed":1,"failed":0,"required_skipped":1}' > "$NANIKA_VERIFICATION_REPORT"
"#,
            false,
            "20",
        ),
        (
            "failure",
            r#"printf '%s\n' '{"schema":"nanika.rust-first-use-verification.v1","discovered":2,"executed":2,"passed":1,"failed":1,"required_skipped":0}' > "$NANIKA_VERIFICATION_REPORT"
exit 7
"#,
            false,
            "20",
        ),
        ("timeout", "sleep 10\n", false, "1"),
    ];
    for (label, body, gate_passed, verifier_timeout) in cases {
        let scratch = Scratch::new()?;
        let repo = scratch.repo()?;
        let mission = scratch.file("mission.md", MISSION, false)?;
        let (provider, verifier, _) = fixture(
            &scratch,
            r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
            None,
            false,
        )?;
        fs::write(
            &verifier,
            format!(
                "#!/bin/sh\nprintf '%s\\n' verify >> '{}'\n{body}",
                scratch.path("verifier-count").display()
            ),
        )?;
        fs::set_permissions(&verifier, fs::Permissions::from_mode(0o700))?;
        let output = scratch.path("out");
        let mut command = pilot(&scratch);
        run_args_with_timeouts(
            &mut command,
            &repo,
            &mission,
            &output,
            &provider,
            &verifier,
            ("20", verifier_timeout),
        );
        let completed = command.output()?;
        assert_eq!(
            completed.status.success(),
            gate_passed,
            "{label}: {}",
            String::from_utf8_lossy(&completed.stderr)
        );
        let report = status_report(&status(&scratch, &output)?)?;
        let metric = report["metrics"]["phases"]
            .as_array()
            .and_then(|phases| phases.iter().find(|phase| phase["phase"] == "verify"))
            .ok_or("verification metric")?;
        assert_eq!(metric["phase"], "verify", "{label}");
        assert_eq!(metric["gate_passed"], gate_passed, "{label}");
        assert_eq!(metric["target_released"], true, "{label}");
        assert!(metric["tokens"].is_null(), "{label}");
        assert!(metric["cost_usd"].is_null(), "{label}");
    }
    Ok(())
}

#[test]
fn provider_deadline_cleans_the_exact_group_and_does_not_repeat() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let ready = scratch.path("deadline-ready");
    fs::write(
        &provider,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.154.0'; exit 0; fi\ncat >/dev/null\nprintf '%s\\n' deadline >> '{}'\n: > '{}'\nsleep 10\n",
            scratch.path("provider-count").display(),
            ready.display()
        ),
    )?;
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700))?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args_with_timeouts(
        &mut command,
        &repo,
        &mission,
        &output,
        &provider,
        &verifier,
        ("1", "20"),
    );
    let failed = command.output()?;
    assert_eq!(failed.status.code(), Some(1));
    let result = latest_result(&failed)?;
    assert_eq!(result["phases"][0]["process"]["cleanup_complete"], true);
    assert!(result["phases"][0]["process"]["group_absence"].is_object());
    assert_eq!(lines(&scratch.path("provider-count"))?, ["deadline"]);
    assert_eq!(resume(&scratch, &output)?.status.code(), Some(1));
    assert_eq!(lines(&scratch.path("provider-count"))?, ["deadline"]);
    Ok(())
}

#[test]
fn provider_signal_cancellation_cleans_the_exact_group_and_does_not_repeat() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, ready) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        Some("Add the first change"),
        false,
    )?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = command.spawn()?;
    wait_for(&ready)?;
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id())?).ok_or("invalid pid")?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
    let failed = child.wait_with_output()?;
    assert_eq!(failed.status.code(), Some(1));
    let result = latest_result(&failed)?;
    assert_eq!(result["phases"][0]["process"]["cleanup_complete"], true);
    assert!(result["phases"][0]["process"]["group_absence"].is_object());
    assert_eq!(lines(&scratch.path("provider-count"))?, ["provider"]);
    assert!(
        journal_records(&output)?
            .iter()
            .all(|record| record.record.kind != "cancellation_requested")
    );
    let report = status_report(&status(&scratch, &output)?)?;
    assert_eq!(report["recorded_status"], "failed");
    assert_eq!(report["metrics"]["mission"]["status"], "failed");
    assert_eq!(resume(&scratch, &output)?.status.code(), Some(1));
    assert_eq!(lines(&scratch.path("provider-count"))?, ["provider"]);
    Ok(())
}

#[test]
fn authenticated_operator_cancellation_is_durable_idempotent_and_cleans_before_exit() -> TestResult
{
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, ready) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        Some("Add the first change"),
        false,
    )?;
    let provider_script = fs::read_to_string(&provider)?;
    fs::write(
        &provider,
        provider_script.replace("sleep 1;", "trap '' TERM; sleep 10;"),
    )?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = command.spawn()?;
    wait_for(&ready)?;

    let exact_mission = mission_id(&output)?;
    let foreign = cancel(&scratch, &output, "foreign-mission")?;
    assert_eq!(foreign.status.code(), Some(1));
    let first = cancel(&scratch, &output, &exact_mission)?;
    assert_eq!(first.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&first.stdout).contains("cancellation-requested"));
    let repeated = cancel(&scratch, &output, &exact_mission)?;
    assert_eq!(repeated.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&repeated.stdout).contains("cancellation-already-requested"));

    let stopped = child.wait_with_output()?;
    assert_eq!(stopped.status.code(), Some(1));
    let result = latest_result(&stopped)?;
    assert_eq!(result["status"], "cancelled");
    assert_eq!(result["completed"], false);
    assert_eq!(result["provider_completed"], false);
    assert_eq!(result["tests_verified"], false);
    assert_eq!(result["phases"][0]["process"]["cleanup_complete"], true);
    assert!(result["phases"][0]["process"]["group_absence"].is_object());
    let records = journal_records(&output)?;
    let request = records
        .iter()
        .position(|record| record.record.kind == "cancellation_requested")
        .ok_or("cancellation record")?;
    assert_eq!(
        records
            .iter()
            .filter(|record| record.record.kind == "cancellation_requested")
            .count(),
        1
    );
    let terminal = records
        .iter()
        .position(|record| record.record.kind == "phase_terminal")
        .ok_or("phase terminal")?;
    let mission_terminal = records
        .iter()
        .position(|record| record.record.kind == "mission_terminal")
        .ok_or("mission terminal")?;
    assert!(request < terminal && terminal < mission_terminal);
    assert_eq!(
        records[mission_terminal].record.status.as_deref(),
        Some("cancelled")
    );
    let report = status_report(&status(&scratch, &output)?)?;
    assert_eq!(report["recorded_status"], "cancelled");
    assert_eq!(report["metrics"]["mission"]["status"], "cancelled");
    assert_eq!(report["metrics"]["mission"]["phases_total"], 1);
    assert_eq!(lines(&scratch.path("provider-count"))?, ["provider"]);
    assert_eq!(resume(&scratch, &output)?.status.code(), Some(1));
    assert_eq!(lines(&scratch.path("provider-count"))?, ["provider"]);
    Ok(())
}

#[test]
fn authenticated_verifier_cancellation_records_cleanup_and_one_cancelled_terminal() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        true,
    )?;
    let output = scratch.path("out");
    let mut initial = pilot(&scratch);
    initial.args(["run", "--durable", "--stop-after-phase", "review"]);
    run_args_tail(&mut initial, &repo, &mission, &output, &provider, &verifier);
    assert_eq!(initial.output()?.status.code(), Some(3));

    let mut resumed = pilot(&scratch);
    resumed
        .args(["resume", "--output-dir", output.to_str().ok_or("output")?])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = resumed.spawn()?;
    wait_for(&scratch.path("verifier-ready"))?;
    assert_eq!(
        cancel(&scratch, &output, &mission_id(&output)?)?
            .status
            .code(),
        Some(0)
    );
    let stopped = child.wait_with_output()?;
    assert_eq!(stopped.status.code(), Some(1));
    let result = latest_result(&stopped)?;
    assert_eq!(result["status"], "cancelled");
    assert_eq!(result["provider_completed"], false);
    assert_eq!(result["tests_verified"], false);
    assert_eq!(result["phases"][3]["process"]["cleanup_complete"], true);
    assert!(result["phases"][3]["process"]["group_absence"].is_object());
    let records = journal_records(&output)?;
    assert_eq!(
        records
            .iter()
            .filter(|record| record.record.kind == "cancellation_requested")
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.record.kind == "mission_terminal")
            .count(),
        1
    );
    let report = status_report(&status(&scratch, &output)?)?;
    assert_eq!(report["recorded_status"], "cancelled");
    assert_eq!(report["metrics"]["mission"]["status"], "cancelled");
    assert_eq!(report["metrics"]["mission"]["phases_total"], 4);
    assert_eq!(lines(&scratch.path("provider-count"))?.len(), 3);
    assert_eq!(lines(&scratch.path("verifier-count"))?, ["verify"]);
    assert_eq!(resume(&scratch, &output)?.status.code(), Some(1));
    assert_eq!(lines(&scratch.path("verifier-count"))?, ["verify"]);
    Ok(())
}

#[test]
fn cancellation_acknowledges_while_release_observer_is_waiting() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let output = scratch.path("out");
    let ready = scratch.path("release-authorized");
    let release = scratch.path("release-observer");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    command
        .env("NANIKA_PILOT_PROCESS_TEST_BARRIER", "released")
        .env("NANIKA_PILOT_PROCESS_TEST_READY", &ready)
        .env("NANIKA_PILOT_PROCESS_TEST_RELEASE", &release)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn()?;
    let acknowledged = (|| -> TestResult<Output> {
        wait_for(&ready)?;
        cancel(&scratch, &output, &mission_id(&output)?)
    })();
    let pending_status = status(&scratch, &output);
    let pending_records = journal_records(&output);
    // Always unblock the observer before checking the response, including when
    // a regression makes the cancellation client time out on the owner mutex.
    if let Err(error) = fs::write(&release, "release\n") {
        kill_and_wait(child)?;
        return Err(error.into());
    }
    let stopped = child.wait_with_output()?;
    let acknowledged = acknowledged?;
    let pending = status_report(&pending_status?)?;
    assert_eq!(
        acknowledged.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&acknowledged.stderr)
    );
    assert_eq!(pending["recorded_status"], "cancellation_requested");
    assert!(pending["metrics"]["mission"].is_null());
    assert!(
        pending_records?
            .iter()
            .all(|record| record.record.kind != "mission_terminal")
    );
    assert_eq!(stopped.status.code(), Some(1));
    assert!(lines(&scratch.path("provider-count"))?.is_empty());
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    Ok(())
}

#[test]
fn accepted_request_wins_forced_race_with_next_phase_release() -> TestResult {
    let scratch = Scratch::new()?;
    let output = paused_durable_run(&scratch)?;
    let ready = scratch.path("next-phase-claimed");
    let release = scratch.path("next-phase-release");
    let mut command = pilot(&scratch);
    command
        .args(["resume", "--output-dir", output.to_str().ok_or("output")?])
        .env("NANIKA_PILOT_PROCESS_TEST_BARRIER", "claimed")
        .env("NANIKA_PILOT_PROCESS_TEST_READY", &ready)
        .env("NANIKA_PILOT_PROCESS_TEST_RELEASE", &release)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn()?;
    wait_for(&ready)?;

    let exact_mission = mission_id(&output)?;
    let acknowledged = cancel(&scratch, &output, &exact_mission)?;
    assert_eq!(acknowledged.status.code(), Some(0));
    fs::write(&release, "release\n")?;
    let stopped = child.wait_with_output()?;
    assert_eq!(stopped.status.code(), Some(1));
    assert_eq!(lines(&scratch.path("provider-count"))?, ["code-one"]);
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    let records = journal_records(&output)?;
    let request = records
        .iter()
        .position(|record| record.record.kind == "cancellation_requested")
        .ok_or("cancellation record")?;
    assert!(
        records[request + 1..]
            .iter()
            .all(|record| record.record.kind != "phase_started")
    );
    let mission_terminal = records
        .last()
        .filter(|record| record.record.kind == "mission_terminal")
        .ok_or("cancelled mission terminal")?;
    assert_eq!(mission_terminal.record.status.as_deref(), Some("cancelled"));
    let result = mission_terminal
        .record
        .result
        .as_ref()
        .ok_or("cancelled mission result")?;
    assert_eq!(result["provider_completed"], false);
    assert_eq!(result["tests_verified"], false);
    assert_eq!(result["phases"][2]["status"], "skipped");
    assert_eq!(result["phases"][3]["status"], "skipped");
    let report = status_report(&status(&scratch, &output)?)?;
    assert_eq!(report["metrics"]["mission"]["status"], "cancelled");
    assert_eq!(report["metrics"]["mission"]["phases_total"], 1);
    Ok(())
}

#[test]
fn acknowledged_request_survives_owner_restart_without_redispatch() -> TestResult {
    let scratch = Scratch::new()?;
    let output = paused_durable_run(&scratch)?;
    let ready = scratch.path("restart-claimed");
    let release = scratch.path("restart-release");
    let mut command = pilot(&scratch);
    command
        .args(["resume", "--output-dir", output.to_str().ok_or("output")?])
        .env("NANIKA_PILOT_PROCESS_TEST_BARRIER", "claimed")
        .env("NANIKA_PILOT_PROCESS_TEST_READY", &ready)
        .env("NANIKA_PILOT_PROCESS_TEST_RELEASE", &release);
    let child = command.spawn()?;
    wait_for(&ready)?;
    let exact_mission = mission_id(&output)?;
    assert_eq!(
        cancel(&scratch, &output, &exact_mission)?.status.code(),
        Some(0)
    );
    kill_and_wait(child)?;

    let request_history = regular_file_snapshot(&output.join("journal"))?;
    let request_only = status_report(&status(&scratch, &output)?)?;
    assert_eq!(request_only["recorded_status"], "cancellation_requested");
    assert!(request_only["metrics"]["mission"].is_null());
    assert_eq!(
        regular_file_snapshot(&output.join("journal"))?,
        request_history
    );

    let resumed = resume(&scratch, &output)?;
    assert_eq!(resumed.status.code(), Some(1));
    assert_eq!(lines(&scratch.path("provider-count"))?, ["code-one"]);
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    let report = status_report(&status(&scratch, &output)?)?;
    assert_eq!(report["recorded_status"], "cancelled");
    assert_eq!(report["metrics"]["mission"]["status"], "cancelled");
    assert_eq!(
        journal_records(&output)?
            .iter()
            .filter(|record| record.record.kind == "mission_terminal")
            .count(),
        1
    );
    assert_eq!(resume(&scratch, &output)?.status.code(), Some(1));
    Ok(())
}

#[test]
fn cancelled_started_process_stays_request_only_until_exact_recovery() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, provider_ready) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        Some("Add the first change"),
        false,
    )?;
    let output = scratch.path("out");
    let started = scratch.path("started-before-resolution");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    command
        .env("NANIKA_PILOT_PROCESS_TEST_BARRIER", "started")
        .env("NANIKA_PILOT_PROCESS_TEST_READY", &started);
    let child = command.spawn()?;
    wait_for(&started)?;
    wait_for(&provider_ready)?;
    assert_eq!(
        cancel(&scratch, &output, &mission_id(&output)?)?
            .status
            .code(),
        Some(0)
    );
    let pending = status_report(&status(&scratch, &output)?)?;
    assert_eq!(pending["recorded_status"], "cancellation_requested");
    assert!(pending["metrics"]["mission"].is_null());
    assert!(
        journal_records(&output)?
            .iter()
            .all(|record| record.record.kind != "mission_terminal")
    );
    kill_and_wait(child)?;

    let recovered = resume(&scratch, &output)?;
    assert_eq!(recovered.status.code(), Some(1));
    let result = latest_result(&recovered)?;
    assert_eq!(result["status"], "cancelled");
    assert_eq!(
        result["phases"][0]["process"]["recovery"],
        "ReleasedLostOutcomeUncertain"
    );
    assert_eq!(lines(&scratch.path("provider-count"))?, ["provider"]);
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    let report = status_report(&status(&scratch, &output)?)?;
    assert_eq!(report["recorded_status"], "cancelled");
    assert_eq!(report["metrics"]["mission"]["phases_total"], 1);
    assert_eq!(resume(&scratch, &output)?.status.code(), Some(1));
    assert_eq!(lines(&scratch.path("provider-count"))?, ["provider"]);
    Ok(())
}

#[test]
fn completed_replay_remains_terminal_and_refuses_live_cancellation() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    assert!(command.output()?.status.success());
    let exact_mission = mission_id(&output)?;
    assert_eq!(
        cancel(&scratch, &output, &exact_mission)?.status.code(),
        Some(1)
    );
    assert!(resume(&scratch, &output)?.status.success());
    assert_eq!(lines(&scratch.path("provider-count"))?.len(), 3);
    assert_eq!(lines(&scratch.path("verifier-count"))?.len(), 1);
    Ok(())
}

#[test]
fn cancelled_terminal_without_authenticated_request_is_rejected() -> TestResult {
    let scratch = Scratch::new()?;
    let output = paused_durable_run(&scratch)?;
    let records = journal_records(&output)?;
    let last = records.last().ok_or("journal is empty")?;
    append_journal_record(
        &output,
        SavedRecord {
            schema: last.record.schema.clone(),
            mission_id: last.record.mission_id.clone(),
            sequence: 0,
            event_id: String::new(),
            timestamp: last.record.timestamp.clone(),
            kind: "mission_terminal".to_owned(),
            phase_id: None,
            role: None,
            status: Some("cancelled".to_owned()),
            reason: Some("operator cancellation reached terminal after owned cleanup".to_owned()),
            result: Some(serde_json::json!({})),
            baseline_digest: last.record.baseline_digest.clone(),
            workspace_digest: last.record.workspace_digest.clone(),
        },
    )?;

    let inspected = status(&scratch, &output)?;
    assert_eq!(inspected.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&inspected.stderr)
            .contains("no preceding durable cancellation request")
    );
    let resumed = resume(&scratch, &output)?;
    assert_eq!(resumed.status.code(), Some(1));
    assert_eq!(lines(&scratch.path("provider-count"))?, ["code-one"]);
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    Ok(())
}

#[test]
fn cancelled_terminal_result_must_match_its_request_and_core_projection() -> TestResult {
    let scratch = Scratch::new()?;
    let output = paused_durable_run(&scratch)?;
    let records = journal_records(&output)?;
    let last = records.last().ok_or("journal is empty")?;
    let schema = last.record.schema.clone();
    let mission_id = last.record.mission_id.clone();
    let timestamp = last.record.timestamp.clone();
    let baseline_digest = last.record.baseline_digest.clone();
    let workspace_digest = last.record.workspace_digest.clone();
    append_journal_record(
        &output,
        SavedRecord {
            schema: schema.clone(),
            mission_id: mission_id.clone(),
            sequence: 0,
            event_id: String::new(),
            timestamp: timestamp.clone(),
            kind: "cancellation_requested".to_owned(),
            phase_id: None,
            role: None,
            status: Some("requested".to_owned()),
            reason: Some("operator cancellation requested".to_owned()),
            result: None,
            baseline_digest: None,
            workspace_digest: None,
        },
    )?;
    append_journal_record(
        &output,
        SavedRecord {
            schema,
            mission_id,
            sequence: 0,
            event_id: String::new(),
            timestamp,
            kind: "mission_terminal".to_owned(),
            phase_id: None,
            role: None,
            status: Some("cancelled".to_owned()),
            reason: Some("operator cancellation reached terminal after owned cleanup".to_owned()),
            result: Some(serde_json::json!({
                "status": "failed",
                "completed": true,
                "provider_completed": true,
                "tests_verified": true
            })),
            baseline_digest,
            workspace_digest,
        },
    )?;

    let inspected = status(&scratch, &output)?;
    assert_eq!(inspected.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&inspected.stderr)
            .contains("mission result does not match its terminal record")
    );
    assert_eq!(lines(&scratch.path("provider-count"))?, ["code-one"]);
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    Ok(())
}

#[test]
fn preparation_refusals_finish_terminally_without_execution_metrics() -> TestResult {
    for case in ["review-packet", "code-executable"] {
        let scratch = Scratch::new()?;
        let repo = scratch.repo()?;
        let mission = scratch.file("mission.md", MISSION, false)?;
        let (provider, verifier, _) = fixture(
            &scratch,
            r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
            None,
            false,
        )?;
        let contents = fs::read_to_string(&provider)?;
        let addition = if case == "review-packet" {
            // Diff plus final file exceeds the accepted 512 KiB review packet.
            "awk 'BEGIN { for (i=0; i<300000; i++) printf \"A\"; print \"\" }' > large.txt; "
        } else {
            // The next phase cannot open its pinned executable. The current
            // admitted shell continues and finishes its already-read branch.
            "rm -- \"$0\"; "
        };
        let marker = "printf 'one\\n' > phase-one.txt; emit_code;;";
        assert!(contents.contains(marker));
        fs::write(
            &provider,
            contents.replace(marker, &format!("{addition}{marker}")),
        )?;
        let output = scratch.path("out");
        let mut command = pilot(&scratch);
        run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
        let failed = command.output()?;
        assert_eq!(failed.status.code(), Some(1), "{case}");
        let result = latest_result(&failed)?;
        let index = if case == "review-packet" { 2 } else { 1 };
        let refused = &result["phases"][index];
        assert_eq!(refused["status"], "failed", "{case}");
        assert_eq!(refused["provider_dispatched"], false, "{case}");
        assert!(refused.get("process").is_none(), "{case}");
        assert!(
            !refused["reason"]
                .as_str()
                .ok_or("failure reason")?
                .is_empty()
        );
        assert_eq!(result["phases"][3]["status"], "skipped", "{case}");
        let report = status_report(&status(&scratch, &output)?)?;
        assert_eq!(report["recorded_status"], "failed", "{case}");
        assert_eq!(
            report["metrics"]["phases"].as_array().map(Vec::len),
            Some(index)
        );
        assert_eq!(report["metrics"]["mission"]["phases_total"], index);
        let before = regular_file_snapshot(&output)?;
        for _ in 0..2 {
            let replay = resume(&scratch, &output)?;
            assert_eq!(replay.status.code(), Some(1), "{case}");
            assert_eq!(latest_result(&replay)?, result, "{case}");
            assert_eq!(regular_file_snapshot(&output)?, before, "{case}");
        }
        assert_eq!(lines(&scratch.path("provider-count"))?.len(), index);
        assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    }
    Ok(())
}

#[test]
fn verifier_artifact_failure_after_execution_is_not_preparation() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let mut script = fs::read_to_string(&verifier)?;
    // Force the owner's output artifact write to fail after verifier execution
    // and cleanup have already completed. There is no returned result record.
    script.push_str("mkdir \"${NANIKA_VERIFICATION_REPORT%/*}/stdout.txt\"\n");
    fs::write(&verifier, script)?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    let failed = command.output()?;
    assert_eq!(failed.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&failed.stderr)
            .contains("has no durable process observation for metrics")
    );
    assert_eq!(lines(&scratch.path("provider-count"))?.len(), 3);
    assert_eq!(lines(&scratch.path("verifier-count"))?.len(), 1);
    let mut entries = fs::read_dir(output.join("journal"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();
    let last: SavedEnvelope = serde_json::from_slice(&fs::read(entries.last().ok_or("journal")?)?)?;
    assert_eq!(last.record.kind, "phase_started");
    assert_eq!(last.record.role.as_deref(), Some("verification"));
    // Recovery still owns the executed attempt; no invented preparation terminal.
    Ok(())
}

#[test]
fn confirmed_provider_no_start_finishes_failed_without_execution_metrics() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let original = fs::read(&provider)?;
    let ready = scratch.path("claim-ready");
    let release = scratch.path("claim-release");
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    command
        .env("NANIKA_PILOT_PROCESS_TEST_BARRIER", "claimed")
        .env("NANIKA_PILOT_PROCESS_TEST_READY", &ready)
        .env("NANIKA_PILOT_PROCESS_TEST_RELEASE", &release)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn()?;
    let tamper = (|| -> TestResult {
        wait_for(&ready)?;
        // The admitted executable's digest must fail before any target launch.
        fs::write(&provider, "#!/bin/sh\nexit 99\n")?;
        fs::write(&release, "release\n")?;
        Ok(())
    })();
    if let Err(error) = tamper {
        kill_and_wait(child)?;
        return Err(error);
    }
    let failed = child.wait_with_output()?;
    fs::write(&provider, original)?;
    assert_eq!(failed.status.code(), Some(1));
    let result = latest_result(&failed)?;
    let first = &result["phases"][0];
    assert_eq!(first["status"], "failed");
    assert_eq!(first["provider_dispatched"], false);
    assert_eq!(first["process"]["not_started_reason"], "SpawnFailed");
    assert!(
        first["reason"]
            .as_str()
            .ok_or("phase reason")?
            .contains("process confirmed not started")
    );
    assert!(lines(&scratch.path("provider-count"))?.is_empty());
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    let report = status_report(&status(&scratch, &output)?)?;
    assert_eq!(
        report["metrics"]["phases"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(report["metrics"]["mission"]["phases_total"], 0);
    let replay = resume(&scratch, &output)?;
    assert_eq!(replay.status.code(), Some(1));
    assert_eq!(latest_result(&replay)?, result);
    assert!(lines(&scratch.path("provider-count"))?.is_empty());
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    Ok(())
}

#[test]
fn killed_provider_start_is_ambiguous_and_never_repeated() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let ready = scratch.path("before-release-ready");
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    command
        .env("NANIKA_PILOT_PROCESS_TEST_BARRIER", "claimed")
        .env("NANIKA_PILOT_PROCESS_TEST_READY", &ready);
    let child = command.spawn()?;
    wait_for(&ready)?;
    kill_and_wait(child)?;
    let resumed = resume(&scratch, &output)?;
    assert_eq!(resumed.status.code(), Some(1));
    assert_eq!(
        latest_result(&resumed)?["phases"][0]["process"]["recovery"],
        "RecoveredBeforeStartNotStarted"
    );
    assert!(lines(&scratch.path("provider-count"))?.is_empty());
    Ok(())
}

#[test]
fn killed_verifier_start_is_ambiguous_and_never_repeated() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        true,
    )?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    command.args(["run", "--durable", "--stop-after-phase", "review"]);
    run_args_tail(&mut command, &repo, &mission, &output, &provider, &verifier);
    assert_eq!(command.output()?.status.code(), Some(3));

    let ready = scratch.path("after-release-ready");
    let mut command = pilot(&scratch);
    command
        .args(["resume", "--output-dir", output.to_str().ok_or("output")?])
        .env("NANIKA_PILOT_PROCESS_TEST_BARRIER", "started")
        .env("NANIKA_PILOT_PROCESS_TEST_READY", &ready);
    let child = command.spawn()?;
    wait_for(&ready)?;
    wait_for(&scratch.path("verifier-ready"))?;
    kill_and_wait(child)?;
    let resumed = resume(&scratch, &output)?;
    assert_eq!(resumed.status.code(), Some(1));
    assert_eq!(
        latest_result(&resumed)?["phases"][3]["process"]["recovery"],
        "ReleasedLostOutcomeUncertain"
    );
    assert_eq!(lines(&scratch.path("verifier-count"))?, ["verify"]);
    Ok(())
}

#[test]
fn lost_cleanup_record_keeps_provider_phase_unresolved_until_recovery() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let backup = scratch.path("ownership-backup");
    let script = fs::read_to_string(&provider)?;
    let corrupt = format!(
        "input=$(cat)\ncp ../orchestrator.process-ownership.v1 '{}'\nprintf corrupt > ../orchestrator.process-ownership.v1\n",
        backup.display()
    );
    fs::write(&provider, script.replace("input=$(cat)\n", &corrupt))?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    let failed = command.output()?;
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("cleanup is unresolved"));
    let report = status_report(&status(&scratch, &output)?)?;
    assert_eq!(report["recorded_status"], "unresolved");
    assert_eq!(report["unresolved_phase_count"], 1);
    assert_eq!(lines(&scratch.path("provider-count"))?, ["code-one"]);
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    fs::write(
        output.join("orchestrator.process-ownership.v1"),
        fs::read(backup)?,
    )?;
    let recovered = resume(&scratch, &output)?;
    assert_eq!(recovered.status.code(), Some(1));
    assert_eq!(
        latest_result(&recovered)?["phases"][0]["process"]["recovery"],
        "ReleasedLostOutcomeUncertain"
    );
    assert_eq!(lines(&scratch.path("provider-count"))?, ["code-one"]);
    Ok(())
}

#[test]
fn interrupted_original_v1_run_remains_unresolved_without_rewriting_history() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let ready = scratch.path("legacy-interrupted-ready");
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    command
        .env("NANIKA_PILOT_PROCESS_TEST_BARRIER", "claimed")
        .env("NANIKA_PILOT_PROCESS_TEST_READY", &ready);
    let child = command.spawn()?;
    wait_for(&ready)?;
    kill_and_wait(child)?;
    downgrade_to_original_v1_manifest(&output)?;
    let history = regular_file_snapshot(&output.join("journal"))?;
    let manifest = fs::read(output.join("manifest.json"))?;
    let resumed = resume(&scratch, &output)?;
    assert!(!resumed.status.success());
    assert!(String::from_utf8_lossy(&resumed.stderr).contains("legacy interrupted phase"));
    assert_eq!(regular_file_snapshot(&output.join("journal"))?, history);
    assert_eq!(fs::read(output.join("manifest.json"))?, manifest);
    assert!(lines(&scratch.path("provider-count"))?.is_empty());
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    Ok(())
}

#[test]
fn changed_workspace_source_baseline_and_manifest_are_refused() -> TestResult {
    for changed in [
        "workspace",
        "source",
        "source-head",
        "baseline",
        "manifest",
        "plan",
    ] {
        let scratch = Scratch::new()?;
        let repo = scratch.repo()?;
        let mission = scratch.file("mission.md", MISSION, false)?;
        let (provider, verifier, _) = fixture(
            &scratch,
            r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
            None,
            false,
        )?;
        let output = scratch.path("out");
        let mut command = pilot(&scratch);
        command.args(["run", "--durable", "--stop-after-phase", "code-one"]);
        run_args_tail(&mut command, &repo, &mission, &output, &provider, &verifier);
        assert_eq!(command.output()?.status.code(), Some(3));
        match changed {
            "workspace" => fs::write(output.join("workspace/phase-one.txt"), "changed\n")?,
            "source" => fs::write(repo.join("base.txt"), "changed\n")?,
            "source-head" => {
                fs::write(repo.join("base.txt"), "changed\n")?;
                git(&repo, &["add", "base.txt"])?;
                git(&repo, &["commit", "-q", "-m", "changed-head"])?;
            }
            "baseline" => fs::write(output.join("source-head/base.txt"), "changed\n")?,
            "manifest" => {
                let path = output.join("manifest.json");
                let mut bytes = fs::read(&path)?;
                bytes[0] ^= 1;
                fs::write(path, bytes)?;
            }
            "plan" => {
                let path = output.join("manifest.json");
                let mut manifest: Value = serde_json::from_slice(&fs::read(&path)?)?;
                manifest["plan"]["phases"][0]["name"] = Value::String("mismatch".to_owned());
                fs::write(path, serde_json::to_vec_pretty(&manifest)?)?;
            }
            _ => unreachable!(),
        }
        let resumed = resume(&scratch, &output)?;
        assert_eq!(resumed.status.code(), Some(1), "{changed}");
        assert_eq!(lines(&scratch.path("provider-count"))?, ["code-one"]);
    }
    Ok(())
}

#[test]
fn competing_resume_writer_is_refused() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, ready) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        Some("Add the second change"),
        false,
    )?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    command.args(["run", "--durable", "--stop-after-phase", "code-one"]);
    run_args_tail(&mut command, &repo, &mission, &output, &provider, &verifier);
    assert_eq!(command.output()?.status.code(), Some(3));

    let mut first = pilot(&scratch);
    first.args(["resume", "--output-dir", output.to_str().ok_or("output")?]);
    let child = first.spawn()?;
    wait_for(&ready)?;
    let competing = resume(&scratch, &output)?;
    assert_eq!(competing.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&competing.stderr).contains("private writer admission refused")
    );
    kill_and_wait(child)?;
    Ok(())
}

#[test]
fn one_writer_stays_contended_across_two_fresh_process_attempts_and_journal_boundaries()
-> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, ready) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        Some("Add the second change"),
        false,
    )?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    let owner = command.spawn()?;
    wait_for(&ready)?;

    let competing = resume(&scratch, &output)?;
    assert_eq!(competing.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&competing.stderr).contains("private writer admission refused")
    );

    let completed = owner.wait_with_output()?;
    assert!(
        completed.status.success(),
        "{}",
        String::from_utf8_lossy(&completed.stderr)
    );
    assert_eq!(
        lines(&scratch.path("provider-count"))?,
        ["code-one", "provider", "review"]
    );
    assert!(resume(&scratch, &output)?.status.success());
    Ok(())
}

#[test]
fn changed_pinned_provider_is_refused_before_a_second_child() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    command.args(["run", "--durable", "--stop-after-phase", "code-one"]);
    run_args_tail(&mut command, &repo, &mission, &output, &provider, &verifier);
    assert_eq!(command.output()?.status.code(), Some(3));

    fs::write(
        &provider,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.154.0'; exit 0; fi\nprintf '%s\\n' unexpected >> '{}'\nexit 88\n",
            scratch.path("provider-count").display()
        ),
    )?;
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700))?;
    let resumed = resume(&scratch, &output)?;
    assert_eq!(resumed.status.code(), Some(1));
    assert_eq!(lines(&scratch.path("provider-count"))?, ["code-one"]);
    Ok(())
}

#[test]
fn resume_refuses_a_saved_fifo_without_hanging() -> TestResult {
    let scratch = Scratch::new()?;
    let output = paused_durable_run(&scratch)?;
    let manifest = output.join("manifest.json");
    fs::remove_file(&manifest)?;
    let mut mkfifo = Command::new("mkfifo");
    mkfifo.args(["-m", "600"]).arg(&manifest);
    let created = bounded_output(&mut mkfifo, Duration::from_secs(2))?;
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let mut command = pilot(&scratch);
    command.args([
        "resume",
        "--output-dir",
        output.to_str().ok_or("output path")?,
    ]);
    let refused = bounded_output(&mut command, Duration::from_secs(2))?;
    assert_eq!(
        refused.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    Ok(())
}

#[test]
fn resume_refuses_journal_overflow_during_enumeration() -> TestResult {
    let scratch = Scratch::new()?;
    let output = paused_durable_run(&scratch)?;
    let journal = output.join("journal");
    for sequence in 1..=513 {
        let path = journal.join(format!("{sequence:06}.json"));
        if !path.exists() {
            fs::write(&path, b"{}\n")?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
    }

    let refused = resume(&scratch, &output)?;
    assert_eq!(
        refused.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(String::from_utf8_lossy(&refused.stderr).contains("journal count is invalid"));
    Ok(())
}

#[test]
fn resume_refuses_saved_file_link_mode_and_size_corruption() -> TestResult {
    let scratch = Scratch::new()?;
    let output = paused_durable_run(&scratch)?;
    let manifest = output.join("manifest.json");
    let original = fs::read(&manifest)?;
    let link_source = scratch.path("manifest-link-source");
    replace_with_private_file(&link_source, &original)?;

    for corruption in ["symlink", "hardlink", "nonprivate", "oversize"] {
        replace_with_private_file(&manifest, &original)?;
        match corruption {
            "symlink" => {
                fs::remove_file(&manifest)?;
                symlink(&link_source, &manifest)?;
            }
            "hardlink" => {
                fs::remove_file(&manifest)?;
                fs::hard_link(&link_source, &manifest)?;
            }
            "nonprivate" => {
                fs::set_permissions(&manifest, fs::Permissions::from_mode(0o644))?;
            }
            "oversize" => {
                let file = fs::File::create(&manifest)?;
                file.set_len(1024 * 1024 + 1)?;
                fs::set_permissions(&manifest, fs::Permissions::from_mode(0o600))?;
            }
            _ => unreachable!(),
        }

        let refused = resume(&scratch, &output)?;
        assert_eq!(
            refused.status.code(),
            Some(1),
            "{corruption}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
    Ok(())
}

#[test]
fn status_reports_paused_then_completed_with_ordered_progress() -> TestResult {
    let scratch = Scratch::new()?;
    let output = paused_durable_run(&scratch)?;

    let paused = status_report(&status(&scratch, &output)?)?;
    assert_eq!(paused["schema"], "nanika.rust-first-use-durable-status.v1");
    assert_eq!(paused["recorded_status"], "paused");
    assert_eq!(paused["execution_liveness"], "not_checked");
    assert_eq!(paused["durability"], "not_attested");
    assert_eq!(paused["total_phase_count"], 4);
    assert_eq!(paused["completed_phase_count"], 1);
    assert!(
        paused["mission_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert!(
        paused["last_journal_timestamp"]
            .as_str()
            .is_some_and(|timestamp| !timestamp.is_empty())
    );
    assert_eq!(paused["phases"][0]["name"], "code-one");
    assert_eq!(paused["phases"][0]["role"], "code");
    assert_eq!(paused["phases"][0]["recorded_status"], "completed");
    assert!(paused["phases"][0]["model"].is_string());
    assert!(paused["phases"][0]["effort"].is_string());
    assert_eq!(paused["phases"][3]["role"], "verification");
    assert!(paused["phases"][3]["model"].is_null());
    assert!(paused["phases"][3]["effort"].is_null());

    let resumed = resume(&scratch, &output)?;
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let completed = status_report(&status(&scratch, &output)?)?;
    assert_eq!(completed["recorded_status"], "completed");
    assert_eq!(completed["execution_liveness"], "not_checked");
    assert_eq!(completed["durability"], "not_attested");
    assert_eq!(completed["completed_phase_count"], 4);
    assert_eq!(completed["failed_phase_count"], 0);
    assert_eq!(completed["unresolved_phase_count"], 0);
    Ok(())
}

#[test]
fn status_reports_durable_failure_as_a_valid_inspection() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"reject","blockers":["broken"],"warnings":[]}"#,
        None,
        false,
    )?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    assert_eq!(command.output()?.status.code(), Some(1));

    let report = status_report(&status(&scratch, &output)?)?;
    assert_eq!(report["recorded_status"], "failed");
    assert_eq!(report["execution_liveness"], "not_checked");
    assert_eq!(report["durability"], "not_attested");
    assert_eq!(report["failed_phase_count"], 1);
    Ok(())
}

#[test]
fn status_reports_unresolved_start_while_writer_is_held_without_claiming_liveness() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, ready) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        Some("Add the first change"),
        false,
    )?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    let child = command.spawn()?;
    wait_for(&ready)?;

    let report = status_report(&status(&scratch, &output)?)?;
    assert_eq!(report["recorded_status"], "unresolved");
    assert_eq!(report["execution_liveness"], "not_checked");
    assert_eq!(report["durability"], "not_attested");
    assert_eq!(report["unresolved_phase_count"], 1);
    assert_eq!(report["phases"][0]["recorded_status"], "unresolved");
    kill_and_wait(child)?;
    Ok(())
}

#[test]
fn status_is_read_only_and_source_independent_and_executes_no_saved_commands() -> TestResult {
    for replacement in ["missing", "file", "directory", "symlink", "inaccessible"] {
        let scratch = Scratch::new()?;
        let inaccessible_parent = scratch.path("inaccessible-source-parent");
        let output = if replacement == "inaccessible" {
            let repo = scratch.nested_repo("inaccessible-source-parent")?;
            paused_durable_run_with_repo(&scratch, &repo)?
        } else {
            paused_durable_run(&scratch)?
        };
        let manifest: Value = serde_json::from_slice(&fs::read(output.join("manifest.json"))?)?;
        let source = PathBuf::from(
            manifest["source"]["canonical_path"]
                .as_str()
                .ok_or("source")?,
        );
        let provider = PathBuf::from(
            manifest["provider"]["executable"]
                .as_str()
                .ok_or("provider")?,
        );
        let execution_counter = scratch.path("status-execution-counter");
        fs::write(
            &provider,
            format!(
                "#!/bin/sh\nprintf invoked >> '{}'\nexit 97\n",
                execution_counter.display()
            ),
        )?;
        fs::set_permissions(&provider, fs::Permissions::from_mode(0o700))?;
        match replacement {
            "missing" => fs::remove_dir_all(&source)?,
            "file" => {
                fs::remove_dir_all(&source)?;
                fs::write(&source, "replacement\n")?;
            }
            "directory" => {
                fs::remove_dir_all(&source)?;
                fs::create_dir(&source)?;
            }
            "symlink" => {
                fs::remove_dir_all(&source)?;
                let target = scratch.path("replacement-source-target");
                fs::create_dir(&target)?;
                symlink(&target, &source)?;
            }
            "inaccessible" => {
                fs::set_permissions(&inaccessible_parent, fs::Permissions::from_mode(0o000))?;
                let error = source
                    .symlink_metadata()
                    .err()
                    .ok_or("source remained inspectable after removing ancestor access")?;
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            }
            _ => unreachable!(),
        }
        let provider_before = lines(&scratch.path("provider-count"))?;
        let verifier_before = lines(&scratch.path("verifier-count"))?;
        let before = regular_file_snapshot(&output)?;

        let observed = status(&scratch, &output)?;
        if replacement == "inaccessible" {
            fs::set_permissions(&inaccessible_parent, fs::Permissions::from_mode(0o700))?;
        }
        let report = status_report(&observed)?;
        assert_eq!(report["recorded_status"], "paused", "{replacement}");
        assert_eq!(report["execution_liveness"], "not_checked", "{replacement}");
        assert_eq!(report["durability"], "not_attested", "{replacement}");
        assert_eq!(regular_file_snapshot(&output)?, before, "{replacement}");
        assert_eq!(
            lines(&scratch.path("provider-count"))?,
            provider_before,
            "{replacement}"
        );
        assert_eq!(
            lines(&scratch.path("verifier-count"))?,
            verifier_before,
            "{replacement}"
        );
        assert!(!execution_counter.exists(), "{replacement}");
    }
    Ok(())
}

#[test]
fn status_refuses_missing_sequence_bad_digest_and_malformed_saved_data() -> TestResult {
    for corruption in ["missing-sequence", "bad-digest", "malformed"] {
        let scratch = Scratch::new()?;
        let output = paused_durable_run(&scratch)?;
        let journal = output.join("journal");
        match corruption {
            "missing-sequence" => fs::remove_file(journal.join("000001.json"))?,
            "bad-digest" => rewrite_json(&last_record_path(&output)?, |record| {
                record["digest"] = Value::String("0".repeat(64));
                Ok(())
            })?,
            "malformed" => replace_with_private_file(&last_record_path(&output)?, b"{\n")?,
            _ => unreachable!(),
        }
        let refused = status(&scratch, &output)?;
        assert_eq!(
            refused.status.code(),
            Some(1),
            "{corruption}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
        assert!(refused.stdout.is_empty(), "{corruption}");
    }
    Ok(())
}

#[test]
fn status_prior_journal_replacement_uses_valid_old_format_contract() -> TestResult {
    for corruption in [
        "root-symlink",
        "root-replacement",
        "journal-symlink",
        "journal-replacement",
    ] {
        let scratch = Scratch::new()?;
        let output = paused_durable_run(&scratch)?;
        match corruption {
            "root-symlink" => {
                let displaced = scratch.path("displaced-root");
                fs::rename(&output, &displaced)?;
                symlink(&displaced, &output)?;
            }
            "root-replacement" => {
                let displaced = scratch.path("displaced-root");
                fs::rename(&output, &displaced)?;
                fs::create_dir(&output)?;
                fs::set_permissions(&output, fs::Permissions::from_mode(0o700))?;
                for name in ["manifest.json", "orchestrator.rust-pilot.seal"] {
                    fs::copy(displaced.join(name), output.join(name))?;
                    fs::set_permissions(output.join(name), fs::Permissions::from_mode(0o600))?;
                }
                fs::create_dir(output.join("journal"))?;
                fs::set_permissions(output.join("journal"), fs::Permissions::from_mode(0o700))?;
            }
            "journal-symlink" => {
                let journal = output.join("journal");
                let displaced = output.join("displaced-journal");
                fs::rename(&journal, &displaced)?;
                symlink(&displaced, &journal)?;
            }
            "journal-replacement" => {
                replace_journal_directory_with_copy(&output)?;
            }
            _ => unreachable!(),
        }
        let observed = status(&scratch, &output)?;
        if corruption == "journal-replacement" {
            let report = status_report(&observed)?;
            assert_eq!(report["recorded_status"], "paused");
            assert_eq!(report["durability"], "not_attested");
        } else {
            assert_eq!(
                observed.status.code(),
                Some(1),
                "{corruption}: {}",
                String::from_utf8_lossy(&observed.stderr)
            );
        }
    }
    Ok(())
}

#[test]
fn status_refuses_bad_root_record_modes_and_fifo_without_hanging() -> TestResult {
    for corruption in [
        "root-mode",
        "seal",
        "journal-mode",
        "record-mode",
        "record-fifo",
    ] {
        let scratch = Scratch::new()?;
        let output = paused_durable_run(&scratch)?;
        assert_eq!(
            fs::metadata(&output)?.uid(),
            rustix::process::geteuid().as_raw()
        );
        match corruption {
            "root-mode" => {
                fs::set_permissions(&output, fs::Permissions::from_mode(0o755))?;
            }
            "seal" => {
                replace_with_private_file(
                    &output.join("orchestrator.rust-pilot.seal"),
                    b"not-this-pilot\n",
                )?;
            }
            "journal-mode" => {
                fs::set_permissions(output.join("journal"), fs::Permissions::from_mode(0o755))?;
            }
            "record-mode" => {
                fs::set_permissions(
                    last_record_path(&output)?,
                    fs::Permissions::from_mode(0o644),
                )?;
            }
            "record-fifo" => {
                let record = last_record_path(&output)?;
                fs::remove_file(&record)?;
                let created = bounded_output(
                    Command::new("mkfifo").args(["-m", "600"]).arg(&record),
                    Duration::from_secs(2),
                )?;
                assert!(created.status.success());
            }
            _ => unreachable!(),
        }
        let refused = bounded_output(
            pilot(&scratch).args(["status", "--output-dir", output.to_str().ok_or("output")?]),
            Duration::from_secs(2),
        )?;
        assert_eq!(refused.status.code(), Some(1), "{corruption}");
    }
    Ok(())
}

#[test]
fn status_argument_contract_is_exact() -> TestResult {
    let scratch = Scratch::new()?;
    let output = paused_durable_run(&scratch)?;
    let output_text = output.to_str().ok_or("output")?;
    let cases = [
        vec!["status"],
        vec!["status", "--output-dir", output_text, "extra"],
        vec![
            "status",
            "--output-dir",
            output_text,
            "--output-dir",
            output_text,
        ],
        vec!["status", "--output-dir", output_text, "--runtime", "codex"],
        vec![
            "status",
            "--output-dir",
            output_text,
            "--codex",
            "one",
            "--codex",
            "two",
        ],
        vec!["status", "--output-dir", output_text, "--timeout-secs", "1"],
    ];
    for arguments in cases {
        let refused = pilot(&scratch).args(arguments).output()?;
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
    Ok(())
}

#[test]
fn status_refuses_missing_duplicate_extra_and_inconsistent_routes() -> TestResult {
    for corruption in ["missing", "duplicate", "extra", "inconsistent"] {
        let scratch = Scratch::new()?;
        let output = paused_durable_run(&scratch)?;
        rewrite_json(&output.join("manifest.json"), |manifest| {
            let routes = manifest["routes"].as_array_mut().ok_or("routes")?;
            match corruption {
                "missing" => {
                    routes.remove(0);
                }
                "duplicate" => {
                    let duplicate = routes[0].clone();
                    routes[1] = duplicate;
                }
                "extra" => {
                    let extra = routes[0].clone();
                    routes.push(extra);
                }
                "inconsistent" => {
                    routes[0]["phase_id"] = Value::String("verify".to_owned());
                }
                _ => unreachable!(),
            }
            Ok(())
        })?;
        let refused = status(&scratch, &output)?;
        assert_eq!(refused.status.code(), Some(1), "{corruption}");
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("selected routes"),
            "{corruption}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
    Ok(())
}

#[test]
fn concurrent_writer_observations_are_valid_json_prefixes_or_bounded_refusals() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, ready) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        Some("Add the first change"),
        false,
    )?;
    let output = scratch.path("out");
    let mut command = pilot(&scratch);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    let mut child = command.spawn()?;
    wait_for(&ready)?;
    let mut valid = 0_usize;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let observed = status(&scratch, &output)?;
        match observed.status.code() {
            Some(0) => {
                let report: Value = serde_json::from_slice(&observed.stdout)?;
                assert_eq!(report["schema"], "nanika.rust-first-use-durable-status.v1");
                assert_eq!(report["execution_liveness"], "not_checked");
                assert_eq!(report["durability"], "not_attested");
                assert!(report["last_journal_sequence"].as_i64().is_some());
                valid += 1;
            }
            Some(1) => assert!(observed.stdout.is_empty()),
            other => return Err(format!("unexpected status exit {other:?}").into()),
        }
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill()?;
            let _ = child.wait()?;
            return Err("concurrent durable writer did not finish within ten seconds".into());
        }
    }
    let _ = child.wait()?;
    assert!(valid > 0);
    Ok(())
}

#[test]
fn abrupt_metrics_owner_death_redelivers_exactly_once_without_repeating_target_effects()
-> TestResult {
    for barrier in ["admitted", "consumed"] {
        let scratch = Scratch::new()?;
        let repo = scratch.repo()?;
        let mission = scratch.file("mission.md", MISSION, false)?;
        let (provider, verifier, _) = fixture(
            &scratch,
            r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
            None,
            false,
        )?;
        let output = scratch.path("out");
        let ready = scratch.path("metrics-barrier-ready");
        let mut command = pilot(&scratch);
        command
            .env("NANIKA_PILOT_METRICS_TEST_BARRIER", barrier)
            .env("NANIKA_PILOT_METRICS_TEST_READY", &ready);
        run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
        let child = command.spawn()?;
        wait_for(&ready)?;
        kill_and_wait(child)?;
        assert_eq!(lines(&scratch.path("provider-count"))?, ["code-one"]);

        let resumed = resume(&scratch, &output)?;
        assert!(
            resumed.status.success(),
            "{barrier}: {}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        assert_eq!(
            lines(&scratch.path("provider-count"))?,
            ["code-one", "code-two", "review"],
            "{barrier}"
        );
        assert_eq!(lines(&scratch.path("verifier-count"))?, ["verify"]);
        let report = status_report(&status(&scratch, &output)?)?;
        assert_eq!(report["metrics"]["mission"]["phases_total"], 4);
        assert_eq!(
            report["metrics"]["phases"].as_array().map(Vec::len),
            Some(4)
        );
        assert!(resume(&scratch, &output)?.status.success());
        assert_eq!(lines(&scratch.path("provider-count"))?.len(), 3);
        assert_eq!(lines(&scratch.path("verifier-count"))?.len(), 1);
    }
    Ok(())
}

#[test]
fn terminal_publication_admission_replays_the_exact_authoritative_terminal() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let output = scratch.path("out");
    let ready = scratch.path("terminal-metrics-ready");
    let mut command = pilot(&scratch);
    command
        .env("NANIKA_PILOT_METRICS_TEST_BARRIER", "terminal-admitted")
        .env("NANIKA_PILOT_METRICS_TEST_READY", &ready);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    let child = command.spawn()?;
    wait_for(&ready)?;
    kill_and_wait(child)?;
    assert_eq!(lines(&scratch.path("provider-count"))?.len(), 3);
    assert_eq!(lines(&scratch.path("verifier-count"))?.len(), 1);

    let resumed = resume(&scratch, &output)?;
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let first = status_report(&status(&scratch, &output)?)?;
    assert_eq!(first["recorded_status"], "completed");
    assert_eq!(first["metrics"]["mission"]["phases_total"], 4);
    assert_eq!(first["metrics"]["phases"].as_array().map(Vec::len), Some(4));
    assert!(resume(&scratch, &output)?.status.success());
    let repeated = status_report(&status(&scratch, &output)?)?;
    assert_eq!(repeated["metrics"], first["metrics"]);
    assert_eq!(lines(&scratch.path("provider-count"))?.len(), 3);
    assert_eq!(lines(&scratch.path("verifier-count"))?.len(), 1);
    Ok(())
}

#[test]
fn cancelled_terminal_publication_crash_replays_once_without_repeating_effects() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, provider_ready) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        Some("Add the first change"),
        false,
    )?;
    let output = scratch.path("out");
    let terminal_ready = scratch.path("cancelled-terminal-metrics-ready");
    let mut command = pilot(&scratch);
    command
        .env("NANIKA_PILOT_METRICS_TEST_BARRIER", "terminal-admitted")
        .env("NANIKA_PILOT_METRICS_TEST_READY", &terminal_ready);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    let child = command.spawn()?;
    wait_for(&provider_ready)?;
    assert_eq!(
        cancel(&scratch, &output, &mission_id(&output)?)?
            .status
            .code(),
        Some(0)
    );
    wait_for(&terminal_ready)?;
    kill_and_wait(child)?;
    assert_eq!(lines(&scratch.path("provider-count"))?, ["provider"]);
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    assert!(
        journal_records(&output)?
            .iter()
            .all(|record| record.record.kind != "mission_terminal")
    );

    assert_eq!(resume(&scratch, &output)?.status.code(), Some(1));
    let first = status_report(&status(&scratch, &output)?)?;
    assert_eq!(first["recorded_status"], "cancelled");
    assert_eq!(first["metrics"]["mission"]["status"], "cancelled");
    assert_eq!(first["metrics"]["mission"]["phases_total"], 1);
    assert_eq!(first["metrics"]["phases"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        journal_records(&output)?
            .iter()
            .filter(|record| record.record.kind == "mission_terminal")
            .count(),
        1
    );
    assert_eq!(resume(&scratch, &output)?.status.code(), Some(1));
    let repeated = status_report(&status(&scratch, &output)?)?;
    assert_eq!(repeated["metrics"], first["metrics"]);
    assert_eq!(lines(&scratch.path("provider-count"))?, ["provider"]);
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    Ok(())
}

#[test]
fn status_metrics_view_refuses_malformed_or_foreign_projection() -> TestResult {
    for corruption in ["mode", "foreign"] {
        let scratch = Scratch::new()?;
        let output = paused_durable_run(&scratch)?;
        let database = output.join(orchestrator_app::RUST_PILOT_METRICS_SNAPSHOT_FILE);
        match corruption {
            "mode" => fs::set_permissions(&database, fs::Permissions::from_mode(0o644))?,
            "foreign" => fs::write(&database, b"not a sqlite metrics store\n")?,
            _ => unreachable!(),
        }
        let refused = status(&scratch, &output)?;
        assert_eq!(refused.status.code(), Some(1), "{corruption}");
        assert!(refused.stdout.is_empty(), "{corruption}");
    }
    Ok(())
}

#[test]
fn resume_metrics_owner_refuses_malformed_or_foreign_database() -> TestResult {
    for corruption in ["mode", "foreign"] {
        let scratch = Scratch::new()?;
        let output = paused_durable_run(&scratch)?;
        let database = output.join("metrics.db");
        match corruption {
            "mode" => fs::set_permissions(&database, fs::Permissions::from_mode(0o644))?,
            "foreign" => fs::write(&database, b"not a sqlite metrics store\n")?,
            _ => unreachable!(),
        }
        let before = lines(&scratch.path("provider-count"))?;
        assert_eq!(
            resume(&scratch, &output)?.status.code(),
            Some(1),
            "{corruption}"
        );
        assert_eq!(
            lines(&scratch.path("provider-count"))?,
            before,
            "{corruption}"
        );
    }
    Ok(())
}

#[test]
fn status_metrics_projection_rejects_invalid_bindings_and_values_without_execution() -> TestResult {
    for corruption in [
        "mission",
        "schema",
        "negative",
        "duplicate",
        "gate",
        "unknown",
        "oversized",
        "symlink",
        "fifo",
    ] {
        let scratch = Scratch::new()?;
        let output = paused_durable_run(&scratch)?;
        let projection = output.join(orchestrator_app::RUST_PILOT_METRICS_SNAPSHOT_FILE);
        let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&projection)?)?;
        match corruption {
            "mission" => value["mission_id"] = serde_json::json!("another-mission"),
            "schema" => value["schema_version"] = serde_json::json!(99),
            "negative" => value["metrics"]["phases"][0]["tokens"]["input"] = serde_json::json!(-1),
            "duplicate" => {
                let phase = value["metrics"]["phases"][0].clone();
                value["metrics"]["phases"]
                    .as_array_mut()
                    .ok_or("phase rows")?
                    .push(phase);
            }
            "gate" => value["metrics"]["phases"][0]["status"] = serde_json::json!("failed"),
            "unknown" => value["unexpected"] = serde_json::json!(true),
            "oversized" => fs::OpenOptions::new()
                .write(true)
                .open(&projection)?
                .set_len(9 * 1024 * 1024)?,
            "symlink" => {
                let saved = scratch.path("saved-projection");
                fs::rename(&projection, &saved)?;
                symlink(&saved, &projection)?;
            }
            "fifo" => {
                fs::remove_file(&projection)?;
                let made = bounded_output(
                    Command::new("mkfifo").args(["-m", "600"]).arg(&projection),
                    Duration::from_secs(2),
                )?;
                assert!(made.status.success());
            }
            _ => unreachable!(),
        }
        if !matches!(corruption, "oversized" | "symlink" | "fifo") {
            fs::write(&projection, serde_json::to_vec(&value)?)?;
        }
        let before = lines(&scratch.path("provider-count"))?;
        let refused = status(&scratch, &output)?;
        assert_eq!(refused.status.code(), Some(1), "{corruption}");
        assert!(refused.stdout.is_empty(), "{corruption}");
        assert_eq!(
            lines(&scratch.path("provider-count"))?,
            before,
            "{corruption}"
        );
    }
    Ok(())
}

#[test]
fn admitted_phase_recovery_refuses_a_changed_workspace_before_further_execution() -> TestResult {
    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let output = scratch.path("out");
    let ready = scratch.path("metrics-ready");
    let mut command = pilot(&scratch);
    command
        .env("NANIKA_PILOT_METRICS_TEST_BARRIER", "admitted")
        .env("NANIKA_PILOT_METRICS_TEST_READY", &ready);
    run_args(&mut command, &repo, &mission, &output, &provider, &verifier);
    let child = command.spawn()?;
    wait_for(&ready)?;
    kill_and_wait(child)?;
    fs::write(
        output.join("workspace/phase-one.txt"),
        "changed after admission\n",
    )?;
    let refused = resume(&scratch, &output)?;
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("workspace changed after"));
    assert_eq!(lines(&scratch.path("provider-count"))?, ["code-one"]);
    assert!(lines(&scratch.path("verifier-count"))?.is_empty());
    Ok(())
}

#[test]
fn live_output_arrives_before_provider_exit() -> TestResult {
    use std::io::BufRead;
    use std::sync::mpsc;

    let scratch = Scratch::new()?;
    let repo = scratch.repo()?;
    let mission = scratch.file("mission.md", MISSION, false)?;
    let (provider, verifier, _) = fixture(
        &scratch,
        r#"{"schema":"nanika.rust-first-use-review.v1","verdict":"pass","blockers":[],"warnings":[]}"#,
        None,
        false,
    )?;
    let release = scratch.path("release-provider");
    let script = fs::read_to_string(&provider)?.replace(
        "input=$(cat)",
        &format!("input=$(cat)\nif [ ! -e '{}' ]; then printf '%s\\n' LIVE-OUTPUT-MARKER >&2; while [ ! -e '{}' ]; do sleep 0.05; done; fi", release.display(), release.display()),
    );
    fs::write(&provider, script)?;
    let output_dir = scratch.path("live-run");
    let mut command = pilot(&scratch);
    run_args(
        &mut command,
        &repo,
        &mission,
        &output_dir,
        &provider,
        &verifier,
    );
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stderr = child.stderr.take().ok_or("missing progress pipe")?;
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            if let Ok(event) = serde_json::from_str::<Value>(&line) {
                if event["kind"] == "process_output"
                    && event["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("LIVE-OUTPUT-MARKER"))
                {
                    let _ = sender.try_send(());
                }
            }
        }
    });
    let observed = receiver.recv_timeout(Duration::from_secs(15)).is_ok();
    let before_exit = child.try_wait()?.is_none();
    // Always release the fixture, including when the assertion will fail.
    fs::write(&release, "continue")?;
    let result = child.wait_with_output()?;
    reader.join().map_err(|_| "progress reader panicked")?;
    assert!(
        observed,
        "no live output was received before releasing the provider"
    );
    assert!(
        before_exit,
        "provider/mission finished before the fixture was released"
    );
    assert!(
        result.status.success(),
        "mission failed: {:?}",
        result.status
    );
    Ok(())
}
