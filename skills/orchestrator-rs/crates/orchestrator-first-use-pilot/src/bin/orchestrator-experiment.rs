//! Bounded, explicitly opted-in matched standalone Codex experiments.
#[path = "experiment_report/mod.rs"]
mod experiment_report;

use orchestrator_process::{ProcessReport, ProcessSpec, ProcessSupervisor};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const LIMIT: usize = 1024 * 1024;
const HELP: &str = "orchestrator-experiment --allow-provider-experiments --pilot ABS --provider ABS --repo CLEAN-ROOT --prompt-file FILE --output-dir FRESH --model NAME --persona NAME --pairs 2..10 [--timeout-secs 1..1800] [--verification-timeout-secs 1..1800] -- ABS-VERIFIER [ARG...]\nCache state is external/uncontrolled. Verification depends on the supplied argv. The process broker must be beside this executable.";
type Result<T> = std::result::Result<T, String>;

#[derive(Debug)]
struct Options {
    pilot: PathBuf,
    provider: PathBuf,
    repo: PathBuf,
    prompt: PathBuf,
    output: PathBuf,
    model: String,
    persona: String,
    pairs: usize,
    timeout: u64,
    verification_timeout: u64,
    verifier: Vec<OsString>,
}

fn parse(args: Vec<OsString>) -> Result<Option<Options>> {
    if args == [OsString::from("--help")] {
        return Ok(None);
    }
    let mut flags = BTreeMap::new();
    let mut opt_in = false;
    let mut verifier = Vec::new();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let key = arg.into_string().map_err(|_| "non-UTF-8 option")?;
        if key == "--" {
            verifier.extend(it);
            break;
        }
        if key == "--allow-provider-experiments" {
            if opt_in {
                return Err("duplicate opt-in".into());
            }
            opt_in = true;
            continue;
        }
        if ![
            "--pilot",
            "--provider",
            "--repo",
            "--prompt-file",
            "--output-dir",
            "--model",
            "--persona",
            "--pairs",
            "--timeout-secs",
            "--verification-timeout-secs",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("unknown option: {key}"));
        }
        let value = it.next().ok_or_else(|| format!("missing value: {key}"))?;
        let value = value.into_string().map_err(|_| "non-UTF-8 value")?;
        if value.is_empty() || flags.insert(key.clone(), value).is_some() {
            return Err(format!("empty or duplicate option: {key}"));
        }
    }
    if !opt_in {
        return Err("explicit --allow-provider-experiments required".into());
    }
    if verifier.is_empty() || verifier.iter().any(|v| v.to_str().is_none()) {
        return Err("UTF-8 verifier argv required after --".into());
    }
    let mut take = |key: &str| flags.remove(key).ok_or_else(|| format!("required {key}"));
    let pilot = PathBuf::from(take("--pilot")?);
    let provider = PathBuf::from(take("--provider")?);
    let repo = PathBuf::from(take("--repo")?);
    let prompt = PathBuf::from(take("--prompt-file")?);
    let output = PathBuf::from(take("--output-dir")?);
    let model = take("--model")?;
    let persona = take("--persona")?;
    let pairs = take("--pairs")?
        .parse::<usize>()
        .map_err(|_| "invalid pairs")?;
    if !(2..=10).contains(&pairs) {
        return Err("pairs must be 2..10".into());
    }
    let number = |key: &str, default: u64| -> Result<u64> {
        let value = flags
            .get(key)
            .map(|v| v.parse::<u64>())
            .transpose()
            .map_err(|_| format!("invalid {key}"))?
            .unwrap_or(default);
        if !(1..=1800).contains(&value) {
            return Err(format!("{key} must be 1..1800"));
        }
        Ok(value)
    };
    let timeout = number("--timeout-secs", 300)?;
    let verification_timeout = number("--verification-timeout-secs", 60)?;
    for path in [
        &pilot,
        &provider,
        &repo,
        &prompt,
        &output,
        &PathBuf::from(&verifier[0]),
    ] {
        if !path.is_absolute() {
            return Err(format!("absolute path required: {}", path.display()));
        }
    }
    Ok(Some(Options {
        pilot,
        provider,
        repo,
        prompt,
        output,
        model,
        persona,
        pairs,
        timeout,
        verification_timeout,
        verifier,
    }))
}

fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|e| format!("create {}: {e}", path.display()))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("create {}: {e}", path.display()))?;
    file.write_all(bytes).map_err(|e| e.to_string())
}

fn canonical(path: &Path, directory: bool) -> Result<PathBuf> {
    let metadata =
        fs::symlink_metadata(path).map_err(|e| format!("inspect {}: {e}", path.display()))?;
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(format!(
            "expected real {}: {}",
            if directory { "directory" } else { "file" },
            path.display()
        ));
    }
    let resolved = fs::canonicalize(path).map_err(|e| e.to_string())?;
    if resolved.to_str().is_none() {
        return Err("non-UTF-8 path".into());
    }
    Ok(resolved)
}

fn hash(path: &Path, executable: bool) -> Result<String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|e| format!("inspect {}: {e}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!("not regular file: {}", path.display()));
    }
    if executable
        && (metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.permissions().mode() & 0o022 != 0
            || metadata.permissions().mode() & 0o111 == 0)
    {
        return Err(format!(
            "executable must be owned and not group/other writable: {}",
            path.display()
        ));
    }
    if metadata.len() > 512 * 1024 * 1024 {
        return Err("pinned file too large".into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > 512 * 1024 * 1024 {
            return Err("pinned file grew beyond bound".into());
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn run(
    supervisor: &ProcessSupervisor,
    argv: Vec<OsString>,
    cwd: &Path,
    secs: u64,
) -> Result<ProcessReport> {
    let mut spec = ProcessSpec::new(argv, Duration::from_secs(secs))
        .map_err(|e| e.to_string())?
        .with_max_output_bytes(LIMIT);
    for key in ["HOME", "USER", "LOGNAME", "PATH", "TMPDIR"] {
        spec = spec.with_inherited_env(key);
    }
    spec = spec.with_env("NANIKA_RUST_FIRST_USE_PILOT", "1");
    supervisor.run(&spec, cwd).map_err(|e| e.to_string())
}

fn receipt(result: &Result<ProcessReport>) -> Value {
    match result {
        Err(error) => json!({"success":false,"launch_error":error}),
        Ok(report) => {
            json!({"success":report.is_success(),"elapsed_ms":report.elapsed.as_millis(),"termination":format!("{:?}",report.termination),"spawned":report.spawned,"cleanup_complete":report.cleanup_complete,"stdout_bytes":report.stdout.len(),"stderr_bytes":report.stderr.len(),"stdout_discarded_bytes":report.stdout_discarded_bytes,"stderr_discarded_bytes":report.stderr_discarded_bytes})
        }
    }
}

fn save_capture(dir: &Path, name: &str, result: &Result<ProcessReport>) -> Result<()> {
    if let Ok(report) = result {
        write_new(&dir.join(format!("{name}.stdout")), &report.stdout)?;
        write_new(&dir.join(format!("{name}.stderr")), &report.stderr)?;
    }
    Ok(())
}

fn git(supervisor: &ProcessSupervisor, repo: &Path, args: &[&str]) -> Result<String> {
    let mut argv = vec![OsString::from("/usr/bin/git")];
    argv.extend(args.iter().map(OsString::from));
    let report = run(supervisor, argv, repo, 15)?;
    if !report.is_success() {
        return Err(format!("git {args:?} failed: {:?}", report.termination));
    }
    String::from_utf8(report.stdout).map_err(|_| "non-UTF-8 git output".into())
}

fn source(supervisor: &ProcessSupervisor, repo: &Path) -> Result<Value> {
    let top = git(supervisor, repo, &["rev-parse", "--show-toplevel"])?;
    if Path::new(top.trim_end()) != repo {
        return Err("repo must be its Git top-level".into());
    }
    if !git(
        supervisor,
        repo,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("repository is not clean".into());
    }
    Ok(
        json!({"path":repo,"head":git(supervisor,repo,&["rev-parse","HEAD"])?.trim_end(),"tree":git(supervisor,repo,&["rev-parse","HEAD^{tree}"])?.trim_end()}),
    )
}

fn artifact(path: &Path) -> Value {
    let read = || -> Result<Value> {
        let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > LIMIT as u64
        {
            return Err("not a bounded regular artifact".into());
        }
        let mut bytes = Vec::new();
        OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)
            .map_err(|e| e.to_string())?
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > LIMIT {
            return Err("artifact grew beyond bound".into());
        }
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    };
    match read() {
        Ok(value) => json!({"path":path,"status":"available","value":value}),
        Err(error) => json!({"path":path,"status":"unavailable","reason":error}),
    }
}

fn check(
    supervisor: &ProcessSupervisor,
    options: &Options,
    identity: &Value,
    pins: &[(PathBuf, bool, String)],
) -> Result<()> {
    if source(supervisor, &options.repo)? != *identity {
        return Err("source identity drift".into());
    }
    for (path, executable, digest) in pins {
        if hash(path, *executable)? != *digest {
            return Err(format!("pinned input drift: {}", path.display()));
        }
    }
    Ok(())
}

fn persist(output: &Path, manifest: &Value, samples: &[Value], complete: bool) -> Result<()> {
    let report = json!({"schema":"nanika.portal-experiment.v1","complete":complete,"manifest":manifest,"samples":samples,"summary":experiment_report::summarize(samples),"savings_claim":false,"conclusions":"descriptive-only","cache_condition":"external/uncontrolled"});
    let tmp = output.join("report.json.next");
    write_new(
        &tmp,
        &serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )?;
    fs::rename(tmp, output.join("report.json")).map_err(|e| e.to_string())
}

fn sample(
    supervisor: &ProcessSupervisor,
    options: &Options,
    index: usize,
    pair: usize,
    arm: &str,
) -> Result<Value> {
    let dir = options
        .output
        .join(format!("sample-{:02}-{arm}", index + 1));
    private_dir(&dir)?;
    let run_dir = dir.join("run");
    let argv = vec![
        options.pilot.as_os_str().to_owned(),
        "code".into(),
        "--runtime".into(),
        "codex".into(),
        "--codex".into(),
        options.provider.as_os_str().to_owned(),
        "--repo".into(),
        options.repo.as_os_str().to_owned(),
        "--prompt-file".into(),
        options.output.join("prompt.md").into_os_string(),
        "--output-dir".into(),
        run_dir.as_os_str().to_owned(),
        "--model".into(),
        options.model.clone().into(),
        "--persona".into(),
        options.persona.clone().into(),
        "--timeout-secs".into(),
        options.timeout.to_string().into(),
        "--feature".into(),
        format!("portal-output-cap={arm}").into(),
    ];
    let started = Instant::now();
    let pilot = run(supervisor, argv, &options.repo, options.timeout + 30);
    save_capture(&dir, "pilot", &pilot)?;
    let result = artifact(&run_dir.join("pilot-result.json"));
    let features = artifact(&run_dir.join("run-features.json"));
    let usage = artifact(&run_dir.join("worker-usage.json"));
    let portal = artifact(&run_dir.join("portal-application.json"));
    let workspace = run_dir.join("workspace");
    let verifier = match canonical(&workspace, true) {
        Ok(path) if path == workspace => {
            let report = run(
                supervisor,
                options.verifier.clone(),
                &workspace,
                options.verification_timeout,
            );
            save_capture(&dir, "verification", &report)?;
            receipt(&report)
        }
        _ => json!({"success":false,"skipped":"real workspace unavailable"}),
    };
    let config_ok = features["value"]["runtime"] == "codex"
        && features["value"]["command"] == "code"
        && features["value"]["entries"]
            .as_array()
            .is_some_and(|entries| {
                let mut matching = entries
                    .iter()
                    .filter(|entry| entry["name"] == "portal-output-cap");
                let valid = matching.next().is_some_and(|entry| {
                    entry["requested"] == arm
                        && entry["effective"] == arm
                        && entry["source"] == "explicit-run-option"
                });
                valid && matching.next().is_none()
            });
    let applied = arm == "off"
        || (portal["value"]["status"] == "observed"
            && portal["value"]["applied"] == true
            && portal["value"]["verified_wrapped_calls"]
                .as_u64()
                .is_some_and(|n| n > 0));
    let pilot_receipt = receipt(&pilot);
    let quality = pilot_receipt["success"] == true
        && result["value"]["status"] == "completed"
        && config_ok
        && applied
        && verifier["success"] == true;
    let commands = portal["value"]["commands"].as_array();
    let failed_commands = commands.map(|items| {
        items
            .iter()
            .filter(|item| {
                item["command_exit_code"]
                    .as_i64()
                    .is_some_and(|code| code != 0)
            })
            .count()
    });
    Ok(
        json!({"index":index,"pair":pair,"arm":arm,"status":"sample_completed","quality_passed":quality,"quality_gates":{"pilot_process":pilot_receipt["success"],"pilot_completed":result["value"]["status"]=="completed","correct_configuration":config_ok,"observed_application":applied,"verification":verifier["success"]},"elapsed_ms":started.elapsed().as_millis(),"pilot_process":pilot_receipt,"verification":verifier,"route":result["value"]["route"],"provider_tool_failed":result["value"]["codex_protocol"]["tool_failed"],"portal_failed_command_count":failed_commands,"metrics":usage["value"]["report"]["summary"],"captured_provider_stdout_bytes":result["value"]["process"]["stdout_bytes"],"full_command_log_bytes":if arm=="on" && applied {portal["value"]["full_log_bytes"].clone()} else {Value::Null},"artifacts":{"pilot_result":result,"features":features,"usage":usage,"portal_application":portal}}),
    )
}

fn execute(mut options: Options) -> Result<()> {
    options.repo = canonical(&options.repo, true)?;
    options.pilot = canonical(&options.pilot, false)?;
    options.provider = canonical(&options.provider, false)?;
    options.prompt = canonical(&options.prompt, false)?;
    let mut prompt = Vec::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&options.prompt)
        .map_err(|e| e.to_string())?
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut prompt)
        .map_err(|e| e.to_string())?;
    if prompt.len() > LIMIT {
        return Err("prompt exceeds 1 MiB".into());
    }
    let prompt_digest = format!("{:x}", Sha256::digest(&prompt));
    options.verifier[0] = canonical(Path::new(&options.verifier[0]), false)?.into_os_string();
    if fs::symlink_metadata(&options.output).is_ok() {
        return Err("output already exists".into());
    }
    let parent = canonical(
        options.output.parent().ok_or("output parent required")?,
        true,
    )?;
    options.output = parent.join(options.output.file_name().ok_or("output name required")?);
    if options.output.starts_with(&options.repo) {
        return Err("output must be outside repository".into());
    }
    let pilot_parent = options.pilot.parent().ok_or("pilot parent required")?;
    let runner = canonical(&std::env::current_exe().map_err(|e| e.to_string())?, false)?;
    let runner_broker = runner
        .parent()
        .ok_or("runner parent")?
        .join("orchestrator-process-broker");
    let paths = vec![
        (options.pilot.clone(), true),
        (options.provider.clone(), true),
        (PathBuf::from(&options.verifier[0]), true),
        (pilot_parent.join("orchestrator-output"), true),
        (pilot_parent.join("orchestrator-process-broker"), true),
        (runner, true),
        (runner_broker, true),
        (options.prompt.clone(), false),
    ];
    let mut pins = Vec::new();
    for (path, executable) in paths {
        let digest = hash(&path, executable)?;
        if path == options.prompt && digest != prompt_digest {
            return Err("prompt changed while admitting".into());
        }
        pins.push((path, executable, digest));
    }
    let supervisor = ProcessSupervisor::process_wide().map_err(|e| e.to_string())?;
    let identity = source(&supervisor, &options.repo)?;
    let version = run(
        &supervisor,
        vec![options.provider.as_os_str().to_owned(), "--version".into()],
        &options.repo,
        15,
    )?;
    if !version.is_success() {
        return Err("provider version probe failed".into());
    }
    let version = String::from_utf8(version.stdout).map_err(|_| "non-UTF-8 provider version")?;
    let catalog = run(
        &supervisor,
        vec![options.pilot.as_os_str().to_owned(), "features".into()],
        &options.repo,
        15,
    )?;
    if !catalog.is_success() {
        return Err("pilot features probe failed".into());
    }
    let catalog: Value =
        serde_json::from_slice(&catalog.stdout).map_err(|e| format!("features JSON: {e}"))?;
    check(&supervisor, &options, &identity, &pins)?;
    private_dir(&options.output)?;
    write_new(&options.output.join("prompt.md"), &prompt)?;
    pins.push((
        options.output.join("prompt.md"),
        false,
        hash(&options.output.join("prompt.md"), false)?,
    ));
    let manifest = json!({"schema":"nanika.portal-experiment-manifest.v1","source":identity,"model":options.model,"persona":options.persona,"pairs":options.pairs,"sample_count":2*options.pairs,"pins":pins.iter().map(|(p,e,h)|json!({"path":p,"executable":e,"sha256":h})).collect::<Vec<_>>(),"provider_version":version.trim_end(),"pilot_version":null,"pilot_identity":"SHA256 plus features catalog","features_catalog":catalog,"runner_package_version":env!("CARGO_PKG_VERSION"),"verification_argv":options.verifier.iter().map(|v|v.to_str()).collect::<Vec<_>>(),"timeout_secs":options.timeout,"verification_timeout_secs":options.verification_timeout,"cache_condition":"external/uncontrolled","off_full_command_bytes":"unknown; provider capture may truncate commands","savings_claim":false});
    write_new(
        &options.output.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
    )?;
    let mut samples = Vec::new();
    for pair in 0..options.pairs {
        for arm in if pair % 2 == 0 {
            ["off", "on"]
        } else {
            ["on", "off"]
        } {
            samples.push(json!({"index":samples.len(),"pair":pair+1,"arm":arm,"status":"not-run","reason":"pending","quality_passed":false}));
        }
    }
    persist(&options.output, &manifest, &samples, false)?;
    for index in 0..samples.len() {
        if let Err(reason) = check(&supervisor, &options, &identity, &pins) {
            for sample in &mut samples[index..] {
                sample["reason"] = json!(&reason);
            }
            persist(&options.output, &manifest, &samples, true)?;
            return Ok(());
        }
        let arm = if samples[index]["arm"] == "on" {
            "on"
        } else {
            "off"
        };
        println!("sample {}/{}: {arm}", index + 1, samples.len());
        samples[index] = match sample(&supervisor, &options, index, index / 2 + 1, arm) {
            Ok(value) => value,
            Err(reason) => {
                json!({"index":index,"pair":index/2+1,"arm":arm,"status":"sample_failed","quality_passed":false,"reason":reason})
            }
        };
        if let Err(reason) = check(&supervisor, &options, &identity, &pins) {
            samples[index]["quality_passed"] = json!(false);
            samples[index]["identity_drift"] = json!(&reason);
            for sample in &mut samples[index + 1..] {
                sample["reason"] = json!(&reason);
            }
            persist(&options.output, &manifest, &samples, true)?;
            return Ok(());
        }
        persist(
            &options.output,
            &manifest,
            &samples,
            index + 1 == samples.len(),
        )?;
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    let result = parse(std::env::args_os().skip(1).collect()).and_then(|options| match options {
        None => {
            println!("{HELP}");
            Ok(())
        }
        Some(options) => execute(options),
    });
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("orchestrator-experiment: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
